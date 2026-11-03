//! Builds a [`BlockProofInput`] from a `ledger-dump` block fixture, or a synthetic one with
//! many validators (the local chain has only one). Feature `fixture`; not for the zkVM.

use {
    crate::{
        block_proof::{Aggregate, BlockProofInput, Validator},
        finality::{hash_to_g2, CertType},
        shred::Hash,
    },
    bls12_381::{G1Affine, G1Projective, G2Affine, Scalar},
    serde_json::Value,
};

fn bytes(v: &Value) -> Vec<u8> {
    hex::decode(v.as_str().expect("hex string")).expect("hex")
}

fn hash(v: &Value) -> Hash {
    bytes(v).try_into().expect("32 bytes")
}

fn aggregate(v: &Value) -> Aggregate {
    Aggregate { signature: bytes(&v["signature"]), bitmap: bytes(&v["bitmap"]) }
}

/// From `fixtures/local/block_<slot>.json`.
pub fn from_block_fixture(f: &Value) -> BlockProofInput {
    let cert = &f["block_final_cert"];
    BlockProofInput {
        validators: f["validator_set"]["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|e| Validator { bls_pubkey: bytes(&e["bls_pubkey"]), stake: e["stake"].as_u64().expect("stake") })
            .collect(),
        shred_version: f["shred_version"].as_u64().expect("shred_version") as u16,
        slot: cert["slot"].as_u64().expect("slot"),
        block_id: hash(&cert["block_id"]),
        final_aggregate: aggregate(&cert["final_aggregate"]),
        notar_aggregate: (!cert["notar_aggregate"].is_null()).then(|| aggregate(&cert["notar_aggregate"])),
        parent_slot: f["parent_slot"].as_u64().expect("parent_slot"),
        parent_block_id: hash(&f["parent_block_final_cert"]["block_id"]),
        data_shreds: f["data_shreds"].as_array().expect("data_shreds").iter().map(bytes).collect(),
    }
}

fn base2(ranks: &[usize], bits: usize) -> Vec<u8> {
    let mut out = vec![0u8, bits as u8, (bits >> 8) as u8];
    out.resize(3 + bits.div_ceil(8), 0);
    for &r in ranks {
        out[3 + r / 8] |= 1 << (r % 8);
    }
    out
}

/// The same block, finalized instead by `n` synthetic validators, ~90% of whom sign
/// (comfortably over the 80% fast-path threshold). Fast path only.
///
/// Secret keys are deterministic (`i + 2`): test material, never real keys. The aggregate
/// signature is H(m) * sum(sk), which equals the sum of the individual signatures.
pub fn synthetic(base: &BlockProofInput, n: usize) -> BlockProofInput {
    assert!((1..=u16::MAX as usize).contains(&n));
    let sks: Vec<Scalar> = (0..n as u64).map(|i| Scalar::from(i + 2)).collect();
    // Ranked: stake descending. A spread of stakes, like a real set.
    let validators: Vec<Validator> = sks
        .iter()
        .enumerate()
        .map(|(i, sk)| Validator {
            bls_pubkey: G1Affine::from(G1Projective::generator() * sk).to_compressed().to_vec(),
            stake: 1_000_000 + ((n - i) as u64) * 1_000,
        })
        .collect();
    let signers: Vec<usize> = (0..n).filter(|i| (i * 7919) % 10 != 3 || n == 1).collect();
    let total: u64 = validators.iter().map(|v| v.stake).sum();
    let signed: u64 = signers.iter().map(|&i| validators[i].stake).sum();
    assert!(signed * 100 >= total * 80, "synthetic signers below 80%");

    let payload = CertType::FinalizeFast { slot: base.slot, block_id: base.block_id }.payload(base.shred_version);
    let sk_sum: Scalar = signers.iter().map(|&i| sks[i]).sum();
    let sig = G2Affine::from(hash_to_g2(&payload) * sk_sum);
    BlockProofInput {
        validators,
        final_aggregate: Aggregate { signature: sig.to_compressed().to_vec(), bitmap: base2(&signers, n) },
        notar_aggregate: None,
        ..base.clone()
    }
}
