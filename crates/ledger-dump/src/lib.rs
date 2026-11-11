//! Block fixtures from any **shred source**.
//!
//! Shreds are all Huginn needs: block S's shreds hold its transactions, and its child's
//! footer holds the certificate that finalized it (item 6.5). So a data source only has to
//! answer "the shreds of slot S" ([`ShredSource`]); building and checking the fixture is
//! the same for every source ([`build_fixture`]).
//!
//! Sources here:
//! - [`Ledger`]: an Agave ledger's RocksDB, read-only (your local validator, or any node's
//!   ledger on devnet or mainnet);
//! - [`ShredFiles`]: `shreds_<slot>.json` files written by [`export`] (offline, tests).
//!
//! A ShredStream receiver or a repair-protocol client would be one more `ShredSource`.

use {
    alpenglow_core::block::{self, Component, Marker},
    alpenglow_lc::{
        cert::{verify_footer, FooterCert},
        shred::{self, Kind, Shred},
        validator_set::ValidatorSet,
    },
    anyhow::{bail, Context, Result},
    rocksdb::{IteratorMode, Options, DB},
    serde_json::{json, Value},
    std::{
        fs,
        path::{Path, PathBuf},
    },
};

/// Where shreds come from.
pub trait ShredSource {
    /// For messages: "ledger .local-ledger", "files fixtures/local/shreds".
    fn describe(&self) -> String;
    /// First and last slot with data shreds.
    fn range(&self) -> Result<(u64, u64)>;
    /// Raw data shreds of `slot`, in index order. Empty if the source has none.
    fn data_shreds(&self, slot: u64) -> Result<Vec<Vec<u8>>>;
    /// Raw coding shreds of `slot` (only used for an extra consistency check; may be empty).
    fn code_shreds(&self, slot: u64) -> Result<Vec<Vec<u8>>>;
}

// ---- sources -------------------------------------------------------------------------

/// An Agave ledger, opened read-only (safe while the validator runs). The `data_shred` and
/// `code_shred` columns are keyed by `slot BE u64 | index BE u64` and hold the raw shred
/// bytes (Agave v4.4.0 `ledger/src/blockstore/column.rs`).
pub struct Ledger {
    path: PathBuf,
    db: DB,
}

impl Ledger {
    pub fn open(ledger: &Path) -> Result<Ledger> {
        let path = ledger.join("rocksdb");
        // Read-only may open a subset of column families; we only need the two shred columns.
        let db = DB::open_cf_for_read_only(
            &Options::default(),
            &path,
            ["data_shred", "code_shred"],
            false,
        )
        .with_context(|| format!("open {}", path.display()))?;
        Ok(Ledger {
            path: ledger.to_path_buf(),
            db,
        })
    }

    fn read(&self, column: &str, slot: u64) -> Result<Vec<Vec<u8>>> {
        let cf = self.db.cf_handle(column).context(column.to_string())?;
        let start = [slot.to_be_bytes(), 0u64.to_be_bytes()].concat();
        let mut out = Vec::new();
        for kv in self
            .db
            .iterator_cf(cf, IteratorMode::From(&start, rocksdb::Direction::Forward))
        {
            let (k, v) = kv?;
            if k[..8] != slot.to_be_bytes() {
                break;
            }
            out.push(v.to_vec());
        }
        Ok(out)
    }
}

impl ShredSource for Ledger {
    fn describe(&self) -> String {
        format!("ledger {}", self.path.display())
    }

    fn range(&self) -> Result<(u64, u64)> {
        let cf = self.db.cf_handle("data_shred").context("data_shred")?;
        let slot_of = |k: &[u8]| u64::from_be_bytes(k[..8].try_into().unwrap());
        let first = self
            .db
            .iterator_cf(cf, IteratorMode::Start)
            .next()
            .transpose()?
            .context("empty ledger")?;
        let last = self
            .db
            .iterator_cf(cf, IteratorMode::End)
            .next()
            .transpose()?
            .context("empty ledger")?;
        Ok((slot_of(&first.0), slot_of(&last.0)))
    }

    fn data_shreds(&self, slot: u64) -> Result<Vec<Vec<u8>>> {
        self.read("data_shred", slot)
    }

    fn code_shreds(&self, slot: u64) -> Result<Vec<Vec<u8>>> {
        self.read("code_shred", slot)
    }
}

/// A directory of `shreds_<slot>.json` files: `{ "slot", "data_shreds": [hex], "code_shreds": [hex] }`.
pub struct ShredFiles {
    dir: PathBuf,
}

impl ShredFiles {
    pub fn new(dir: &Path) -> ShredFiles {
        ShredFiles {
            dir: dir.to_path_buf(),
        }
    }

    fn file(&self, slot: u64) -> PathBuf {
        self.dir.join(format!("shreds_{slot}.json"))
    }

    fn read(&self, slot: u64, key: &str) -> Result<Vec<Vec<u8>>> {
        let path = self.file(slot);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let v = read_json(&path)?;
        v[key]
            .as_array()
            .with_context(|| format!("{}: {key}", path.display()))?
            .iter()
            .map(|h| Ok(hex::decode(h.as_str().context("hex")?)?))
            .collect()
    }
}

impl ShredSource for ShredFiles {
    fn describe(&self) -> String {
        format!("files {}", self.dir.display())
    }

    fn range(&self) -> Result<(u64, u64)> {
        let mut slots = Vec::new();
        for e in fs::read_dir(&self.dir).with_context(|| format!("read {}", self.dir.display()))? {
            let name = e?.file_name().to_string_lossy().to_string();
            if let Some(slot) = name
                .strip_prefix("shreds_")
                .and_then(|n| n.strip_suffix(".json"))
                .and_then(|n| n.parse::<u64>().ok())
            {
                slots.push(slot);
            }
        }
        Ok((
            *slots.iter().min().context("no shreds_<slot>.json files")?,
            *slots.iter().max().unwrap(),
        ))
    }

    fn data_shreds(&self, slot: u64) -> Result<Vec<Vec<u8>>> {
        self.read(slot, "data_shreds")
    }

    fn code_shreds(&self, slot: u64) -> Result<Vec<Vec<u8>>> {
        self.read(slot, "code_shreds")
    }
}

/// Writes `shreds_<slot>.json` for `slot` from any source, for [`ShredFiles`].
pub fn export(src: &dyn ShredSource, slot: u64, dir: &Path) -> Result<PathBuf> {
    let data = src.data_shreds(slot)?;
    if data.is_empty() {
        bail!("no data shreds for slot {slot} in {}", src.describe());
    }
    let code = src.code_shreds(slot)?;
    fs::create_dir_all(dir)?;
    let out = dir.join(format!("shreds_{slot}.json"));
    let v = json!({
        "slot": slot,
        "data_shreds": data.iter().map(hex::encode).collect::<Vec<_>>(),
        "code_shreds": code.iter().map(hex::encode).collect::<Vec<_>>(),
    });
    fs::write(&out, serde_json::to_string(&v)? + "\n")?;
    Ok(out)
}

// ---- certificates ----------------------------------------------------------------------

/// Where the finality certificates come from.
pub enum Certs {
    /// The child block's footer, from the same shred source (default).
    Shreds,
    /// footer-dump plugin output: every non-null `block_final_cert`, as written.
    Plugin(Vec<Value>),
}

impl Certs {
    pub fn plugin(footers: &Path) -> Result<Certs> {
        let text =
            fs::read_to_string(footers).with_context(|| format!("read {}", footers.display()))?;
        // The plugin appends while we read: skip a last line it hasn't finished writing.
        let complete = &text[..text.rfind('\n').map_or(0, |i| i + 1)];
        let mut certs = Vec::new();
        for line in complete.lines() {
            let v: Value = serde_json::from_str(line)?;
            if !v["block_final_cert"].is_null() {
                certs.push(v["block_final_cert"].clone());
            }
        }
        Ok(Certs::Plugin(certs))
    }

    pub fn name(&self) -> &'static str {
        match self {
            Certs::Shreds => "shreds",
            Certs::Plugin(_) => "plugin",
        }
    }

    /// The certificate for `slot`, in the plugin's JSON shape.
    fn for_slot(&self, src: &dyn ShredSource, slot: u64, last: u64) -> Result<Value> {
        match self {
            Certs::Shreds => child_cert(src, slot, last),
            Certs::Plugin(certs) => certs
                .iter()
                .find(|c| c["slot"] == slot)
                .cloned()
                .with_context(|| format!("no footer cert for slot {slot}")),
        }
    }
}

/// The finalization certificate in a block's footer, in the plugin's JSON shape (so both
/// sources feed the same checks and the same fixture format). `None` if the block has no
/// footer yet or the footer carries no certificate.
pub fn footer_cert(data: &[Vec<u8>]) -> Result<Option<Value>> {
    let shreds = data
        .iter()
        .map(|b| Shred::parse(b))
        .collect::<Result<Vec<_>, _>>()?;
    let Ok(components) = block::deshred(&shreds) else {
        return Ok(None);
    };
    for c in &components {
        if let Component::Marker(Marker::Footer(bytes)) = block::parse_component(c)? {
            let Some(cert) = block::parse_footer(bytes)?.final_cert else {
                return Ok(None);
            };
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
fn child_cert(src: &dyn ShredSource, slot: u64, last: u64) -> Result<Value> {
    // A footer carries the certificate of the newest block that was final when it was
    // written: often the parent, but under load a few blocks later. The certificate proves
    // itself (BLS over the block's id), so any later block's footer will do.
    let mut seen = 0;
    for later in slot + 1..=last.min(slot + 64) {
        let data = src.data_shreds(later)?;
        if data.is_empty() {
            continue;
        }
        seen += 1;
        if let Some(c) = footer_cert(&data)?.filter(|c| c["slot"] == slot) {
            return Ok(c);
        }
    }
    if seen == 0 {
        bail!(
            "no block after {slot} in {} yet: it isn't finalized, or wait a slot",
            src.describe()
        )
    }
    bail!(
        "none of the {seen} block(s) after {slot} in {} carries its certificate yet: fetch a few more",
        src.describe()
    )
}

/// The newest slot a certificate can be found for.
pub fn newest_certified(src: &dyn ShredSource, certs: &Certs) -> Result<u64> {
    let (first, last) = src.range()?;
    match certs {
        // The parent of the newest block whose footer is there.
        Certs::Shreds => {
            for s in (first..=last).rev().take(64) {
                if let Some(c) = footer_cert(&src.data_shreds(s)?)? {
                    return c["slot"].as_u64().context("slot");
                }
            }
            bail!(
                "no footer with a certificate in the last 64 slots of {}",
                src.describe()
            )
        }
        Certs::Plugin(c) => c
            .iter()
            .rev()
            .filter_map(|c| c["slot"].as_u64())
            .find(|s| (first..=last).contains(s))
            .context("no certified slot in the source's range"),
    }
}

// ---- the fixture -----------------------------------------------------------------------

/// The validator sets and shred version, written by `fetch` (`cluster_info.json`,
/// `validator_set_<epoch>.json`).
pub struct Sets {
    pub dir: PathBuf,
    pub slots_per_epoch: u64,
}

/// A checked, self-contained block fixture, and what was checked (for printing).
pub struct Built {
    pub slot: u64,
    pub fixture: Value,
    pub report: Vec<String>,
}

/// Reads slot `slot` and its certificates, and returns the fixture only if:
/// - every coding shred agrees with its FEC set's root,
/// - every shred carries the cluster's shred version,
/// - both certificates (slot and parent) verify against the epoch's validator set,
/// - the block_id rebuilt from the shreds equals the certified one.
pub fn build_fixture(
    src: &dyn ShredSource,
    slot: u64,
    sets: &Sets,
    certs: &Certs,
) -> Result<Built> {
    let (first, last) = src.range()?;
    let data = src.data_shreds(slot)?;
    let code = src.code_shreds(slot)?;
    if data.is_empty() {
        bail!(
            "no data shreds for slot {slot} ({} holds {first} ..= {last})",
            src.describe()
        );
    }
    let shreds = data
        .iter()
        .map(|b| Shred::parse(b))
        .collect::<Result<Vec<_>, _>>()?;
    for b in &code {
        let s = Shred::parse(b)?;
        if s.kind() != Kind::Code
            || s.merkle_root()? != shreds[s.fec_set_index() as usize].merkle_root()?
        {
            bail!("coding shred {} disagrees with its FEC set root", s.index());
        }
    }
    let roots = shred::fec_roots(slot, &shreds)?;
    let parent_slot = shreds[0].parent_slot().context("parent slot")?;

    // The shred version every vote signs: the cluster's, and the one in the shreds.
    let cluster = read_json(&sets.dir.join("cluster_info.json"))?;
    let shred_version = cluster["shred_version"].as_u64().context("shred_version")? as u16;
    if let Some(s) = shreds.iter().find(|s| s.version() != shred_version) {
        bail!(
            "shred {} has version {}, cluster has {shred_version}",
            s.index(),
            s.version()
        );
    }

    // The validator set of the slot's epoch. RPC only serves the current epoch's stakes,
    // so the set must have been fetched during that epoch.
    let epoch = slot / sets.slots_per_epoch;
    let set_path = sets.dir.join(format!("validator_set_{epoch}.json"));
    let set_json = read_json(&set_path).with_context(|| {
        format!("no set for epoch {epoch}: run `fetch validator-set --cluster local` during it")
    })?;
    let set: ValidatorSet = serde_json::from_value(set_json["validator_set"].clone())?;
    if set.epoch != epoch {
        bail!(
            "{} holds epoch {}, slot {slot} is in epoch {epoch}",
            set_path.display(),
            set.epoch
        );
    }
    // A parent in an earlier epoch would need that epoch's set too; not handled yet.
    if parent_slot / sets.slots_per_epoch != epoch {
        bail!("parent {parent_slot} is in a different epoch than {slot}");
    }

    // The parent's own certificate is optional: a footer carries the cert of the newest
    // block that was final when it was written, which on a busy cluster is often not the
    // parent, and finalizing a block finalizes its ancestors, so a parent may never get one.
    // When it is there (a local chain, mostly) it is checked too.
    let raw_parent = match certs {
        Certs::Shreds => footer_cert(&data)?.filter(|c| c["slot"] == parent_slot),
        Certs::Plugin(_) => certs.for_slot(src, parent_slot, last).ok(),
    };
    let raw_cert = certs.for_slot(src, slot, last)?;
    let cert: FooterCert = serde_json::from_value(raw_cert.clone())?;
    verify_footer(&cert, shred_version, &set).with_context(|| format!("cert for {slot}"))?;
    let parent_cert: Option<FooterCert> = match &raw_parent {
        Some(raw) => {
            let c: FooterCert = serde_json::from_value(raw.clone())?;
            verify_footer(&c, shred_version, &set)
                .with_context(|| format!("cert for {parent_slot}"))?;
            Some(c)
        }
        None => None,
    };

    // The parent's block id is written in the block itself (its header, and any
    // update-parent marker) and bound into the block_id the certificate signs. So any
    // candidate is fine as an input: only the right one rebuilds the certified block_id.
    let mut candidates: Vec<(u64, [u8; 32])> = Vec::new();
    if let Some(c) = &parent_cert {
        candidates.push((parent_slot, c.block_id));
    }
    if let Ok(components) = block::deshred(&shreds) {
        for c in &components {
            match block::parse_component(c) {
                Ok(Component::Marker(Marker::Header {
                    parent_slot: ps,
                    parent_block_id: pb,
                }))
                | Ok(Component::Marker(Marker::UpdateParent {
                    parent_slot: ps,
                    parent_block_id: pb,
                })) => candidates.push((ps, pb)),
                _ => {}
            }
        }
    }
    let (parent_slot_used, parent_block_id) = candidates
        .iter()
        .copied()
        .find(|(ps, pb)| shred::block_id(&roots, *ps, pb).is_ok_and(|id| id == cert.block_id))
        .or(candidates.first().copied())
        .with_context(|| format!("slot {slot}: no parent block id in its header or footer"))?;
    let parent_source = if parent_cert.is_some() {
        "its certificate"
    } else {
        "the block's header"
    };
    let computed = shred::block_id(&roots, parent_slot_used, &parent_block_id)?;
    let report = vec![
        format!("slot {slot} (epoch {epoch}): {} data + {} coding shreds, {} FEC set(s), parent {parent_slot}", data.len(), code.len(), roots.len()),
        format!(
            "  cert for {slot}{} verifies (shred version {shred_version}, from {}); parent block id from {parent_source}",
            if parent_cert.is_some() { format!(" and for {parent_slot}") } else { String::new() },
            match certs {
                Certs::Shreds => "the shreds' footers",
                Certs::Plugin(_) => "the plugin",
            }
        ),
        format!("  computed block_id  {}", hex::encode(computed)),
        format!("  certified block_id {}", hex::encode(cert.block_id)),
    ];
    if computed != cert.block_id {
        bail!(
            "{}\nblock_id mismatch: not writing a fixture",
            report.join("\n")
        );
    }

    let fixture = json!({
        "cluster": "local",
        "slot": slot,
        "parent_slot": parent_slot_used,
        "epoch": epoch,
        "shred_version": shred_version,
        "validator_set": set_json["validator_set"],
        "cert_source": certs.name(),
        "block_final_cert": raw_cert,
        "parent_block_id": hex::encode(parent_block_id),
        "parent_block_final_cert": raw_parent,
        // Coding shreds aren't needed to rebuild the block_id (checked above, not stored).
        "data_shreds": data.iter().map(hex::encode).collect::<Vec<_>>(),
    });
    Ok(Built {
        slot,
        fixture,
        report,
    })
}

pub fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_str(
        &fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
    )?)
}

/// Most validators we will try dropping from a fetched set to match a certificate.
const MAX_DROPPED: usize = 4;

/// Every way to leave out `k` of `n` ranks, in order.
fn omissions(n: usize, k: usize) -> Vec<Vec<usize>> {
    fn go(start: usize, n: usize, k: usize, cur: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if cur.len() == k {
            out.push(cur.clone());
            return;
        }
        for i in start..n {
            cur.push(i);
            go(i + 1, n, k, cur, out);
            cur.pop();
        }
    }
    let mut out = Vec::new();
    go(0, n, k, &mut Vec::new(), &mut out);
    out
}

/// Makes the saved validator set agree with real certificates.
///
/// `fetch validator-set` reads the vote accounts as they are now, but the certificates were
/// signed by the set the epoch started with. A validator that registered a BLS key after the
/// epoch began is in our list and not in the signers' list, which shifts every rank below it
/// and makes the signature fail. The signer bitmap says how many validators the signers had,
/// so we leave out exactly that many of ours.
///
/// One certificate can leave several choices open (a validator that did not sign that block
/// is not tested by it), so every given slot's certificate must verify, and we keep the one
/// choice they all accept. Nothing is guessed: a wrong set cannot verify, and if no choice or
/// more than one survives we stop and say so (give it more slots).
///
/// Returns what it did, one line each. The set file is rewritten only when it was changed.
pub fn reconcile_set(
    src: &dyn ShredSource,
    slots: &[u64],
    sets: &Sets,
    certs: &Certs,
) -> Result<Vec<String>> {
    let (_, last) = src.range()?;
    let shred_version = read_json(&sets.dir.join("cluster_info.json"))?["shred_version"]
        .as_u64()
        .context("shred_version")? as u16;
    let first = *slots.first().context("no slots to check against")?;
    let epoch = first / sets.slots_per_epoch;
    let path = sets.dir.join(format!("validator_set_{epoch}.json"));
    let mut file = read_json(&path)?;
    let set: ValidatorSet = serde_json::from_value(file["validator_set"].clone())?;
    let n = set.entries.len();

    let mut certified: Vec<(u64, FooterCert)> = Vec::new();
    for &slot in slots {
        if slot / sets.slots_per_epoch != epoch {
            continue;
        }
        if let Ok(raw) = certs.for_slot(src, slot, last) {
            certified.push((slot, serde_json::from_value(raw)?));
        }
    }
    if certified.is_empty() {
        bail!("none of the {} slots has a certificate", slots.len());
    }

    let without = |drop: &[usize]| {
        let entries: Vec<_> = set
            .entries
            .iter()
            .enumerate()
            .filter(|(i, _)| !drop.contains(i))
            .map(|(_, e)| e.clone())
            .collect();
        let total_stake = entries.iter().fold(0u64, |t, e| t.saturating_add(e.stake));
        ValidatorSet {
            epoch: set.epoch,
            total_stake,
            entries,
        }
    };

    if certified
        .iter()
        .all(|(_, c)| verify_footer(c, shred_version, &set).is_ok())
    {
        return Ok(vec![format!(
            "set for epoch {epoch} ({n} validators) matches the certificates of {} blocks",
            certified.len()
        )]);
    }

    // How many validators the signers had, when the bitmap says (version 0, then a u16 count).
    let bm = &certified[0].1.final_aggregate.bitmap;
    let sizes: Vec<usize> = match (bm.first(), bm.get(1..3)) {
        (Some(0), Some(c)) => {
            let bits = u16::from_le_bytes([c[0], c[1]]) as usize;
            if bits >= n {
                bail!("the certificates were signed by {bits} validators; our set has {n}, so nothing to leave out");
            }
            vec![bits]
        }
        _ => (n.saturating_sub(MAX_DROPPED)..n).rev().collect(),
    };

    // Choices the first certificate accepts, then only those the others accept too.
    let mut survivors: Vec<Vec<usize>> = Vec::new();
    for size in sizes {
        let k = n - size;
        if k > MAX_DROPPED {
            bail!("the signers are {k} fewer than our set; will not guess past {MAX_DROPPED}");
        }
        survivors = omissions(n, k)
            .into_iter()
            .filter(|d| verify_footer(&certified[0].1, shred_version, &without(d)).is_ok())
            .collect();
        if !survivors.is_empty() {
            break;
        }
    }
    for (_, c) in &certified[1..] {
        survivors.retain(|d| verify_footer(c, shred_version, &without(d)).is_ok());
    }
    let blocks = certified.len();
    if survivors.is_empty() {
        bail!("no way of leaving out a few validators makes all {blocks} certificates verify: the set is wrong in some other way");
    }
    // Several choices left means the validators they differ on never signed in these blocks,
    // so no certificate here can tell them apart and every choice verifies exactly the same
    // blocks. Take the first and say so; a later block where one of them signs would show it.
    let equal = survivors.len();
    let drop = survivors.remove(0);
    let fixed = without(&drop);

    let mut report = vec![format!(
        "set for epoch {epoch} had {n} validators; {blocks} certificates were signed by {}. Left out ranks {drop:?}, which the epoch's snapshot did not have (new stake, low balance or no BLS key then); the rest verifies",
        fixed.entries.len()
    )];
    if equal > 1 {
        report.push(format!(
            "  note: {equal} choices verify these {blocks} blocks equally (they differ on validators that did not sign in them); took the first. If a block they did sign in fails to verify, rerun"
        ));
    }
    for &i in &drop {
        report.push(format!(
            "  left out rank {i}: vote account {} stake {}",
            hex::encode(set.entries[i].vote_pubkey),
            set.entries[i].stake
        ));
    }
    file["validator_set"] = serde_json::to_value(&fixed)?;
    file["commitment"] = json!(hex::encode(fixed.commitment()));
    file["stats"]["ranked"] = json!(fixed.entries.len());
    file["reconciled"] = json!({
        "checked_slots": certified.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        "left_out_ranks": drop,
        "equally_valid_choices": equal,
        "why": "not in the epoch's stake snapshot (taken at the start of the previous epoch): no stake yet, too little balance, or no BLS key then",
    });
    fs::write(&path, serde_json::to_string_pretty(&file)? + "\n")?;
    report.push(format!(
        "  new commitment {} -> {}",
        hex::encode(fixed.commitment()),
        path.display()
    ));
    Ok(report)
}

#[cfg(test)]
mod reconcile_tests {
    use super::omissions;

    #[test]
    fn omissions_lists_every_choice_once() {
        assert_eq!(omissions(4, 2).len(), 6);
        assert_eq!(omissions(23, 3).len(), 1771);
        assert_eq!(omissions(3, 0), vec![Vec::<usize>::new()]);
        assert_eq!(omissions(3, 1), vec![vec![0], vec![1], vec![2]]);
    }
}
