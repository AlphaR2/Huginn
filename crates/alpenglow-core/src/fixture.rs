//! Builds proof inputs from `ledger-dump` block fixtures, or synthetic ones with many
//! validators (the local chain has only one). Feature `fixture`; host side only, never in
//! the zkVM.

use {
    crate::{
        finality::{hash_to_g2, CertType},
        proof::{Aggregate, BlockInput, FinalityInput, TxInclusion, TxInput, Validator},
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
pub fn block_input(f: &Value) -> BlockInput {
    let cert = &f["block_final_cert"];
    BlockInput {
        finality: FinalityInput {
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
        },
        parent_slot: f["parent_slot"].as_u64().expect("parent_slot"),
        parent_block_id: hash(&f["parent_block_final_cert"]["block_id"]),
        data_shreds: f["data_shreds"].as_array().expect("data_shreds").iter().map(bytes).collect(),
    }
}

/// Finality for the fixture's block plus the smallest inclusion of transaction `signature`.
pub fn tx_input(f: &Value, signature: [u8; 64]) -> TxInput {
    let b = block_input(f);
    let inclusion = TxInclusion::from_block(&b.data_shreds, b.parent_slot, b.parent_block_id, signature).expect("tx in block");
    TxInput { finality: b.finality, inclusion }
}

fn base2(ranks: &[usize], bits: usize) -> Vec<u8> {
    let mut out = vec![0u8, bits as u8, (bits >> 8) as u8];
    out.resize(3 + bits.div_ceil(8), 0);
    for &r in ranks {
        out[3 + r / 8] |= 1 << (r % 8);
    }
    out
}

/// `n` deterministic test validators, ranked (stake descending). Secret keys are `i + 2`:
/// test material, never real keys.
pub fn synthetic_set(n: usize) -> (Vec<Scalar>, Vec<Validator>) {
    assert!((1..=u16::MAX as usize).contains(&n));
    let sks: Vec<Scalar> = (0..n as u64).map(|i| Scalar::from(i + 2)).collect();
    let validators = sks
        .iter()
        .enumerate()
        .map(|(i, sk)| Validator {
            bls_pubkey: G1Affine::from(G1Projective::generator() * sk).to_compressed().to_vec(),
            stake: 1_000_000 + ((n - i) as u64) * 1_000,
        })
        .collect();
    (sks, validators)
}

/// A real aggregate over `signers`: H(m) * sum(sk), which equals the sum of their signatures.
pub fn sign(sks: &[Scalar], signers: &[usize], cert: CertType, shred_version: u16) -> Aggregate {
    let sk_sum: Scalar = signers.iter().map(|&i| sks[i]).sum();
    let sig = G2Affine::from(hash_to_g2(&cert.payload(shred_version)) * sk_sum);
    Aggregate { signature: sig.to_compressed().to_vec(), bitmap: base2(signers, sks.len()) }
}

/// ~90% of `n` validators: comfortably over every threshold.
pub fn most(n: usize) -> Vec<usize> {
    (0..n).filter(|i| (i * 7919) % 10 != 3 || n == 1).collect()
}

/// The same block, finalized instead by `n` synthetic validators on the fast path.
pub fn synthetic(base: &FinalityInput, n: usize) -> FinalityInput {
    let (sks, validators) = synthetic_set(n);
    let cert = CertType::FinalizeFast { slot: base.slot, block_id: base.block_id };
    FinalityInput {
        final_aggregate: sign(&sks, &most(n), cert, base.shred_version),
        notar_aggregate: None,
        validators,
        ..base.clone()
    }
}

/// The same block, finalized by `n` synthetic validators on the slow path.
pub fn synthetic_slow(base: &FinalityInput, n: usize) -> FinalityInput {
    let (sks, validators) = synthetic_set(n);
    let signers = most(n);
    let notar = CertType::Notarize { slot: base.slot, block_id: base.block_id };
    FinalityInput {
        final_aggregate: sign(&sks, &signers, CertType::Finalize { slot: base.slot }, base.shred_version),
        notar_aggregate: Some(sign(&sks, &signers, notar, base.shred_version)),
        validators,
        ..base.clone()
    }
}
