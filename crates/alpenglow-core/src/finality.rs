//! Alpenglow certificate verification on `bls12_381` (zkcrypto), the curve library SP1
//! accelerates.
//!
//! Mirrors Agave v4.4.0 `agave_bls_cert_verify::verify_certificate`, step for step:
//! 1. Decode the signer bitmap (base2 only; the cert types we accept never use base3).
//! 2. Look up every signer's key and stake by rank; a missing rank fails.
//! 3. Sum the keys, then check `e(agg_pk, H(payload)) == e(g1, sig)`.
//! 4. Check `signed_stake / total_stake >= threshold`, by cross-multiplication.
//!
//! `alpenglow-lc` keeps Agave's own verifier and cross-checks this one against it.

use {
    crate::shred::Hash,
    bls12_381::{
        hash_to_curve::{ExpandMsgXmd, HashToCurve},
        multi_miller_loop, G1Affine, G1Projective, G2Affine, G2Prepared, G2Projective, Gt,
    },
};

/// Hash-to-curve domain separation tag (Agave `solana-bls-signatures` `HASH_TO_POINT_DST`).
pub const DST: &[u8] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("bitmap is malformed")]
    Decode,
    #[error("bitmap uses base3, which this certificate type never does")]
    WrongEncoding,
    #[error("bitmap names rank {0}, which is not in the set")]
    MissingRank(usize),
    #[error("no signers")]
    NoSigners,
    #[error("signature bytes are not a valid G2 point")]
    SignatureEncoding,
    #[error("public key bytes are not a valid G1 point")]
    PubkeyEncoding,
    #[error("signature does not verify")]
    VerifySig,
    #[error("signed stake {signed} of {total} is below {numerator}/{denominator}")]
    NotEnoughStake {
        signed: u64,
        total: u64,
        numerator: u64,
        denominator: u64,
    },
}

/// The certificate types a finality proof needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CertType {
    /// 60% notarized this block.
    Notarize { slot: u64, block_id: Hash },
    /// 80% notarized this block: final on its own.
    FinalizeFast { slot: u64, block_id: Hash },
    /// 60% finalized this slot. Names no block, so it needs a Notarize next to it.
    Finalize { slot: u64 },
    /// 82% voted for the migration genesis block.
    Genesis { slot: u64, block_id: Hash },
}

impl CertType {
    /// The bytes every signer signed: Agave `VotePayloadToSign`, wincode-encoded as
    /// `tag u8 | slot u64 LE | [block_id] | shred_version u16 LE`.
    /// FinalizeFast is made of ordinary notarization votes, so it signs the Notar payload.
    pub fn payload(&self, shred_version: u16) -> Vec<u8> {
        let (tag, slot, block_id) = match *self {
            CertType::Notarize { slot, block_id } | CertType::FinalizeFast { slot, block_id } => {
                (1, slot, Some(block_id))
            }
            CertType::Finalize { slot } => (2, slot, None),
            CertType::Genesis { slot, block_id } => (6, slot, Some(block_id)),
        };
        let mut out = Vec::with_capacity(43);
        out.push(tag);
        out.extend_from_slice(&slot.to_le_bytes());
        if let Some(id) = block_id {
            out.extend_from_slice(&id);
        }
        out.extend_from_slice(&shred_version.to_le_bytes());
        out
    }

    /// Required share of total stake, as (numerator, denominator).
    pub fn threshold(&self) -> (u64, u64) {
        match self {
            CertType::Notarize { .. } | CertType::Finalize { .. } => (60, 100),
            CertType::FinalizeFast { .. } => (80, 100),
            CertType::Genesis { .. } => (82, 100),
        }
    }
}

/// Signer ranks from a `solana-signer-store` bitmap: `version u8 | bits u16 LE | payload`,
/// LSB first. Version 0 (base2) only. Bits past `bits` in the last byte are ignored, as
/// Agave's decoder truncates them.
pub fn decode_bitmap(bytes: &[u8], max_validators: usize) -> Result<Vec<usize>, Error> {
    let [version, lo, hi, payload @ ..] = bytes else {
        return Err(Error::Decode);
    };
    let bits = u16::from_le_bytes([*lo, *hi]) as usize;
    match version {
        0 => {}
        1 => return Err(Error::WrongEncoding),
        _ => return Err(Error::Decode),
    }
    if bits > max_validators || payload.len() != bits.div_ceil(8) {
        return Err(Error::Decode);
    }
    Ok((0..bits)
        .filter(|&i| payload[i / 8] >> (i % 8) & 1 == 1)
        .collect())
}

/// A 96-byte compressed G2 signature (block footers). Includes the subgroup check.
pub fn signature_from_compressed(bytes: &[u8; 96]) -> Result<G2Affine, Error> {
    Option::from(G2Affine::from_compressed(bytes)).ok_or(Error::SignatureEncoding)
}

/// A 192-byte uncompressed G2 signature (`getAgGenesisCert`). Includes the subgroup check.
pub fn signature_from_uncompressed(bytes: &[u8; 192]) -> Result<G2Affine, Error> {
    Option::from(G2Affine::from_uncompressed(bytes)).ok_or(Error::SignatureEncoding)
}

/// A 48-byte compressed G1 public key. Agave rejects the identity key when ranking.
pub fn pubkey_from_compressed(bytes: &[u8; 48]) -> Result<G1Affine, Error> {
    let p: G1Affine =
        Option::from(G1Affine::from_compressed(bytes)).ok_or(Error::PubkeyEncoding)?;
    if bool::from(p.is_identity()) {
        return Err(Error::PubkeyEncoding);
    }
    Ok(p)
}

/// The message point: hash_to_curve(msg) in G2 with SHA-256 XMD and [`DST`].
pub fn hash_to_g2(msg: &[u8]) -> G2Affine {
    G2Affine::from(
        <G2Projective as HashToCurve<ExpandMsgXmd<sha2::Sha256>>>::hash_to_curve([msg], DST),
    )
}

/// `e(pk, H(msg)) == e(g1, sig)`, as one multi-Miller loop and one final exponentiation.
pub fn verify_signature(pubkey: &G1Affine, msg: &[u8], sig: &G2Affine) -> bool {
    if bool::from(pubkey.is_identity()) {
        return false;
    }
    let h = hash_to_g2(msg);
    let lhs = multi_miller_loop(&[
        (pubkey, &G2Prepared::from(h)),
        (&-G1Affine::generator(), &G2Prepared::from(*sig)),
    ]);
    lhs.final_exponentiation() == Gt::identity()
}

/// Verifies one certificate. `rank_map(rank)` gives that validator's stake and key.
/// Returns the signed stake.
pub fn verify_cert(
    cert: &CertType,
    shred_version: u16,
    signature: &G2Affine,
    bitmap: &[u8],
    max_validators: usize,
    total_stake: u64,
    mut rank_map: impl FnMut(usize) -> Option<(u64, G1Affine)>,
) -> Result<u64, Error> {
    let ranks = decode_bitmap(bitmap, max_validators)?;
    if ranks.is_empty() {
        return Err(Error::NoSigners);
    }
    let mut signed = 0u64;
    let mut agg = G1Projective::identity();
    for r in ranks {
        let (stake, key) = rank_map(r).ok_or(Error::MissingRank(r))?;
        signed = signed.saturating_add(stake);
        agg += key;
    }
    if !verify_signature(
        &G1Affine::from(agg),
        &cert.payload(shred_version),
        signature,
    ) {
        return Err(Error::VerifySig);
    }
    let (numerator, denominator) = cert.threshold();
    if (signed as u128) * (denominator as u128) < (numerator as u128) * (total_stake as u128) {
        return Err(Error::NotEnoughStake {
            signed,
            total: total_stake,
            numerator,
            denominator,
        });
    }
    Ok(signed)
}
