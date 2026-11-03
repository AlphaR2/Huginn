//! The whole "this block is final, and these are its shreds" check, as one function the
//! SP1 program calls. Native tests call the same function, so both sides run identical code.
//!
//! Input: the epoch's validator set, a footer finality certificate, and the block's data
//! shreds plus its parent. Output: what a destination chain needs to trust the block.
//!
//! Provisional (item 0.10). The set commitment and the output layout get settled in
//! items 1.1 and 4.1.

use {
    crate::{
        finality::{self, CertType},
        shred::{self, Hash, Shred},
    },
    serde::{Deserialize, Serialize},
    sha2::{Digest, Sha256},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Validator {
    /// 48-byte compressed BLS key (Vec, as serde has no `[u8; 48]`).
    pub bls_pubkey: Vec<u8>,
    pub stake: u64,
}

/// A compressed (96-byte) aggregate signature and the signer bitmap it covers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Aggregate {
    pub signature: Vec<u8>,
    pub bitmap: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockProofInput {
    /// Ranked set of the certificate's epoch (rank = index).
    pub validators: Vec<Validator>,
    pub shred_version: u16,
    /// The footer cert: FinalizeFast when `notar` is None, else Notarize + Finalize.
    pub slot: u64,
    pub block_id: Hash,
    pub final_aggregate: Aggregate,
    pub notar_aggregate: Option<Aggregate>,
    /// The block itself. The parent goes into the block_id's last leaf, so a matching
    /// block_id also proves the parent is right.
    pub parent_slot: u64,
    pub parent_block_id: Hash,
    pub data_shreds: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockProofOutput {
    /// sha256(count u32 LE | for each validator: bls_pubkey 48 | stake u64 LE).
    pub validator_set_hash: Hash,
    pub shred_version: u16,
    pub slot: u64,
    pub block_id: Hash,
    /// Signed stake of the finalizing cert, and the set's total.
    pub signed_stake: u64,
    pub total_stake: u64,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("validator {0}: key must be 48 bytes")]
    KeyLength(usize),
    #[error("signature must be 96 bytes")]
    SignatureLength,
    #[error(transparent)]
    Finality(#[from] finality::Error),
    #[error(transparent)]
    Shred(#[from] shred::Error),
    #[error("shreds rebuild block_id {computed:02x?}, cert signed {certified:02x?}")]
    BlockIdMismatch { computed: Hash, certified: Hash },
    #[error("shred has version {0}, certificate uses {1}")]
    ShredVersion(u16, u16),
}

pub fn validator_set_hash(validators: &[Validator]) -> Hash {
    let mut h = Sha256::new();
    h.update((validators.len() as u32).to_le_bytes());
    for v in validators {
        h.update(&v.bls_pubkey);
        h.update(v.stake.to_le_bytes());
    }
    h.finalize().into()
}

fn signature(a: &Aggregate) -> Result<bls12_381::G2Affine, Error> {
    let bytes: &[u8; 96] = a.signature.as_slice().try_into().map_err(|_| Error::SignatureLength)?;
    Ok(finality::signature_from_compressed(bytes)?)
}

impl BlockProofInput {
    /// Finality and inclusion together. On success, the block is final and the shreds are
    /// exactly its contents.
    pub fn verify(&self) -> Result<BlockProofOutput, Error> {
        let (signed_stake, total_stake) = self.verify_finality()?;
        self.verify_inclusion()?;
        Ok(BlockProofOutput {
            validator_set_hash: validator_set_hash(&self.validators),
            shred_version: self.shred_version,
            slot: self.slot,
            block_id: self.block_id,
            signed_stake,
            total_stake,
        })
    }

    /// The certificate proves `block_id` final at `slot`. Returns (signed, total) stake.
    pub fn verify_finality(&self) -> Result<(u64, u64), Error> {
        if let Some(i) = self.validators.iter().position(|v| v.bls_pubkey.len() != 48) {
            return Err(Error::KeyLength(i));
        }
        let total_stake: u64 = self.validators.iter().map(|v| v.stake).sum();
        // Keys are decompressed only for ranks that signed: decompression is one of the
        // costliest steps in the zkVM. A key that fails to decode is reported as a missing
        // rank; Agave would never have ranked it (validator_set drops invalid keys).
        let rank_map = |r: usize| {
            let v = self.validators.get(r)?;
            Some((v.stake, finality::pubkey_from_compressed(v.bls_pubkey.as_slice().try_into().ok()?).ok()?))
        };
        let check = |t: CertType, a: &Aggregate| {
            finality::verify_cert(&t, self.shred_version, &signature(a)?, &a.bitmap, self.validators.len(), total_stake, rank_map)
                .map_err(Error::from)
        };
        // Fast path, or slow path (the Notarize part names the block).
        let (slot, block_id) = (self.slot, self.block_id);
        let signed = match &self.notar_aggregate {
            None => check(CertType::FinalizeFast { slot, block_id }, &self.final_aggregate)?,
            Some(notar) => {
                check(CertType::Notarize { slot, block_id }, notar)?;
                check(CertType::Finalize { slot }, &self.final_aggregate)?
            }
        };
        Ok((signed, total_stake))
    }

    /// The data shreds rebuild exactly the certified `block_id`.
    pub fn verify_inclusion(&self) -> Result<(), Error> {
        let shreds = self.data_shreds.iter().map(|b| Shred::parse(b)).collect::<Result<Vec<_>, _>>()?;
        if let Some(s) = shreds.iter().find(|s| s.version() != self.shred_version) {
            return Err(Error::ShredVersion(s.version(), self.shred_version));
        }
        let roots = shred::fec_roots(self.slot, &shreds)?;
        let computed = shred::block_id(&roots, self.parent_slot, &self.parent_block_id)?;
        if computed != self.block_id {
            return Err(Error::BlockIdMismatch { computed, certified: self.block_id });
        }
        Ok(())
    }
}
