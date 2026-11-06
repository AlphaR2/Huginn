//! The three statements in `proof.rs`, on real local data and synthetic validator sets.
//! - Finality (items 1.2, 1.3): both paths, and the attacks a verifier must refuse.
//! - Whole block (2.4 shape): shreds rebuild the certified block_id.
//! - One transaction (3.3, 3.4): found with only its component's shreds; every tamper fails.

use {
    alpenglow_core::{
        finality::{self, CertType},
        fixture,
        proof::{self, Error, FinalityInput, Path, TxInput},
        shred::Shred,
    },
    serde_json::Value,
};

fn json(file: &str) -> Value {
    let p = format!("{}/../../fixtures/local/{file}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn tx_sig() -> [u8; 64] {
    hex::decode(json("tx_46718.json")["signature"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap()
}

fn real_finality() -> FinalityInput {
    fixture::block_input(&json("block_46718.json")).finality
}

fn real_tx() -> TxInput {
    fixture::tx_input(&json("block_46718.json"), tx_sig())
}

fn bad_sig() -> Error {
    Error::Finality(finality::Error::VerifySig)
}

// ---------- Finality (1.2) ----------

#[test]
fn real_fast_path_cert_verifies() {
    let f = real_finality().verify().unwrap();
    assert_eq!(
        (f.slot, f.path, f.shred_version),
        (46718, Path::Fast, 28090)
    );
    assert_eq!(f.signed_stake, f.total_stake);
    // The commitment the destination chain would store for epoch 0.
    assert_eq!(
        f.validator_set_hash,
        proof::validator_set_hash(&real_finality().validators)
    );
}

#[test]
fn synthetic_fast_and_slow_paths_verify() {
    let base = real_finality();
    for n in [1, 7, 100] {
        assert_eq!(
            fixture::synthetic(&base, n).verify().unwrap().path,
            Path::Fast,
            "n={n}"
        );
        assert_eq!(
            fixture::synthetic_slow(&base, n).verify().unwrap().path,
            Path::Slow,
            "n={n}"
        );
    }
}

// ---------- Adversarial finality (1.3) ----------

#[test]
fn slow_path_trap_finalize_alone_proves_nothing_about_the_block() {
    let base = real_finality();
    let slow = fixture::synthetic_slow(&base, 10);
    // Drop the Notarize part: the Finalize(slot) signature read as FinalizeFast(block).
    let mut c = slow.clone();
    c.notar_aggregate = None;
    assert_eq!(c.verify().unwrap_err(), bad_sig());
    // Keep a genuine Finalize(slot) but swap in a Notarize for a *different* block: the
    // footer claims block_id, so the notar signature can't match.
    let (sks, _) = fixture::synthetic_set(10);
    let mut other = base.block_id;
    other[0] ^= 1;
    let mut c = slow.clone();
    c.notar_aggregate = Some(fixture::sign(
        &sks,
        &fixture::most(10),
        CertType::Notarize {
            slot: base.slot,
            block_id: other,
        },
        base.shred_version,
    ));
    assert_eq!(c.verify().unwrap_err(), bad_sig());
    // And claiming the other block instead fails the Finalize? No: Finalize names no block.
    // It's the Notarize part that must match the claimed block_id, which is what we check.
    let mut c = slow;
    c.block_id = other;
    assert_eq!(c.verify().unwrap_err(), bad_sig());
}

#[test]
fn shred_version_mismatch_fails() {
    let mut c = real_finality();
    c.shred_version ^= 1;
    assert_eq!(c.verify().unwrap_err(), bad_sig());
}

#[test]
fn wrong_epoch_set_fails() {
    // The cert was made by synthetic set A; check it against set B (other keys, other
    // stakes), as if the verifier had picked the wrong epoch's set.
    let base = real_finality();
    let a = fixture::synthetic(&base, 10);
    let mut c = a.clone();
    c.validators = fixture::synthetic_set(11).1[1..].to_vec(); // shifted keys, same size
    assert_eq!(c.verify().unwrap_err(), bad_sig());
    // Same keys but in a different rank order: the bitmap now names different people.
    let mut c = a;
    c.validators.reverse();
    assert!(c.verify().is_err());
}

#[test]
fn genuine_signatures_below_threshold_fail() {
    let base = real_finality();
    let (sks, validators) = fixture::synthetic_set(10);
    let total: u64 = validators.iter().map(|v| v.stake).sum();
    // Smallest prefix of ranks with >= 60% but < 80% of stake.
    let mut signers = vec![];
    let mut acc = 0;
    for (i, v) in validators.iter().enumerate() {
        if acc * 100 >= total * 60 {
            break;
        }
        acc += v.stake;
        signers.push(i);
    }
    assert!(acc * 100 < total * 80);
    let fast = FinalityInput {
        validators: validators.clone(),
        final_aggregate: fixture::sign(
            &sks,
            &signers,
            CertType::FinalizeFast {
                slot: base.slot,
                block_id: base.block_id,
            },
            base.shred_version,
        ),
        notar_aggregate: None,
        ..base.clone()
    };
    assert!(matches!(
        fast.verify().unwrap_err(),
        Error::Finality(finality::Error::NotEnoughStake { .. })
    ));
    // The same signers are enough for the slow path (60%).
    let slow = FinalityInput {
        notar_aggregate: Some(fixture::sign(
            &sks,
            &signers,
            CertType::Notarize {
                slot: base.slot,
                block_id: base.block_id,
            },
            base.shred_version,
        )),
        final_aggregate: fixture::sign(
            &sks,
            &signers,
            CertType::Finalize { slot: base.slot },
            base.shred_version,
        ),
        ..fast
    };
    assert_eq!(slow.verify().unwrap().path, Path::Slow);
}

#[test]
fn inflating_stakes_changes_the_set_commitment() {
    // A prover could lie about stakes inside its own input and pass the threshold. What
    // stops it: the output carries the set hash, and the destination chain compares it to
    // the hash it stored for that epoch. Any change to a key, a stake or the order shows.
    let base = fixture::synthetic(&real_finality(), 10);
    let honest = proof::validator_set_hash(&base.validators);
    let mut v = base.validators.clone();
    v[3].stake += 1;
    assert_ne!(proof::validator_set_hash(&v), honest);
    let mut v = base.validators.clone();
    v.swap(0, 1);
    assert_ne!(proof::validator_set_hash(&v), honest);
    let mut v = base.validators.clone();
    v[9].bls_pubkey[47] ^= 1;
    assert_ne!(proof::validator_set_hash(&v), honest);
    assert_ne!(proof::validator_set_hash(&base.validators[..9]), honest);
}

#[test]
fn malformed_sets_are_rejected() {
    let mut c = real_finality();
    c.validators[0].stake = 0;
    assert_eq!(c.verify().unwrap_err(), Error::EmptySet);
    let mut c = real_finality();
    c.validators[0].bls_pubkey.pop();
    assert_eq!(c.verify().unwrap_err(), Error::KeyLength(0));
    let mut c = real_finality();
    c.validators[0].bls_pubkey = vec![0xff; 48]; // not a curve point
    assert_eq!(
        c.verify().unwrap_err(),
        Error::Finality(finality::Error::MissingRank(0))
    );
    let mut c = real_finality();
    c.final_aggregate.bitmap = vec![0, 2, 0, 0b11]; // claims 2 ranks, set has 1
    assert_eq!(
        c.verify().unwrap_err(),
        Error::Finality(finality::Error::Decode)
    );
}

// ---------- Whole block ----------

#[test]
fn whole_blocks_verify() {
    for f in ["block_44000.json", "block_46718.json"] {
        fixture::block_input(&json(f))
            .verify()
            .unwrap_or_else(|e| panic!("{f}: {e}"));
    }
}

// ---------- One transaction (3.3) ----------

#[test]
fn real_transaction_is_proven_with_one_component() {
    let input = real_tx();
    let out = input.verify().unwrap();
    let rpc = json("tx_46718.json");
    assert_eq!(hex::encode(&out.tx.raw), rpc["raw"].as_str().unwrap());
    // Only the transaction's component (one FEC set of 32 shreds), not all 128.
    assert_eq!(input.inclusion.shreds.len(), 32);
    assert_eq!(input.inclusion.fec_roots.len(), 4);
    assert_eq!(
        (
            out.tx.component_start_shred,
            out.tx.entry_index,
            out.tx.tx_index
        ),
        (32, 0, 0)
    );

    // Public values: fixed offsets, as documented on `TxOutput::encode`.
    let pv = out.encode();
    assert_eq!(pv[0], 1);
    assert_eq!(&pv[1..33], &out.finality.validator_set_hash);
    assert_eq!(u64::from_be_bytes(pv[35..43].try_into().unwrap()), 46718);
    assert_eq!(&pv[43..75], &out.finality.block_id);
    assert_eq!(
        u16::from_be_bytes(pv[104..106].try_into().unwrap()) as usize,
        out.tx.raw.len()
    );
    assert_eq!(&pv[106..], out.tx.raw.as_slice());
}

// ---------- Inclusion tampering (3.4) ----------

#[test]
fn changed_byte_in_any_component_shred_fails() {
    for (i, at) in [(0, 100), (5, 600), (31, 1000), (31, 1150)] {
        let mut input = real_tx();
        input.inclusion.shreds[i][at] ^= 1;
        assert!(input.verify().is_err(), "shred {i} byte {at}");
    }
}

#[test]
fn merkle_proof_sibling_changed_fails() {
    // The last bytes of a shred are its Merkle proof (no retransmitter signature here).
    let mut input = real_tx();
    let n = input.inclusion.shreds[7].len();
    input.inclusion.shreds[7][n - 5] ^= 1;
    assert_eq!(input.verify().unwrap_err(), Error::NotInBlock(39));
}

#[test]
fn wrong_fec_root_or_parent_fails() {
    let mut input = real_tx();
    input.inclusion.fec_roots[3][0] ^= 1; // a root we don't even use for the tx
    assert!(matches!(
        input.verify().unwrap_err(),
        Error::BlockIdMismatch { .. }
    ));
    let mut input = real_tx();
    input.inclusion.fec_roots.pop(); // pretend the block has 3 FEC sets
    assert!(matches!(
        input.verify().unwrap_err(),
        Error::BlockIdMismatch { .. }
    ));
    let mut input = real_tx();
    input.inclusion.parent_slot -= 1;
    assert!(matches!(
        input.verify().unwrap_err(),
        Error::BlockIdMismatch { .. }
    ));
}

#[test]
fn component_boundary_is_enforced() {
    // No boundary shred although the run doesn't start at 0.
    let mut input = real_tx();
    input.inclusion.boundary_shred = None;
    assert_eq!(input.verify().unwrap_err(), Error::NotAtBoundary);
    // Start one shred late, using the run's own first shred as "boundary": it's in the
    // block, but not DATA_COMPLETE, so the run would start mid-component.
    let mut input = real_tx();
    let first = input.inclusion.shreds.remove(0);
    input.inclusion.boundary_shred = Some(first);
    assert_eq!(input.verify().unwrap_err(), Error::NotAtBoundary);
}

#[test]
fn truncated_or_foreign_shreds_fail() {
    // Drop the last shred: the component never completes.
    let mut input = real_tx();
    input.inclusion.shreds.pop();
    assert!(input.verify().is_err());
    // Shreds from block 44000 against block 46718's roots.
    let other = fixture::block_input(&json("block_44000.json"));
    let mut input = real_tx();
    input.inclusion.shreds = other.data_shreds[32..64].to_vec();
    assert!(matches!(input.verify().unwrap_err(), Error::NotInBlock(_)));
}

#[test]
fn transaction_not_in_component_fails() {
    let mut input = real_tx();
    input.inclusion.signature[0] ^= 1;
    assert_eq!(input.verify().unwrap_err(), Error::TxNotFound);
}

#[test]
fn finality_failure_stops_inclusion() {
    let mut input = real_tx();
    input.finality.final_aggregate.bitmap[3] = 0;
    assert!(matches!(input.verify().unwrap_err(), Error::Finality(_)));
}

#[test]
fn parsers_never_panic_on_mangled_shreds() {
    // Cheap fuzz: random byte changes anywhere in the component; every outcome is an Err
    // or (if the change only hit padding) the same answer, never a panic.
    let base = real_tx();
    let honest = base.verify().unwrap();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for _ in 0..300 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let mut input = base.clone();
        let i = (seed % 32) as usize;
        let at = ((seed >> 8) % 1203) as usize;
        input.inclusion.shreds[i][at] ^= 1 << ((seed >> 20) % 8);
        if let Ok(out) = input.verify() {
            assert_eq!(out, honest);
        }
    }
    // Shred::parse on truncated bytes.
    let s = &base.inclusion.shreds[0];
    for len in [0, 10, 64, 65, 88, 500, 1202] {
        assert!(Shred::parse(&s[..len]).is_err());
    }
}
