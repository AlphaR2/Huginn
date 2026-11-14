//! `shred-repair`: fetch the shreds of a block (and its child) from validators, then hand
//! them to `ledger-dump --shreds` for the certificate and inclusion checks.

use {
    anyhow::{bail, ensure, Context, Result},
    clap::{Parser, Subcommand},
    shred_repair::{
        client::Client, fetch_block, fetch_shape, fetch_slot, footer_cert_slot, write_shred_file,
    },
    std::{net::SocketAddr, path::PathBuf},
};

#[derive(Parser)]
#[command(about = "Fetch Solana shreds with Agave's repair protocol (no node, no paid feed)")]
struct Cli {
    /// JSON-RPC URL, used to find a peer's repair address and identity (`getClusterNodes`)
    #[arg(long, env = "SOLANA_RPC", hide_env_values = true, default_value = "http://127.0.0.1:8899", global = true)]
    rpc: String,
    /// A peer's `serveRepair` address. With --peer-id, skips the RPC lookup.
    #[arg(long, global = true)]
    peer: Option<SocketAddr>,
    /// A peer's identity (base58 pubkey). Needed with --peer.
    #[arg(long, global = true)]
    peer_id: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// List peers that serve repair.
    Peers,
    /// Download a block's shreds and its child's, write shred files.
    Fetch {
        #[arg(long)]
        slot: u64,
        /// The block's certified id (hex). With it the block is fetched and checked by id;
        /// without it, by slot (the certificate check in `ledger-dump` then does the work).
        #[arg(long)]
        block_id: Option<String>,
        #[arg(long, default_value = "fixtures/repaired")]
        out: PathBuf,
    },
    /// Download a run of consecutive blocks and write their shred files, nothing more. Each
    /// block's footer carries a certificate for a recent block, so a run gives several
    /// certificates for the price of one walk (`ledger-dump --fix-set` reads them).
    Footers {
        #[arg(long)]
        from: u64,
        #[arg(long, default_value_t = 6)]
        count: u64,
        #[arg(long, default_value = "fixtures/repaired")]
        out: PathBuf,
    },
    /// Ask only for the block's shape (set count, parent, every set root) and verify it
    /// against the block id. Downloads no shred.
    Shape {
        #[arg(long)]
        slot: u64,
        #[arg(long)]
        block_id: String,
    },
}

struct Peer {
    addr: SocketAddr,
    id: [u8; 32],
}

fn peers(rpc: &str) -> Result<Vec<Peer>> {
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getClusterNodes"});
    let v: serde_json::Value = ureq::post(rpc)
        .send_json(body)
        .map_err(|e| anyhow::anyhow!("RPC request to {} failed: {}", redact::Host(rpc), redact::redact_text(&e.to_string())))?
        .into_json()?;
    let mut out = Vec::new();
    for n in v["result"].as_array().context("getClusterNodes result")? {
        let (Some(addr), Some(id)) = (n["serveRepair"].as_str(), n["pubkey"].as_str()) else {
            continue;
        };
        let id: [u8; 32] = bs58::decode(id)
            .into_vec()?
            .try_into()
            .map_err(|_| anyhow::anyhow!("bad pubkey"))?;
        out.push(Peer {
            addr: addr.parse()?,
            id,
        });
    }
    Ok(out)
}

fn hash(s: &str) -> Result<[u8; 32]> {
    hex::decode(s.trim_start_matches("0x"))?
        .try_into()
        .map_err(|_| anyhow::anyhow!("block id must be 32 bytes of hex"))
}

/// The peers to try, in order: the one named, or every node `getClusterNodes` lists.
fn candidates(cli: &Cli) -> Result<Vec<Peer>> {
    match (&cli.peer, &cli.peer_id) {
        (Some(addr), Some(id)) => Ok(vec![Peer {
            addr: *addr,
            id: bs58::decode(id)
                .into_vec()?
                .try_into()
                .map_err(|_| anyhow::anyhow!("bad --peer-id"))?,
        }]),
        (None, None) => {
            let all = peers(&cli.rpc)?;
            ensure!(!all.is_empty(), "no peer with a serveRepair address");
            Ok(all)
        }
        _ => bail!("--peer and --peer-id go together"),
    }
}

/// Slots with a block, from `from` to `to`, per the cluster's RPC.
fn produced(rpc: &str, from: u64, to: u64) -> Result<Vec<u64>> {
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getBlocks","params":[from, to, {"commitment":"confirmed"}]});
    let v: serde_json::Value = ureq::post(rpc)
        .send_json(body)
        .map_err(|e| anyhow::anyhow!("RPC request to {} failed: {}", redact::Host(rpc), redact::redact_text(&e.to_string())))?
        .into_json()?;
    Ok(v["result"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_u64()).collect())
        .unwrap_or_default())
}

/// How many peers to try before giving up. A silent peer costs about 9 seconds.
const MAX_PEERS: usize = 8;

/// Runs `job` against each candidate peer until one succeeds.
fn with_peer<T>(cli: &Cli, mut job: impl FnMut(&mut Client, &Peer) -> Result<T>) -> Result<T> {
    let mut last = None;
    for peer in candidates(cli)?.into_iter().take(MAX_PEERS) {
        let mut c = Client::new(peer.addr, peer.id)?;
        eprintln!(
            "asking {} ({}) as {}",
            peer.addr,
            bs58::encode(peer.id).into_string(),
            bs58::encode(c.identity()).into_string()
        );
        match job(&mut c, &peer) {
            Ok(v) => return Ok(v),
            Err(e) => {
                eprintln!("  that peer failed: {e:#}");
                last = Some(e);
            }
        }
    }
    Err(last.unwrap()).context(format!(
        "none of the first {MAX_PEERS} peers could serve this"
    ))
}

/// A block that was only just produced may not be complete on a peer yet: ask again.
fn fetch_slot_patiently(c: &mut Client, slot: u64) -> Result<Vec<Vec<u8>>> {
    let mut last = None;
    for attempt in 0..6 {
        match fetch_slot(c, slot) {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt < 5 {
                    eprintln!("  slot {slot} not complete there yet ({e:#}); retrying");
                    std::thread::sleep(std::time::Duration::from_secs(3));
                }
                last = Some(e);
            }
        }
    }
    Err(last.unwrap())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {}", redact::redact_text(&format!("{e:#}")));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match &cli.cmd {
        Cmd::Peers => {
            for p in peers(&cli.rpc)? {
                println!("{}  {}", p.addr, bs58::encode(p.id).into_string());
            }
        }
        Cmd::Footers { from, count, out } => {
            let served_by = with_peer(&cli, |c, peer| {
                let slots: Vec<u64> = produced(&cli.rpc, *from, from + count * 3)?
                    .into_iter()
                    .take(*count as usize)
                    .collect();
                ensure!(!slots.is_empty(), "no blocks from {from} on {}", redact::Host(&cli.rpc));
                for s in slots {
                    let shreds = fetch_slot_patiently(c, s)?;
                    let cert = match footer_cert_slot(&shreds) {
                        Some(x) => format!("its footer certifies slot {x}"),
                        None => "its footer carries no certificate".to_string(),
                    };
                    println!("slot {s}: {} data shreds ({cert})", shreds.len());
                    write_shred_file(out, s, &shreds)?;
                }
                Ok(format!(
                    "{} {}",
                    peer.addr,
                    bs58::encode(peer.id).into_string()
                ))
            })?;
            println!("served by {served_by}");
        }
        Cmd::Shape { slot, block_id } => {
            let id = hash(block_id)?;
            let s = with_peer(&cli, |c, _| fetch_shape(c, *slot, id))?;
            println!(
                "slot {slot}: {} FEC sets, parent slot {} ({}), every root proven against the block id",
                s.fec_set_count, s.parent_slot, hex::encode(s.parent_block_id)
            );
        }
        Cmd::Fetch {
            slot,
            block_id,
            out,
        } => {
            let (child, served_by) = with_peer(&cli, |c, peer| {
                let shreds = match block_id {
                    Some(id) => {
                        let (shape, shreds) = fetch_block(c, *slot, hash(id)?)?;
                        println!(
                            "slot {slot}: {} data shreds, {} sets, all match the block id",
                            shreds.len(),
                            shape.fec_set_count
                        );
                        shreds
                    }
                    None => {
                        let shreds = fetch_slot_patiently(c, *slot)?;
                        println!(
                            "slot {slot}: {} data shreds (complete, block id not yet known)",
                            shreds.len()
                        );
                        shreds
                    }
                };
                println!("wrote {}", write_shred_file(out, *slot, &shreds)?.display());
                // The certificate for this block is in the footer of some later block: usually
                // the next, under load a few later. Walk forward until one carries it.
                let mut next = *slot + 1;
                let mut waited = 0;
                let child = loop {
                    let Some(&s) = produced(&cli.rpc, next, next + 12)?.first() else {
                        waited += 1;
                        ensure!(
                            waited < 30,
                            "no later block appeared on {} within a minute",
                            redact::Host(&cli.rpc)
                        );
                        std::thread::sleep(std::time::Duration::from_secs(2));
                        continue;
                    };
                    let shreds =
                        fetch_slot_patiently(c, s).with_context(|| format!("later slot {s}"))?;
                    let found = footer_cert_slot(&shreds) == Some(*slot);
                    println!(
                        "slot {s}: {} data shreds ({})",
                        shreds.len(),
                        if found {
                            format!("its footer carries the certificate for {slot}")
                        } else {
                            format!("its footer has no certificate for {slot}; continuing")
                        }
                    );
                    println!("wrote {}", write_shred_file(out, s, &shreds)?.display());
                    if found {
                        break shreds;
                    }
                    next = s + 1;
                    ensure!(
                        next <= *slot + 24,
                        "no footer within 24 blocks of {slot} carries its certificate"
                    );
                };
                Ok((
                    child,
                    format!("{} {}", peer.addr, bs58::encode(peer.id).into_string()),
                ))
            })?;
            ensure!(!child.is_empty(), "empty child");
            println!("served by {served_by}");
            println!("next: ledger-dump --shreds {} --slot {slot}", out.display());
        }
    }
    Ok(())
}
