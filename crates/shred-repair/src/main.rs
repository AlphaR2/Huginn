//! `shred-repair`: fetch the shreds of a block (and its child) from validators, then hand
//! them to `ledger-dump --shreds` for the certificate and inclusion checks.

use {
    anyhow::{bail, ensure, Context, Result},
    clap::{Parser, Subcommand},
    shred_repair::{client::Client, fetch_block, fetch_shape, fetch_slot, write_shred_file},
    std::{net::SocketAddr, path::PathBuf},
};

#[derive(Parser)]
#[command(about = "Fetch Solana shreds with Agave's repair protocol (no node, no paid feed)")]
struct Cli {
    /// JSON-RPC URL, used to find a peer's repair address and identity (`getClusterNodes`)
    #[arg(long, default_value = "http://127.0.0.1:8899", global = true)]
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
    let v: serde_json::Value = ureq::post(rpc).send_json(body)?.into_json()?;
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

fn client(cli: &Cli) -> Result<Client> {
    let peer = match (&cli.peer, &cli.peer_id) {
        (Some(addr), Some(id)) => Peer {
            addr: *addr,
            id: bs58::decode(id)
                .into_vec()?
                .try_into()
                .map_err(|_| anyhow::anyhow!("bad --peer-id"))?,
        },
        (None, None) => peers(&cli.rpc)?
            .into_iter()
            .next()
            .context("no peer with a serveRepair address")?,
        _ => bail!("--peer and --peer-id go together"),
    };
    let c = Client::new(peer.addr, peer.id)?;
    eprintln!(
        "asking {} as {}",
        peer.addr,
        bs58::encode(c.identity()).into_string()
    );
    Ok(c)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.cmd {
        Cmd::Peers => {
            for p in peers(&cli.rpc)? {
                println!("{}  {}", p.addr, bs58::encode(p.id).into_string());
            }
        }
        Cmd::Shape { slot, block_id } => {
            let mut c = client(&cli)?;
            let s = fetch_shape(&mut c, *slot, hash(block_id)?)?;
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
            let mut c = client(&cli)?;
            let shreds = match block_id {
                Some(id) => {
                    let (shape, shreds) = fetch_block(&mut c, *slot, hash(id)?)?;
                    println!(
                        "slot {slot}: {} data shreds, {} sets, all match the block id",
                        shreds.len(),
                        shape.fec_set_count
                    );
                    shreds
                }
                None => {
                    let shreds = fetch_slot(&mut c, *slot)?;
                    println!(
                        "slot {slot}: {} data shreds (complete, block id not yet known)",
                        shreds.len()
                    );
                    shreds
                }
            };
            println!("wrote {}", write_shred_file(out, *slot, &shreds)?.display());
            let child = *slot + 1;
            let shreds =
                fetch_slot(&mut c, child).with_context(|| format!("child slot {child}"))?;
            println!(
                "slot {child}: {} data shreds (carries the certificate for {slot})",
                shreds.len()
            );
            println!("wrote {}", write_shred_file(out, child, &shreds)?.display());
            ensure!(!shreds.is_empty(), "empty child");
            println!("next: ledger-dump --shreds {} --slot {slot}", out.display());
        }
    }
    Ok(())
}
