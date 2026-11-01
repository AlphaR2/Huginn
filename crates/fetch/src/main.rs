//! Pulls Alpenglow data from Solana RPC and writes fixtures under `fixtures/<cluster>/`.

mod vote_account;

use {
    alpenglow_lc::validator_set::{Candidate, ValidatorSet},
    anyhow::{bail, Context, Result},
    base64::Engine,
    clap::{Parser, Subcommand, ValueEnum},
    serde_json::{json, Value},
    std::{fs, path::PathBuf},
};

#[derive(Parser)]
#[command(about = "Fetch Alpenglow fixtures from Solana RPC")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Override the RPC URL (defaults to the public endpoint for the cluster).
    #[arg(long, global = true)]
    rpc: Option<String>,
    /// Fixtures root.
    #[arg(long, global = true, default_value = "fixtures")]
    out: PathBuf,
}

#[derive(Subcommand)]
enum Cmd {
    /// Save the raw `getAgGenesisCert` response.
    GenesisCert {
        #[arg(long)]
        cluster: Cluster,
    },
    /// Build the ranked BLS validator set from current vote accounts.
    ValidatorSet {
        #[arg(long)]
        cluster: Cluster,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Cluster {
    Mainnet,
    Devnet,
}

impl Cluster {
    fn name(self) -> &'static str {
        match self {
            Cluster::Mainnet => "mainnet",
            Cluster::Devnet => "devnet",
        }
    }
    fn default_rpc(self) -> &'static str {
        match self {
            Cluster::Mainnet => "https://api.mainnet-beta.solana.com",
            Cluster::Devnet => "https://api.devnet.solana.com",
        }
    }
}

struct Rpc(String);

impl Rpc {
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        let resp: Value = ureq::post(&self.0)
            .send_json(body)
            .with_context(|| format!("{method} request failed"))?
            .into_json()?;
        if let Some(err) = resp.get("error") {
            bail!("{method} returned error: {err}");
        }
        resp.get("result").cloned().context("missing result")
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::GenesisCert { cluster } => {
            let rpc = Rpc(cli.rpc.unwrap_or_else(|| cluster.default_rpc().into()));
            let result = rpc.call("getAgGenesisCert", json!([]))?;
            if result.is_null() {
                bail!("{} has no Alpenglow genesis certificate", cluster.name());
            }
            let path = write(&cli.out, cluster, "genesis_cert.json", &json!({ "result": result }))?;
            println!("slot {} -> {}", result["block"]["slot"], path.display());
        }
        Cmd::ValidatorSet { cluster } => {
            let rpc = Rpc(cli.rpc.unwrap_or_else(|| cluster.default_rpc().into()));
            validator_set(&rpc, &cli.out, cluster)?;
        }
    }
    Ok(())
}

fn validator_set(rpc: &Rpc, out: &PathBuf, cluster: Cluster) -> Result<()> {
    // Read the slot first and pin every later call to it with minContextSlot, so a
    // set is never stitched together across an epoch boundary without us noticing.
    let info = rpc.call("getEpochInfo", json!([{"commitment": "finalized"}]))?;
    let epoch = info["epoch"].as_u64().context("epoch")?;
    let slot = info["absoluteSlot"].as_u64().context("slot")?;

    // `activatedStake` is the stake the bank uses for the current epoch.
    let va = rpc.call(
        "getVoteAccounts",
        json!([{"commitment": "finalized", "minContextSlot": slot, "keepUnstakedDelinquents": true}]),
    )?;
    let mut stakes = Vec::new();
    for list in ["current", "delinquent"] {
        for v in va[list].as_array().context(list)? {
            let pk = v["votePubkey"].as_str().context("votePubkey")?.to_string();
            stakes.push((pk, v["activatedStake"].as_u64().context("activatedStake")?));
        }
    }

    let mut candidates = Vec::with_capacity(stakes.len());
    let (mut v4, mut with_key) = (0usize, 0usize);
    for chunk in stakes.chunks(100) {
        let keys: Vec<&str> = chunk.iter().map(|(k, _)| k.as_str()).collect();
        let accts = rpc.call(
            "getMultipleAccounts",
            json!([keys, {"encoding": "base64", "commitment": "finalized", "minContextSlot": slot}]),
        )?;
        for ((pk, stake), acct) in chunk.iter().zip(accts["value"].as_array().context("value")?) {
            let b64 = acct["data"][0].as_str().with_context(|| format!("no data for {pk}"))?;
            let data = base64::engine::general_purpose::STANDARD.decode(b64)?;
            let parsed = vote_account::parse(&data).with_context(|| format!("vote account {pk}"))?;
            v4 += (parsed.version == vote_account::VERSION_V4) as usize;
            with_key += parsed.bls_pubkey.is_some() as usize;
            candidates.push(Candidate {
                vote_pubkey: bs58::decode(pk).into_vec()?.try_into().ok().context("vote pubkey")?,
                node_pubkey: parsed.node_pubkey,
                stake: *stake,
                bls_pubkey: parsed.bls_pubkey,
            });
        }
    }

    let total = candidates.len();
    let set = ValidatorSet::from_candidates(epoch, candidates);
    let fixture = json!({
        "cluster": cluster.name(),
        "epoch": epoch,
        "fetched_at_slot": slot,
        "source": "getVoteAccounts.activatedStake + vote account data (current epoch only)",
        "stats": {
            "vote_accounts": total,
            "v4_accounts": v4,
            "with_bls_key": with_key,
            "ranked": set.entries.len(),
        },
        "validator_set": set,
    });
    let path = write(out, cluster, &format!("validator_set_{epoch}.json"), &fixture)?;
    println!(
        "epoch {epoch}: {total} vote accounts, {with_key} with BLS key, {} ranked, total stake {} -> {}",
        set.entries.len(),
        set.total_stake,
        path.display()
    );
    Ok(())
}

fn write(root: &PathBuf, cluster: Cluster, file: &str, v: &Value) -> Result<PathBuf> {
    let dir = root.join(cluster.name());
    fs::create_dir_all(&dir)?;
    let path = dir.join(file);
    fs::write(&path, serde_json::to_string_pretty(v)? + "\n")?;
    Ok(path)
}
