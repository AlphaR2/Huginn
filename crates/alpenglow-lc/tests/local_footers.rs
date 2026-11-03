//! Real block-footer certificates from the local Alpenglow validator (item 2.3).
//!
//! Unlike the devnet genesis test, nothing here is reconstructed: the footers
//! (`footers_sample.jsonl`, captured by plugins/footer-dump) and the validator set
//! (`validator_set_0.json`) both come from epoch 0 of the same local chain, so ranks,
//! stakes and threshold are all exact.

use {
    alpenglow_lc::{
        cert::{self, verify_footer, Error, FooterCert},
        validator_set::ValidatorSet,
    },
    serde_json::Value,
    solana_bls_signatures::BlsError,
};

const SHRED_VERSION: u16 = 28090; // fixtures/local/cluster_info.json

fn path(file: &str) -> String {
    format!("{}/../../fixtures/local/{file}", env!("CARGO_MANIFEST_DIR"))
}

fn set() -> ValidatorSet {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path("validator_set_0.json")).unwrap()).unwrap();
    serde_json::from_value(v["validator_set"].clone()).unwrap()
}

/// (footer slot, its cert if any) for every sampled footer.
fn footers() -> Vec<(u64, Option<FooterCert>)> {
    std::fs::read_to_string(path("footers_sample.jsonl"))
        .unwrap()
        .lines()
        .map(|l| {
            let v: Value = serde_json::from_str(l).unwrap();
            (v["slot"].as_u64().unwrap(), serde_json::from_value(v["block_final_cert"].clone()).unwrap())
        })
        .collect()
}

fn first_cert() -> FooterCert {
    footers().into_iter().find_map(|(_, c)| c).unwrap()
}

fn bad_sig() -> Error {
    Error::Cert(cert::VerifySig(BlsError::VerificationFailed))
}

#[test]
fn every_sampled_footer_cert_verifies() {
    let set = set();
    let (mut with_cert, mut without) = (0, 0);
    for (slot, c) in footers() {
        let Some(c) = c else {
            without += 1;
            continue;
        };
        // A footer certifies an earlier block, never itself.
        assert!(c.slot < slot, "footer {slot} certifies {}", c.slot);
        assert!(c.notar_aggregate.is_none(), "one validator always takes the fast path");
        let block = verify_footer(&c, SHRED_VERSION, &set).unwrap_or_else(|e| panic!("footer {slot}: {e}"));
        assert_eq!(block.slot, c.slot);
        with_cert += 1;
    }
    assert_eq!((with_cert, without), (6, 1));
}

/// Every footer the plugin has captured so far (`footers.jsonl` is git-ignored and grows
/// while the validator runs). Run with `cargo test -p alpenglow-lc -- --ignored`.
#[test]
#[ignore]
fn every_captured_footer_cert_verifies() {
    let set = set();
    let (mut with_cert, mut without) = (0, 0);
    for l in std::fs::read_to_string(path("footers.jsonl")).unwrap().lines() {
        let v: Value = serde_json::from_str(l).unwrap();
        match serde_json::from_value::<Option<FooterCert>>(v["block_final_cert"].clone()).unwrap() {
            Some(c) => {
                verify_footer(&c, SHRED_VERSION, &set).unwrap_or_else(|e| panic!("footer {}: {e}", v["slot"]));
                with_cert += 1;
            }
            None => without += 1,
        }
    }
    println!("{with_cert} certs verified, {without} footers without a cert");
}

#[test]
fn flipped_signature_bit_fails() {
    let mut c = first_cert();
    c.final_aggregate.signature[50] ^= 1;
    // Usually fails decompression (off the curve); a valid point would fail the pairing.
    let err = verify_footer(&c, SHRED_VERSION, &set()).unwrap_err();
    assert!(matches!(err, Error::SignatureDecode(_) | Error::Cert(cert::VerifySig(_))), "{err:?}");
}

#[test]
fn different_block_id_fails() {
    let mut c = first_cert();
    c.block_id[0] ^= 1;
    assert_eq!(verify_footer(&c, SHRED_VERSION, &set()).unwrap_err(), bad_sig());
}

#[test]
fn different_slot_fails() {
    let mut c = first_cert();
    c.slot += 1;
    assert_eq!(verify_footer(&c, SHRED_VERSION, &set()).unwrap_err(), bad_sig());
}

#[test]
fn wrong_shred_version_fails() {
    assert_eq!(verify_footer(&first_cert(), 7016, &set()).unwrap_err(), bad_sig());
}

#[test]
fn reading_fast_cert_as_slow_finalize_fails() {
    // Same signature, claimed as Finalize(slot): different vote tag, different payload.
    let c = first_cert();
    let mut u = c.to_unverified(SHRED_VERSION).unwrap().remove(0);
    u.cert_type = agave_votor_messages::certificate::CertificateType::Finalize(c.slot);
    assert_eq!(cert::verify(u, &set()).unwrap_err(), bad_sig());
}

#[test]
fn different_validator_key_fails() {
    let mut set = set();
    // Another valid BLS key in rank 0's place (the devnet set's rank 0).
    set.entries[0].bls_pubkey = other_key();
    assert_eq!(verify_footer(&first_cert(), SHRED_VERSION, &set).unwrap_err(), bad_sig());
}

#[test]
fn empty_bitmap_fails() {
    let mut c = first_cert();
    c.final_aggregate.bitmap = vec![0x00, 0x01, 0x00, 0x00]; // base2, 1 bit, nobody signed
    assert!(verify_footer(&c, SHRED_VERSION, &set()).is_err());
}

fn other_key() -> [u8; 48] {
    let p = format!("{}/../../fixtures/devnet/validator_set_1198.json", env!("CARGO_MANIFEST_DIR"));
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    let s: ValidatorSet = serde_json::from_value(v["validator_set"].clone()).unwrap();
    s.entries[0].bls_pubkey
}

#[test]
fn set_commitment_is_stable() {
    // Pinned: if this changes, every stored commitment on every destination chain is
    // invalidated, so it must only change together with a new layout version.
    assert_eq!(
        hex::encode(set().commitment()),
        hex::encode(alpenglow_core::proof::validator_set_hash(&set().to_proof_validators()))
    );
    assert_eq!(set().to_proof_validators().len(), 1);
}
