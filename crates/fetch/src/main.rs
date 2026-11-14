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
    /// RPC URL. Prefer the `SOLANA_RPC` environment variable: URLs can carry API keys, and a
    /// command-line argument is visible to every user via `ps`. Defaults to the public endpoint.
    #[arg(long, env = "SOLANA_RPC", hide_env_values = true, global = true)]
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
    /// Save the cluster's current shred version (part of every signed vote payload).
    ClusterInfo {
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
    /// `solana-test-validator --alpenglow` on this machine.
    Local,
}

impl Cluster {
    fn name(self) -> &'static str {
        match self {
            Cluster::Mainnet => "mainnet",
            Cluster::Devnet => "devnet",
            Cluster::Local => "local",
        }
    }
    fn default_rpc(self) -> &'static str {
        match self {
            Cluster::Mainnet => "https://api.mainnet-beta.solana.com",
            Cluster::Devnet => "https://api.devnet.solana.com",
            Cluster::Local => "http://127.0.0.1:8899",
        }
    }
}

struct Rpc(String);

impl Rpc {
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        // ureq's error text contains the full URL (and so any API key). Build our own error
        // from the host only.
        let resp: Value = ureq::post(&self.0)
            .send_json(body)
            .map_err(|e| anyhow::anyhow!("{method} request to {} failed: {}", redact::Host(&self.0), redact::redact_text(&e.to_string())))?
            .into_json()?;
        if let Some(err) = resp.get("error") {
            bail!("{method} returned error: {err}");
        }
        resp.get("result").cloned().context("missing result")
    }
}

fn main() {
    if let Err(e) = run() {
        // Every error is scrubbed: no RPC URL or key may reach the terminal or a log.
        eprintln!("error: {}", redact::redact_text(&format!("{e:#}")));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::GenesisCert { cluster } => {
            let rpc = Rpc(cli.rpc.unwrap_or_else(|| cluster.default_rpc().into()));
            let result = rpc.call("getAgGenesisCert", json!([]))?;
            if result.is_null() {
                bail!("{} has no Alpenglow genesis certificate", cluster.name());
            }
            // A cluster that starts on Alpenglow at genesis (e.g. the local test validator)
            // never migrated, so it returns an all-zero placeholder instead of a real cert.
            let sig = result["signature"]["signature"]
                .as_array()
                .context("signature")?;
            if sig.iter().all(|b| b.as_u64() == Some(0)) {
                bail!("{} started on Alpenglow at genesis: the genesis cert is an all-zero placeholder", cluster.name());
            }
            let path = write(
                &cli.out,
                cluster,
                "genesis_cert.json",
                &json!({ "result": result }),
            )?;
            println!("slot {} -> {}", result["block"]["slot"], path.display());
        }
        Cmd::ClusterInfo { cluster } => {
            let rpc = Rpc(cli.rpc.unwrap_or_else(|| cluster.default_rpc().into()));
            cluster_info(&rpc, &cli.out, cluster)?;
        }
        Cmd::ValidatorSet { cluster } => {
            let rpc = Rpc(cli.rpc.unwrap_or_else(|| cluster.default_rpc().into()));
            validator_set(&rpc, &cli.out, cluster)?;
        }
    }
    Ok(())
}

/// The epoch's members (`epochVoteAccount == true`) with their LIVE stake, from a
/// `getVoteAccounts` result fetched with `keepUnstakedDelinquents: true`.
///
/// Fails closed if any member's live stake is 0. Such a validator (typically offline) is still
/// a member: its frozen epoch stake is not 0, but public RPC does not tell us what it is. Using
/// 0 would drop it from the ranked list and shift every rank below it, so the set would be
/// wrong. Only the frozen stake (the recorder, quest Q4) can fix that.
fn members(va: &Value) -> Result<Vec<(String, u64)>> {
    let mut stakes = Vec::new();
    let mut unknown = Vec::new();
    for list in ["current", "delinquent"] {
        for v in va[list].as_array().context(list)? {
            if v["epochVoteAccount"].as_bool() != Some(true) {
                continue;
            }
            let pk = v["votePubkey"].as_str().context("votePubkey")?.to_string();
            let stake = v["activatedStake"].as_u64().context("activatedStake")?;
            if stake == 0 {
                unknown.push(pk.clone());
            }
            stakes.push((pk, stake));
        }
    }
    if !unknown.is_empty() {
        bail!(
            "{} member(s) of this epoch show 0 live stake ({}): they are in the epoch's frozen set, \
             but RPC does not give their frozen stake, so this set would be wrong. Not writing it.",
            unknown.len(),
            unknown.join(", ")
        );
    }
    Ok(stakes)
}

fn validator_set(rpc: &Rpc, out: &std::path::Path, cluster: Cluster) -> Result<()> {
    // Read the slot first and pin every later call to it with minContextSlot, so a
    // set is never stitched together across an epoch boundary without us noticing.
    let info = rpc.call("getEpochInfo", json!([{"commitment": "finalized"}]))?;
    let epoch = info["epoch"].as_u64().context("epoch")?;
    let slot = info["absoluteSlot"].as_u64().context("slot")?;

    // `activatedStake` here is LIVE stake (it moves during the epoch). It is NOT the frozen
    // epoch stake that certificates are weighted by (Agave's VAT-filtered `epoch_stakes`).
    // `epochVoteAccount == true` marks exactly the validators in that frozen set, so we keep
    // only those; their stakes are still unverified until `ledger-dump check-set` passes.
    let va = rpc.call(
        "getVoteAccounts",
        json!([{"commitment": "finalized", "minContextSlot": slot, "keepUnstakedDelinquents": true}]),
    )?;
    let stakes = members(&va)?;

    let mut candidates = Vec::with_capacity(stakes.len());
    let (mut v4, mut with_key) = (0usize, 0usize);
    for chunk in stakes.chunks(100) {
        let keys: Vec<&str> = chunk.iter().map(|(k, _)| k.as_str()).collect();
        let accts = rpc.call(
            "getMultipleAccounts",
            json!([keys, {"encoding": "base64", "commitment": "finalized", "minContextSlot": slot}]),
        )?;
        for ((pk, stake), acct) in chunk
            .iter()
            .zip(accts["value"].as_array().context("value")?)
        {
            let b64 = acct["data"][0]
                .as_str()
                .with_context(|| format!("no data for {pk}"))?;
            let data = base64::engine::general_purpose::STANDARD.decode(b64)?;
            let parsed =
                vote_account::parse(&data).with_context(|| format!("vote account {pk}"))?;
            v4 += (parsed.version == vote_account::VERSION_V4) as usize;
            with_key += parsed.bls_pubkey.is_some() as usize;
            candidates.push(Candidate {
                vote_pubkey: bs58::decode(pk)
                    .into_vec()?
                    .try_into()
                    .ok()
                    .context("vote pubkey")?,
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
        "source": "getVoteAccounts (epochVoteAccount == true only) + vote account data",
        "stake_label": "live (unverified)",
        "stats": {
            "vote_accounts": total,
            "v4_accounts": v4,
            "with_bls_key": with_key,
            "ranked": set.entries.len(),
        },
        // What a destination chain stores for this epoch (alpenglow_core::proof::validator_set_hash).
        "commitment": hex::encode(set.commitment()),
        "validator_set": set,
    });
    let path = write(
        out,
        cluster,
        &format!("validator_set_{epoch}.json"),
        &fixture,
    )?;
    println!(
        "epoch {epoch}: {total} vote accounts, {with_key} with BLS key, {} ranked, total stake {}, commitment {} -> {}",
        set.entries.len(),
        set.total_stake,
        hex::encode(set.commitment()),
        path.display()
    );
    Ok(())
}

fn cluster_info(rpc: &Rpc, out: &std::path::Path, cluster: Cluster) -> Result<()> {
    let info = rpc.call("getEpochInfo", json!([{"commitment": "finalized"}]))?;
    // Nodes report their own shred version. Take the most common one, and keep the full
    // tally so a split (e.g. mid-restart) is visible in the fixture.
    let nodes = rpc.call("getClusterNodes", json!([]))?;
    let mut tally = std::collections::BTreeMap::<u64, u64>::new();
    for n in nodes.as_array().context("nodes")? {
        if let Some(v) = n["shredVersion"].as_u64() {
            *tally.entry(v).or_default() += 1;
        }
    }
    let (&shred_version, _) = tally.iter().max_by_key(|(_, c)| **c).context("no nodes")?;
    let fixture = json!({
        "cluster": cluster.name(),
        "epoch": info["epoch"],
        "slot": info["absoluteSlot"],
        "shred_version": shred_version,
        "shred_version_tally": tally,
    });
    let path = write(out, cluster, "cluster_info.json", &fixture)?;
    println!(
        "shred_version {shred_version} (tally {tally:?}) -> {}",
        path.display()
    );
    Ok(())
}

fn write(root: &std::path::Path, cluster: Cluster, file: &str, v: &Value) -> Result<PathBuf> {
    let dir = root.join(cluster.name());
    fs::create_dir_all(&dir)?;
    let path = dir.join(file);
    fs::write(&path, serde_json::to_string_pretty(v)? + "\n")?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_only_epoch_members_including_delinquent_ones() {
        let va = json!({
            "current": [
                {"votePubkey": "A", "activatedStake": 5, "epochVoteAccount": true},
                {"votePubkey": "B", "activatedStake": 9, "epochVoteAccount": false}
            ],
            "delinquent": [{"votePubkey": "C", "activatedStake": 3, "epochVoteAccount": true}]
        });
        assert_eq!(members(&va).unwrap(), vec![("A".into(), 5), ("C".into(), 3)]);
    }

    #[test]
    fn a_member_with_zero_live_stake_is_refused_not_dropped() {
        // Mainnet epoch 1084 had exactly this: an offline member with live stake 0.
        let va = json!({
            "current": [{"votePubkey": "A", "activatedStake": 5, "epochVoteAccount": true}],
            "delinquent": [{"votePubkey": "57GUg9", "activatedStake": 0, "epochVoteAccount": true}]
        });
        let e = members(&va).unwrap_err().to_string();
        assert!(e.contains("57GUg9") && e.contains("Not writing"), "{e}");
    }
}
