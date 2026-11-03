//! Dumps one slot's raw shreds from a local Agave ledger into a block fixture.
//!
//! Opens `<ledger>/rocksdb` read-only, so it's safe while the validator is running. The
//! `data_shred` and `code_shred` columns are keyed by `slot BE u64 | index BE u64` and hold
//! the raw shred bytes (Agave v4.4.0 `ledger/src/blockstore/column.rs`).
//!
//! The fixture also embeds the footer certificates for the slot and its parent (from the
//! footer-dump plugin's output), so a test can check the whole chain offline:
//! shreds -> FEC roots -> double Merkle root == certified block_id.

use {
    alpenglow_lc::shred::{self, Kind, Shred},
    anyhow::{bail, Context, Result},
    clap::Parser,
    rocksdb::{IteratorMode, Options, DB},
    serde_json::{json, Value},
    std::{fs, path::PathBuf},
};

#[derive(Parser)]
#[command(about = "Dump one slot's raw shreds from a local Agave ledger")]
struct Cli {
    /// Ledger directory (the one passed to the validator with --ledger).
    #[arg(long, default_value = ".local-ledger")]
    ledger: PathBuf,
    /// Slot to dump. Without it, prints the slot range the ledger holds.
    #[arg(long)]
    slot: Option<u64>,
    /// footer-dump plugin output, used to find the certified block_ids.
    #[arg(long, default_value = "fixtures/local/footers.jsonl")]
    footers: PathBuf,
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

    let Some(slot) = cli.slot else {
        let cf = db.cf_handle("data_shred").context("data_shred")?;
        let slot_of = |k: &[u8]| u64::from_be_bytes(k[..8].try_into().unwrap());
        let first = db.iterator_cf(cf, IteratorMode::Start).next().transpose()?.map(|(k, _)| slot_of(&k));
        let last = db.iterator_cf(cf, IteratorMode::End).next().transpose()?.map(|(k, _)| slot_of(&k));
        println!("data shreds for slots {first:?} ..= {last:?}");
        return Ok(());
    };

    let data = read_slot(&db, "data_shred", slot)?;
    let code = read_slot(&db, "code_shred", slot)?;
    if data.is_empty() {
        bail!("no data shreds for slot {slot}");
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

    let certs = footer_certs(&cli.footers)?;
    let cert = certs.iter().find(|c| c["slot"] == slot).with_context(|| format!("no footer cert for slot {slot}"))?;
    let parent_cert = certs
        .iter()
        .find(|c| c["slot"] == parent_slot)
        .with_context(|| format!("no footer cert for parent slot {parent_slot}"))?;
    let hash = |c: &Value| -> Result<shred::Hash> {
        let v = hex::decode(c["block_id"].as_str().context("block_id")?)?;
        v.try_into().ok().context("block_id length")
    };
    let (certified, parent_id) = (hash(cert)?, hash(parent_cert)?);

    let computed = shred::block_id(&roots, parent_slot, &parent_id)?;
    println!(
        "slot {slot}: {} data + {} coding shreds, {} FEC set(s), parent {parent_slot}",
        data.len(),
        code.len(),
        roots.len()
    );
    println!("  computed block_id  {}", hex::encode(computed));
    println!("  certified block_id {}", hex::encode(certified));
    println!(
        "  first FEC set chains to {} (parent block_id? {})",
        hex::encode(shreds[0].chained_merkle_root()),
        shreds[0].chained_merkle_root() == parent_id
    );
    println!("  last FEC set root  {}", hex::encode(roots.last().unwrap()));
    if computed != certified {
        bail!("block_id mismatch: not writing a fixture");
    }

    let fixture = json!({
        "cluster": "local",
        "slot": slot,
        "parent_slot": parent_slot,
        "block_final_cert": cert,
        "parent_block_final_cert": parent_cert,
        // Coding shreds aren't needed to rebuild the block_id (checked above, not stored).
        "data_shreds": data.iter().map(hex::encode).collect::<Vec<_>>(),
    });
    fs::create_dir_all(&cli.out)?;
    let out = cli.out.join(format!("block_{slot}.json"));
    fs::write(&out, serde_json::to_string_pretty(&fixture)? + "\n")?;
    println!("MATCH -> {}", out.display());
    Ok(())
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

/// Every non-null `block_final_cert` in the plugin's output.
fn footer_certs(path: &PathBuf) -> Result<Vec<Value>> {
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
