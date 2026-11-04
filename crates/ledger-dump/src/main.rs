//! Writes a self-contained block fixture for a recent slot of a local Agave ledger.
//!
//! Opens `<ledger>/rocksdb` read-only, so it's safe while the validator is running. The
//! `data_shred` and `code_shred` columns are keyed by `slot BE u64 | index BE u64` and hold
//! the raw shred bytes (Agave v4.4.0 `ledger/src/blockstore/column.rs`).
//!
//! The fixture holds everything a verifier needs, so a test can check the whole chain
//! offline: the epoch's validator set and shred version, the footer certificates for the
//! slot and its parent, and the slot's data shreds.
//!
//! Certificates come from the shreds themselves by default (`--certs shreds`): block S's
//! footer carries the certificate that finalized its parent, so S's own certificate is in
//! its child's footer. `--certs plugin` reads them from the footer-dump plugin instead.
//! Nothing is written unless the certificates verify and the shreds rebuild the
//! certified block_id.

use {
    alpenglow_core::block::{self, Component, Marker},
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
    /// Where certificates come from: `shreds` (the next block's footer, in the ledger) or
    /// `plugin` (footer-dump's output, `--footers`).
    #[arg(long, default_value = "shreds")]
    certs: String,
    /// footer-dump plugin output, for `--certs plugin`.
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

    let from_shreds = match cli.certs.as_str() {
        "shreds" => true,
        "plugin" => false,
        other => bail!("--certs is shreds or plugin, not {other}"),
    };
    let raw = if from_shreds { Vec::new() } else { raw_certs(&cli.footers)? };
    let certs = raw.iter().map(|v| serde_json::from_value(v.clone())).collect::<Result<Vec<FooterCert>, _>>()?;
    let slot = match cli.slot {
        Some(s) => s,
        // Newest certified block that is still in the ledger.
        None if from_shreds => newest_certified_from_shreds(&db, first, last)?,
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

    let (raw_cert, raw_parent_cert) = if from_shreds {
        // The parent's certificate is in this block's footer; ours is in our child's.
        let parent = footer_cert(&data)?.filter(|c| c["slot"] == parent_slot).with_context(|| format!("slot {slot}'s footer has no cert for its parent {parent_slot}"))?;
        let own = child_cert(&db, slot, last)?;
        (own, parent)
    } else {
        let find = |s: u64| certs.iter().position(|c| c.slot == s).with_context(|| format!("no footer cert for slot {s}"));
        (raw[find(slot)?].clone(), raw[find(parent_slot)?].clone())
    };
    let cert: FooterCert = serde_json::from_value(raw_cert.clone())?;
    let parent_cert: FooterCert = serde_json::from_value(raw_parent_cert.clone())?;
    let (cert, parent_cert) = (&cert, &parent_cert);
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
    println!("  certs for {slot} and {parent_slot} verify (shred version {shred_version}, from {})", if from_shreds { "the shreds' footers" } else { "the plugin" });
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
        "cert_source": cli.certs,
        "block_final_cert": raw_cert,
        "parent_block_final_cert": raw_parent_cert,
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

/// The finalization certificate in a block's footer, in the plugin's JSON shape (so both
/// sources feed the same checks and the same fixture format). `None` if the block has no
/// footer yet or the footer carries no certificate.
fn footer_cert(data: &[Vec<u8>]) -> Result<Option<Value>> {
    let shreds = data.iter().map(|b| Shred::parse(b)).collect::<Result<Vec<_>, _>>()?;
    let Ok(components) = block::deshred(&shreds) else { return Ok(None) };
    for c in &components {
        if let Component::Marker(Marker::Footer(bytes)) = block::parse_component(c)? {
            let Some(cert) = block::parse_footer(bytes)?.final_cert else { return Ok(None) };
            let agg = |a: &alpenglow_core::proof::Aggregate| json!({ "signature": hex::encode(&a.signature), "bitmap": hex::encode(&a.bitmap) });
            return Ok(Some(json!({
                "slot": cert.slot,
                "block_id": hex::encode(cert.block_id),
                "final_aggregate": agg(&cert.final_aggregate),
                "notar_aggregate": cert.notar_aggregate.as_ref().map(agg),
            })));
        }
    }
    Ok(None)
}

/// The certificate for `slot`, from the footer of the first later block that names it as
/// parent. Usually slot + 1; skipped slots push it further.
fn child_cert(db: &DB, slot: u64, last: u64) -> Result<Value> {
    for child in slot + 1..=last.min(slot + 64) {
        let data = read_slot(db, "data_shred", child)?;
        let Some(first) = data.first() else { continue };
        if Shred::parse(first)?.parent_slot() != Some(slot) {
            continue;
        }
        return footer_cert(&data)?
            .filter(|c| c["slot"] == slot)
            .with_context(|| format!("child {child}'s footer is not written yet or has no cert for {slot}"));
    }
    bail!("no child block of {slot} in the ledger yet: it isn't finalized, or wait a slot")
}

/// The parent of the newest block whose footer is in the ledger: the newest slot with a
/// certificate we can read.
fn newest_certified_from_shreds(db: &DB, first: u64, last: u64) -> Result<u64> {
    for s in (first..=last).rev().take(64) {
        let data = read_slot(db, "data_shred", s)?;
        if let Some(c) = footer_cert(&data)? {
            return c["slot"].as_u64().context("slot");
        }
    }
    bail!("no footer with a certificate in the last 64 slots")
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
