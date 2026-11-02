//! The real devnet genesis certificate through Agave's full `verify_certificate`.
//!
//! We can't fetch the epoch-1167 validator set (RPC only serves current stakes), so the
//! rank map is reconstructed:
//! - Signer identities: known from item 0.7 (`find_genesis_signers` found them among
//!   today's epoch-1198 keys).
//! - Signer positions: assumed in the same relative order as today. Signature checks are
//!   order-independent, so this choice can't make a bad signature pass.
//! - Stakes: today's, as a stand-in. So the stake-threshold result here is approximate;
//!   the signature result is exact.

use {
    alpenglow_lc::{
        cert::{self, Error, RpcGenesisCert},
        validator_set::ValidatorSet,
    },
    serde_json::Value,
    solana_bls_signatures::BlsError,
    std::num::NonZero,
};

const SHRED_VERSION: u16 = 7016; // fixtures/devnet/cluster_info.json

/// Bit positions set in the genesis bitmap (epoch 1167, 17 validators).
const GENESIS_RANKS: [usize; 11] = [0, 1, 2, 3, 4, 7, 9, 10, 14, 15, 16];
/// The same 11 validators' ranks in today's set (output of item 0.7).
const TODAY_RANKS: [usize; 11] = [0, 1, 2, 3, 4, 7, 10, 11, 17, 18, 19];

fn fixture(path: &str) -> Value {
    let p = format!("{}/../../fixtures/devnet/{path}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn load() -> (RpcGenesisCert, ValidatorSet) {
    let cert = serde_json::from_value(fixture("genesis_cert.json")["result"].clone()).unwrap();
    let set = serde_json::from_value(fixture("validator_set_1198.json")["validator_set"].clone()).unwrap();
    (cert, set)
}

/// Rank map for the reconstructed epoch-1167 set: genesis rank -> today's entry.
fn reconstructed(set: &ValidatorSet) -> impl FnMut(usize) -> Option<(NonZero<u64>, cert::PopKey)> + '_ {
    move |rank| {
        let i = GENESIS_RANKS.iter().position(|&r| r == rank)?;
        let e = &set.entries[TODAY_RANKS[i]];
        Some((NonZero::new(e.stake)?, e.pop_verified_key()?))
    }
}

fn verify(cert: &RpcGenesisCert, set: &ValidatorSet, shred: u16) -> Result<(), Error> {
    let total = NonZero::new(set.total_stake).unwrap();
    cert::verify_with(cert.to_unverified(shred)?, 17, total, reconstructed(set)).map(|_| ())
}

#[test]
fn real_genesis_cert_verifies() {
    let (c, set) = load();
    verify(&c, &set, SHRED_VERSION).unwrap();
}

#[test]
fn real_genesis_cert_with_wrong_shred_version_fails() {
    let (c, set) = load();
    assert_eq!(
        verify(&c, &set, SHRED_VERSION + 1).unwrap_err(),
        Error::Cert(cert::VerifySig(BlsError::VerificationFailed))
    );
}

#[test]
fn real_genesis_cert_with_one_bitmap_bit_flipped_fails() {
    let (mut c, set) = load();
    // Byte 3 holds ranks 0..8. Clear rank 0: drop the biggest signer from the claim.
    c.signature.bitmap[3] &= !1;
    assert_eq!(
        verify(&c, &set, SHRED_VERSION).unwrap_err(),
        Error::Cert(cert::VerifySig(BlsError::VerificationFailed))
    );
}

#[test]
fn real_genesis_cert_with_one_signature_bit_flipped_fails() {
    let (mut c, set) = load();
    c.signature.signature[150] ^= 1;
    assert!(matches!(verify(&c, &set, SHRED_VERSION).unwrap_err(), Error::Cert(cert::VerifySig(_))));
}

#[test]
fn real_genesis_cert_with_different_slot_fails() {
    let (mut c, set) = load();
    c.block.slot += 1;
    assert_eq!(
        verify(&c, &set, SHRED_VERSION).unwrap_err(),
        Error::Cert(cert::VerifySig(BlsError::VerificationFailed))
    );
}

/// Why the epoch's own ranking matters: today's ranks put the old bitmap's bits on the
/// wrong validators, and the signature no longer matches.
#[test]
fn real_genesis_cert_against_todays_ranking_fails() {
    let (c, set) = load();
    assert_eq!(
        cert::verify(c.to_unverified(SHRED_VERSION).unwrap(), &set).unwrap_err(),
        Error::Cert(cert::VerifySig(BlsError::VerificationFailed))
    );
}
