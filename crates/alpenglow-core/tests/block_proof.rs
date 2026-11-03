//! The SP1 program's entry point (`BlockProofInput::verify`), run natively on the real
//! block-44000 fixture and on synthetic sets.

use alpenglow_core::{
    block_proof::{BlockProofInput, Error},
    finality, fixture,
};

fn real() -> BlockProofInput {
    let p = format!("{}/../../fixtures/local/block_44000.json", env!("CARGO_MANIFEST_DIR"));
    fixture::from_block_fixture(&serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap())
}

#[test]
fn real_block_verifies() {
    let input = real();
    let out = input.verify().unwrap();
    assert_eq!((out.slot, out.shred_version), (44000, 28090));
    assert_eq!(out.block_id, input.block_id);
    assert_eq!(out.signed_stake, out.total_stake); // one validator, it signed
}

#[test]
fn tampering_fails() {
    let mut i = real();
    i.block_id[0] ^= 1; // cert no longer matches
    assert_eq!(i.verify().unwrap_err(), Error::Finality(finality::Error::VerifySig));

    let mut i = real();
    i.parent_block_id[0] ^= 1; // cert fine, shreds rebuild a different block_id
    assert!(matches!(i.verify().unwrap_err(), Error::BlockIdMismatch { .. }));

    let mut i = real();
    i.data_shreds[10][300] ^= 1;
    assert!(matches!(i.verify().unwrap_err(), Error::Shred(_) | Error::BlockIdMismatch { .. }));

    let mut i = real();
    i.validators[0].stake /= 2;
    i.validators.push(alpenglow_core::block_proof::Validator { bls_pubkey: i.validators[0].bls_pubkey.clone(), stake: i.validators[0].stake * 2 });
    // Bitmap has 1 bit but the set now has 2 entries: still decodes, but rank 0 holds 1/3.
    assert!(matches!(i.verify().unwrap_err(), Error::Finality(finality::Error::NotEnoughStake { .. })));
}

#[test]
fn synthetic_sets_verify() {
    let base = real();
    for n in [1, 7, 100] {
        let s = fixture::synthetic(&base, n);
        let out = s.verify().unwrap();
        assert_eq!(out.block_id, base.block_id);
        assert!(out.signed_stake * 100 >= out.total_stake * 80);
        // Same set, one bit flipped in the bitmap: rejected.
        let mut bad = s.clone();
        bad.final_aggregate.bitmap[3] ^= 1;
        assert!(bad.verify().is_err(), "n={n}");
    }
}
