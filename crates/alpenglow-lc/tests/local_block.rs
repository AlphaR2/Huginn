//! A whole local block, end to end (item 2.4): raw data shreds -> FEC set roots ->
//! double Merkle root, which must equal the block_id the finality certificate signed.
//!
//! `fixtures/local/block_44000.json` was written by `ledger-dump --slot 44000` and is
//! self-contained: epoch-0 validator set, shred version, the footer certs for 44000 and its
//! parent 43999, and slot 44000's 96 data shreds.

use {
    alpenglow_lc::{
        cert::{verify_footer, FooterCert},
        shred::{self, Hash, Shred},
        validator_set::ValidatorSet,
    },
    serde_json::Value,
};

fn json(file: &str) -> Value {
    let p = format!("{}/../../fixtures/local/{file}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

struct Block {
    slot: u64,
    shred_version: u16,
    set: ValidatorSet,
    parent_slot: u64,
    cert: FooterCert,
    parent_cert: FooterCert,
    shreds: Vec<Vec<u8>>,
}

fn load() -> Block {
    let v = json("block_44000.json");
    Block {
        slot: v["slot"].as_u64().unwrap(),
        shred_version: v["shred_version"].as_u64().unwrap() as u16,
        set: serde_json::from_value(v["validator_set"].clone()).unwrap(),
        parent_slot: v["parent_slot"].as_u64().unwrap(),
        cert: serde_json::from_value(v["block_final_cert"].clone()).unwrap(),
        parent_cert: serde_json::from_value(v["parent_block_final_cert"].clone()).unwrap(),
        shreds: v["data_shreds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| hex::decode(s.as_str().unwrap()).unwrap())
            .collect(),
    }
}

/// Block id from raw shreds, with the parent's id taken from the parent's certificate.
fn rebuild(b: &Block, shreds: &[Vec<u8>]) -> Result<Hash, shred::Error> {
    let parsed = shreds
        .iter()
        .map(|s| Shred::parse(s))
        .collect::<Result<Vec<_>, _>>()?;
    let roots = shred::fec_roots(b.slot, &parsed)?;
    shred::block_id(&roots, b.parent_slot, &b.parent_cert.block_id)
}

#[test]
fn shreds_rebuild_the_certified_block_id() {
    let b = load();
    // The embedded set is the one `fetch` saved for epoch 0.
    let saved: ValidatorSet =
        serde_json::from_value(json("validator_set_0.json")["validator_set"].clone()).unwrap();
    assert_eq!(b.set, saved);
    // Both blocks are final, under the exact epoch-0 set.
    assert_eq!(
        verify_footer(&b.cert, b.shred_version, &b.set)
            .unwrap()
            .slot,
        b.slot
    );
    assert_eq!(
        verify_footer(&b.parent_cert, b.shred_version, &b.set)
            .unwrap()
            .slot,
        b.parent_slot
    );
    // The parent slot in the shred header matches the fixture.
    assert_eq!(
        Shred::parse(&b.shreds[0]).unwrap().parent_slot(),
        Some(b.parent_slot)
    );

    assert_eq!(rebuild(&b, &b.shreds).unwrap(), b.cert.block_id);
}

#[test]
fn shred_layout_matches_expectations() {
    let b = load();
    let shreds: Vec<_> = b.shreds.iter().map(|s| Shred::parse(s).unwrap()).collect();
    assert_eq!(shreds.len(), 96);
    let roots = shred::fec_roots(b.slot, &shreds).unwrap();
    assert_eq!(roots.len(), 3);
    // Only the final shred is flagged last-in-slot.
    assert!(shreds[95].last_in_slot());
    assert!(shreds[..95].iter().all(|s| !s.last_in_slot()));
    // Every shred carries the cluster's shred version, the one the votes signed.
    assert!(shreds.iter().all(|s| s.version() == b.shred_version));
    // Sets 1 and 2 chain to the set before (checked inside fec_roots); set 0 chains to
    // the parent's *last FEC set root*, not to the parent's block_id.
    assert_ne!(shreds[0].chained_merkle_root(), b.parent_cert.block_id);
}

#[test]
fn one_changed_byte_in_any_shred_breaks_it() {
    let b = load();
    // Entry data in the first, a middle and the last shred; also a header byte.
    for (i, byte) in [(0, 100), (40, 500), (95, 200), (17, 70)] {
        let mut shreds = b.shreds.clone();
        shreds[i][byte] ^= 1;
        // An Err is fine too: caught earlier, as a root mismatch inside the set or a bad header.
        if let Ok(id) = rebuild(&b, &shreds) {
            assert_ne!(id, b.cert.block_id, "shred {i} byte {byte}");
        }
    }
}

#[test]
fn missing_or_extra_shreds_are_rejected() {
    let b = load();
    let mut s = b.shreds.clone();
    s.remove(50);
    assert_eq!(rebuild(&b, &s).unwrap_err(), shred::Error::Missing(50));

    // Drop the whole last FEC set: the remaining shreds look complete up to 63, but no
    // shred says "last in slot", so a truncated block can't pass.
    let s = b.shreds[..64].to_vec();
    assert_eq!(rebuild(&b, &s).unwrap_err(), shred::Error::Incomplete);

    let mut s = b.shreds.clone();
    s.push(b.shreds[10].clone());
    assert_eq!(rebuild(&b, &s).unwrap_err(), shred::Error::Duplicate(10));
}

#[test]
fn wrong_parent_changes_the_block_id() {
    let b = load();
    let shreds: Vec<_> = b.shreds.iter().map(|s| Shred::parse(s).unwrap()).collect();
    let roots = shred::fec_roots(b.slot, &shreds).unwrap();
    let parent = b.parent_cert.block_id;
    assert_eq!(
        shred::block_id(&roots, b.parent_slot, &parent).unwrap(),
        b.cert.block_id
    );
    assert_ne!(
        shred::block_id(&roots, b.parent_slot - 1, &parent).unwrap(),
        b.cert.block_id
    );
    let mut other = parent;
    other[0] ^= 1;
    assert_ne!(
        shred::block_id(&roots, b.parent_slot, &other).unwrap(),
        b.cert.block_id
    );
    // Dropping a FEC root changes both the tree and the count bound into the last leaf.
    assert_ne!(
        shred::block_id(&roots[..2], b.parent_slot, &parent).unwrap(),
        b.cert.block_id
    );
}
