//! Writes a self-contained block fixture for a recent slot of a local Agave ledger.
//!
//! Opens `<ledger>/rocksdb` read-only, so it's safe while the validator is running. The
//! `data_shred` and `code_shred` columns are keyed by `slot BE u64 | index BE u64` and hold
//! the raw shred bytes (Agave v4.4.0 `ledger/src/blockstore/column.rs`).
//!
//! The fixture holds everything a verifier needs, so a test can check the whole chain
//! offline: the epoch's validator set and shred version, the footer certificates for the
//! slot and its parent (from the footer-dump plugin), and the slot's data shreds.
//! Nothing is written unless the certificates verify and the shreds rebuild the
//! certified block_id.

use {
    alpenglow_lc::{
        cert::{verify_footer, FooterCert},
        shred::{self, Kind, Shred},
        validator_set::ValidatorSet,
    },
    anyhow::{bail, Context, Result},
    clap::Parser,
    rocksdb::{IteratorMode, Options, DB},
    serde_json::{json, Value},
    std::{
        fs,
        path::{Path, PathBuf},
    },
};

#[derive(Parser)]
#[command(about = "Write a block fixture (set, certs, shreds) for a recent local slot")]
struct Cli {
    /// Ledger directory (the one passed to the validator with --ledger).
    #[arg(long, default_value = ".local-ledger")]
    ledger: PathBuf,
    /// Slot to dump. Defaults to the most recent slot with a footer cert and shreds.
    #[arg(long)]
    slot: Option<u64>,
    /// Only print the slot range the ledger still holds.
    #[arg(long)]
    range: bool,
    /// footer-dump plugin output, used to find the certificates.
    #[arg(long, default_value = "fixtures/local/footers.jsonl")]
    footers: PathBuf,
    /// Where `cluster_info.json` and `validator_set_<epoch>.json` live (from `fetch`).
    #[arg(long, default_value = "fixtures/local")]
    sets: PathBuf,
    /// Epoch length. `solana-test-validator` uses 432,000 with no warmup.
    #[arg(long, default_value_t = 432_000)]
    slots_per_epoch: u64,
    /// Directory for `block_<slot>.json`.
    #[arg(long, default_value = "fixtures/local")]
    out: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = cli.ledger.join("rocksdb");
    // Read-only may open a subset of column families; we only need the two shred columns.
    let db = DB::open_cf_for_read_only(&Options::default(), &path, ["data_shred", "code_shred"], false)
        .with_context(|| format!("open {}", path.display()))?;

    let (first, last) = ledger_range(&db)?;
    if cli.range {
        println!("data shreds for slots {first} ..= {last}");
        return Ok(());
    }

    let raw = raw_certs(&cli.footers)?;
    let certs = raw.iter().map(|v| serde_json::from_value(v.clone())).collect::<Result<Vec<FooterCert>, _>>()?;
    let slot = match cli.slot {
        Some(s) => s,
        // Newest certified block that is still in the ledger.
        None => certs
            .iter()
            .rev()
            .map(|c| c.slot)
            .find(|s| (first..=last).contains(s))
            .context("no certified slot in the ledger's range")?,
    };

    let data = read_slot(&db, "data_shred", slot)?;
    let code = read_slot(&db, "code_shred", slot)?;
    if data.is_empty() {
        bail!("no data shreds for slot {slot} (ledger holds {first} ..= {last})");
    }
    let shreds = data.iter().map(|b| Shred::parse(b)).collect::<Result<Vec<_>, _>>()?;
    for b in &code {
        let s = Shred::parse(b)?;
        if s.kind() != Kind::Code || s.merkle_root()? != shreds[s.fec_set_index() as usize].merkle_root()? {
            bail!("coding shred {} disagrees with its FEC set root", s.index());
        }
    }
    let roots = shred::fec_roots(slot, &shreds)?;
    let parent_slot = shreds[0].parent_slot().context("parent slot")?;

    // The shred version every vote signs: the cluster's, and the one in the shreds.
    let cluster: Value = read_json(&cli.sets.join("cluster_info.json"))?;
    let shred_version = cluster["shred_version"].as_u64().context("shred_version")? as u16;
    if let Some(s) = shreds.iter().find(|s| s.version() != shred_version) {
        bail!("shred {} has version {}, cluster has {shred_version}", s.index(), s.version());
    }

    // The validator set of the slot's epoch. RPC only serves the current epoch's stakes,
    // so the set must have been fetched during that epoch.
    let epoch = slot / cli.slots_per_epoch;
    let set_path = cli.sets.join(format!("validator_set_{epoch}.json"));
    let set_json: Value = read_json(&set_path)
        .with_context(|| format!("no set for epoch {epoch}: run `fetch validator-set --cluster local` during it"))?;
    let set: ValidatorSet = serde_json::from_value(set_json["validator_set"].clone())?;
    if set.epoch != epoch {
        bail!("{} holds epoch {}, slot {slot} is in epoch {epoch}", set_path.display(), set.epoch);
    }

    let find = |s: u64| certs.iter().position(|c| c.slot == s).with_context(|| format!("no footer cert for slot {s}"));
    let (i, pi) = (find(slot)?, find(parent_slot)?);
    let (cert, parent_cert) = (&certs[i], &certs[pi]);
    // A parent in an earlier epoch would need that epoch's set too; not handled yet.
    if parent_slot / cli.slots_per_epoch != epoch {
        bail!("parent {parent_slot} is in a different epoch than {slot}");
    }
    verify_footer(cert, shred_version, &set).with_context(|| format!("cert for {slot}"))?;
    verify_footer(parent_cert, shred_version, &set).with_context(|| format!("cert for {parent_slot}"))?;

    let computed = shred::block_id(&roots, parent_slot, &parent_cert.block_id)?;
    println!(
        "slot {slot} (epoch {epoch}): {} data + {} coding shreds, {} FEC set(s), parent {parent_slot}",
        data.len(),
        code.len(),
        roots.len()
    );
    println!("  certs for {slot} and {parent_slot} verify (shred version {shred_version})");
    println!("  computed block_id  {}", hex::encode(computed));
    println!("  certified block_id {}", hex::encode(cert.block_id));
    if computed != cert.block_id {
        bail!("block_id mismatch: not writing a fixture");
    }

    let fixture = json!({
        "cluster": "local",
        "slot": slot,
        "parent_slot": parent_slot,
        "epoch": epoch,
        "shred_version": shred_version,
        "validator_set": set_json["validator_set"],
        "block_final_cert": raw[i],
        "parent_block_final_cert": raw[pi],
        // Coding shreds aren't needed to rebuild the block_id (checked above, not stored).
        "data_shreds": data.iter().map(hex::encode).collect::<Vec<_>>(),
    });
    fs::create_dir_all(&cli.out)?;
    let out = cli.out.join(format!("block_{slot}.json"));
    fs::write(&out, serde_json::to_string_pretty(&fixture)? + "\n")?;
    println!("MATCH -> {}", out.display());
    Ok(())
}

fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_str(&fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?)?)
}

fn ledger_range(db: &DB) -> Result<(u64, u64)> {
    let cf = db.cf_handle("data_shred").context("data_shred")?;
    let slot_of = |k: &[u8]| u64::from_be_bytes(k[..8].try_into().unwrap());
    let first = db.iterator_cf(cf, IteratorMode::Start).next().transpose()?.context("empty ledger")?;
    let last = db.iterator_cf(cf, IteratorMode::End).next().transpose()?.context("empty ledger")?;
    Ok((slot_of(&first.0), slot_of(&last.0)))
}

fn read_slot(db: &DB, column: &str, slot: u64) -> Result<Vec<Vec<u8>>> {
    let cf = db.cf_handle(column).context(column.to_string())?;
    let start = [slot.to_be_bytes(), 0u64.to_be_bytes()].concat();
    let mut out = Vec::new();
    for kv in db.iterator_cf(cf, IteratorMode::From(&start, rocksdb::Direction::Forward)) {
        let (k, v) = kv?;
        if k[..8] != slot.to_be_bytes() {
            break;
        }
        out.push(v.to_vec());
    }
    Ok(out)
}

/// Every non-null `block_final_cert` in the plugin's output, as written.
fn raw_certs(path: &Path) -> Result<Vec<Value>> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    // The plugin appends while we read: skip a last line it hasn't finished writing.
    let complete = &text[..text.rfind('\n').map_or(0, |i| i + 1)];
    let mut certs = Vec::new();
    for line in complete.lines() {
        let v: Value = serde_json::from_str(line)?;
        if !v["block_final_cert"].is_null() {
            certs.push(v["block_final_cert"].clone());
        }
    }
    Ok(certs)
}
