//! Fetch a Solana block's shreds with Agave's own repair protocol.
//!
//! A validator serves shreds to any node that asks (after a ping/pong handshake), for any
//! slot still in its ledger. For Alpenglow blocks it also answers by `block_id`: how many
//! FEC sets a block has, each set's root with a proof to the `block_id`, and any data shred.
//! Every answer here is checked against a certified `block_id`, so the peer is untrusted.

pub mod client;
pub mod wire;

use {
    alpenglow_core::shred::{self, Shred},
    anyhow::{bail, ensure, Context, Result},
    wire::Hash,
};

const SET: usize = shred::DATA_SHREDS_PER_FEC_SET;

fn pad(entry: &[u8; 20]) -> Hash {
    let mut h = [0u8; 32];
    h[..20].copy_from_slice(entry);
    h
}

fn entries(proof: &[u8]) -> Result<Vec<&[u8]>> {
    ensure!(
        proof.len().is_multiple_of(20),
        "proof is not whole 20-byte entries"
    );
    Ok(proof.chunks(20).collect())
}

/// What the peer told us about a block, all verified against `block_id`.
#[derive(Debug)]
pub struct BlockShape {
    pub fec_set_count: u32,
    pub parent_slot: u64,
    pub parent_block_id: Hash,
    /// Each set's root as the 20-byte entry the block id commits to.
    pub roots: Vec<[u8; 20]>,
}

/// Asks for the parent info and every FEC set root, and checks each against `block_id`.
/// No shred is downloaded: this alone proves the shape of the certified block.
pub fn fetch_shape(c: &mut client::Client, slot: u64, block_id: Hash) -> Result<BlockShape> {
    let (count, parent_slot, parent_block_id, proof) = c.parent(slot, block_id)?;
    ensure!(count > 0, "peer reports zero FEC sets");
    // The parent info is the final leaf, after the `count` set roots.
    let leaf = shred::parent_leaf(parent_slot, &parent_block_id, count);
    let root = shred::root_from_proof(count as usize, leaf, entries(&proof)?)?;
    ensure!(
        root == block_id,
        "parent proof does not lead to the block id"
    );
    let mut roots = Vec::with_capacity(count as usize);
    for i in 0..count {
        let (entry, proof) = c.fec_root(slot, block_id, i * SET as u32)?;
        let got = shred::root_from_proof(i as usize, pad(&entry), entries(&proof)?)?;
        ensure!(
            got == block_id,
            "FEC set {i}: proof does not lead to the block id"
        );
        roots.push(entry);
    }
    Ok(BlockShape {
        fec_set_count: count,
        parent_slot,
        parent_block_id,
        roots,
    })
}

/// All data shreds of the block `block_id`, checked against the roots the peer proved.
pub fn fetch_block(
    c: &mut client::Client,
    slot: u64,
    block_id: Hash,
) -> Result<(BlockShape, Vec<Vec<u8>>)> {
    let shape = fetch_shape(c, slot, block_id)?;
    let total = shape.fec_set_count as usize * SET;
    let mut shreds = Vec::with_capacity(total);
    for i in 0..total {
        shreds.push(c.window_for_block(slot, i as u32, block_id)?);
    }
    check_shreds(slot, &shreds, Some(&shape))?;
    Ok((shape, shreds))
}

/// All data shreds of `slot` when the block id is not known (the child block, whose footer
/// carries the certificate). Nothing here is trusted yet: the certificate check does that.
pub fn fetch_slot(c: &mut client::Client, slot: u64) -> Result<Vec<Vec<u8>>> {
    let top = c.highest(slot, 0)?;
    let top = Shred::parse(&top).context("parse the highest shred")?;
    ensure!(
        top.last_in_slot(),
        "the peer's highest shred for slot {slot} is not the last one: block incomplete there"
    );
    let last = top.index() as u64;
    let mut shreds = Vec::with_capacity(last as usize + 1);
    for i in 0..=last {
        shreds.push(c.window(slot, i)?);
    }
    check_shreds(slot, &shreds, None)?;
    Ok(shreds)
}

/// The shreds are a complete block, and (if `shape` is given) its set roots are the proven ones.
pub fn check_shreds(slot: u64, shreds: &[Vec<u8>], shape: Option<&BlockShape>) -> Result<()> {
    let parsed = shreds
        .iter()
        .map(|b| Shred::parse(b))
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("parse shreds")?;
    let roots = shred::fec_roots(slot, &parsed).context("shreds do not form a complete block")?;
    if let Some(shape) = shape {
        ensure!(
            roots.len() == shape.roots.len(),
            "shred sets {} vs proven sets {}",
            roots.len(),
            shape.roots.len()
        );
        for (i, (r, want)) in roots.iter().zip(&shape.roots).enumerate() {
            if &r[..20] != want {
                bail!("FEC set {i}: the shreds' root is not the one the block id commits to");
            }
        }
    }
    Ok(())
}

/// Writes `shreds_<slot>.json` in the format `ledger-dump --shreds` reads (data shreds only:
/// every data shred's proof already yields its set's root).
pub fn write_shred_file(
    dir: &std::path::Path,
    slot: u64,
    shreds: &[Vec<u8>],
) -> Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("shreds_{slot}.json"));
    let v = serde_json::json!({
        "slot": slot,
        "data_shreds": shreds.iter().map(hex::encode).collect::<Vec<_>>(),
        "code_shreds": Vec::<String>::new(),
    });
    std::fs::write(&path, serde_json::to_string(&v)? + "\n")?;
    Ok(path)
}

/// The slot of the certificate in this block's footer, if it carries one.
pub fn footer_cert_slot(data_shreds: &[Vec<u8>]) -> Option<u64> {
    use alpenglow_core::block::{self, Component, Marker};
    let parsed = data_shreds
        .iter()
        .map(|b| Shred::parse(b))
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()?;
    for c in block::deshred(&parsed).ok()? {
        if let Component::Marker(Marker::Footer(bytes)) = block::parse_component(&c).ok()? {
            return block::parse_footer(bytes).ok()?.final_cert.map(|c| c.slot);
        }
    }
    None
}
