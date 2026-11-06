//! Merkle shreds and the Alpenglow block_id.
//!
//! A block is cut into FEC sets of 32 data shreds plus coding shreds. Each FEC set has
//! its own Merkle tree over its shreds, and every shred carries a proof up to that root.
//! For Alpenglow blocks the block_id that votes sign is a second ("double") Merkle root:
//!
//! ```text
//! leaves = [fec_root_0, ..., fec_root_{n-1}, sha256(parent_slot LE | parent_block_id | n as u32 LE)]
//! block_id = merkle_root(leaves)
//! ```
//!
//! Reimplemented from Agave v4.4.0 (`ledger/src/shred/{merkle.rs,merkle_tree.rs,wire.rs}`,
//! `blockstore.rs::build_double_merkle_tree`) so it needs nothing beyond SHA-256.

use sha2::{Digest, Sha256};

pub type Hash = [u8; 32];

pub const DATA_SHREDS_PER_FEC_SET: usize = 32;

const SIZE_OF_SIGNATURE: usize = 64;
const SIZE_OF_MERKLE_ROOT: usize = 32;
const SIZE_OF_PROOF_ENTRY: usize = 20;
const DATA_PAYLOAD: usize = 1203;
const CODE_PAYLOAD: usize = 1228;
const DATA_HEADERS: usize = 88;
const CODE_HEADERS: usize = 89;
const LAST_SHRED_IN_SLOT: u8 = 0b1100_0000;
const DATA_COMPLETE: u8 = 0b0100_0000;

// Domain separation, so a leaf can't be passed off as an inner node (second preimage).
const LEAF_PREFIX: &[u8] = b"\x00SOLANA_MERKLE_SHREDS_LEAF";
const NODE_PREFIX: &[u8] = b"\x01SOLANA_MERKLE_SHREDS_NODE";

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("shred too short or wrong size for its variant")]
    Size,
    #[error("unknown shred variant {0:#04x}")]
    Variant(u8),
    #[error("Merkle proof index out of range")]
    Proof,
    #[error("no shreds")]
    Empty,
    #[error("shred from slot {got}, expected {expected}")]
    WrongSlot { expected: u64, got: u64 },
    #[error("data shred {0} missing")]
    Missing(u32),
    #[error("data shred {0} is a duplicate")]
    Duplicate(u32),
    #[error("no shred is flagged last-in-slot")]
    Incomplete,
    #[error("shreds of FEC set {0} disagree on its Merkle root")]
    FecRootMismatch(u32),
    #[error("FEC set {0} does not chain to the previous set's root")]
    ChainMismatch(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Data,
    Code,
}

/// A borrowed Merkle shred (data or coding), as stored in the ledger.
#[derive(Clone, Copy, Debug)]
pub struct Shred<'a> {
    bytes: &'a [u8],
    kind: Kind,
    proof_size: usize,
    resigned: bool,
}

fn sha256(parts: &[&[u8]]) -> Hash {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

/// Inner node. Only the first 20 bytes of each child are hashed, as in Agave.
fn join(a: &[u8], b: &[u8]) -> Hash {
    sha256(&[
        NODE_PREFIX,
        &a[..SIZE_OF_PROOF_ENTRY],
        &b[..SIZE_OF_PROOF_ENTRY],
    ])
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}

impl<'a> Shred<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let &variant = bytes.get(64).ok_or(Error::Size)?;
        let (kind, resigned) = match variant & 0xF0 {
            0x60 => (Kind::Code, false),
            0x70 => (Kind::Code, true),
            0x90 => (Kind::Data, false),
            0xb0 => (Kind::Data, true),
            _ => return Err(Error::Variant(variant)),
        };
        let shred = Shred {
            bytes,
            kind,
            proof_size: (variant & 0x0F) as usize,
            resigned,
        };
        // proof_size is at most 15, so the offsets can't underflow; the data buffer
        // (between headers and chained root) just has to be non-negative.
        if bytes.len() != shred.payload_size() || shred.chained_root_offset() < shred.headers() {
            return Err(Error::Size);
        }
        Ok(shred)
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn slot(&self) -> u64 {
        u64::from_le_bytes(self.bytes[65..73].try_into().unwrap())
    }

    pub fn index(&self) -> u32 {
        u32_at(self.bytes, 73)
    }

    /// The cluster's shred version, the same value every vote payload signs.
    pub fn version(&self) -> u16 {
        u16_at(self.bytes, 77)
    }

    pub fn fec_set_index(&self) -> u32 {
        u32_at(self.bytes, 79)
    }

    /// Data shreds only: the parent slot, from the header's `parent_offset`.
    pub fn parent_slot(&self) -> Option<u64> {
        let offset = u16_at(self.bytes, 83);
        (self.kind == Kind::Data)
            .then(|| self.slot().checked_sub(offset.into()))
            .flatten()
    }

    /// Data shreds only: this shred ends a block component (its bytes are complete).
    pub fn data_complete(&self) -> bool {
        self.kind == Kind::Data && self.bytes[85] & DATA_COMPLETE == DATA_COMPLETE
    }

    /// Data shreds only: the component bytes it carries, `[88 .. size)`, where `size`
    /// (header bytes 86..88) must stay within the data buffer.
    pub fn data(&self) -> Option<&'a [u8]> {
        if self.kind != Kind::Data {
            return None;
        }
        let size = u16_at(self.bytes, 86) as usize;
        (DATA_HEADERS..=self.chained_root_offset())
            .contains(&size)
            .then(|| &self.bytes[DATA_HEADERS..size])
    }

    pub fn last_in_slot(&self) -> bool {
        self.kind == Kind::Data && self.bytes[85] & LAST_SHRED_IN_SLOT == LAST_SHRED_IN_SLOT
    }

    fn payload_size(&self) -> usize {
        match self.kind {
            Kind::Data => DATA_PAYLOAD,
            Kind::Code => CODE_PAYLOAD,
        }
    }

    fn headers(&self) -> usize {
        match self.kind {
            Kind::Data => DATA_HEADERS,
            Kind::Code => CODE_HEADERS,
        }
    }

    fn proof_len(&self) -> usize {
        self.proof_size * SIZE_OF_PROOF_ENTRY
    }

    /// Start of the chained root; the proof follows it.
    fn chained_root_offset(&self) -> usize {
        let resigned = if self.resigned { SIZE_OF_SIGNATURE } else { 0 };
        // headers + capacity, where capacity = payload - headers - root - proof - resigned.
        self.payload_size() - SIZE_OF_MERKLE_ROOT - self.proof_len() - resigned
    }

    fn proof_offset(&self) -> usize {
        self.chained_root_offset() + SIZE_OF_MERKLE_ROOT
    }

    /// The root of the previous FEC set (or, for the first set, what the block chains to).
    pub fn chained_merkle_root(&self) -> Hash {
        let o = self.chained_root_offset();
        self.bytes[o..o + SIZE_OF_MERKLE_ROOT].try_into().unwrap()
    }

    /// Position among the FEC set's leaves: data shreds first, then coding shreds.
    pub fn erasure_index(&self) -> usize {
        match self.kind {
            Kind::Data => self.index().wrapping_sub(self.fec_set_index()) as usize,
            Kind::Code => u16_at(self.bytes, 83) as usize + u16_at(self.bytes, 87) as usize,
        }
    }

    /// Leaf hash: everything after the signature, up to (and including) the chained root.
    pub fn leaf(&self) -> Hash {
        sha256(&[
            LEAF_PREFIX,
            &self.bytes[SIZE_OF_SIGNATURE..self.proof_offset()],
        ])
    }

    pub fn proof(&self) -> impl Iterator<Item = &'a [u8]> {
        let o = self.proof_offset();
        self.bytes[o..o + self.proof_len()].chunks(SIZE_OF_PROOF_ENTRY)
    }

    /// The FEC set's Merkle root, from this shred's leaf and proof.
    pub fn merkle_root(&self) -> Result<Hash, Error> {
        root_from_proof(self.erasure_index(), self.leaf(), self.proof())
    }
}

/// Walks a proof from `leaf` at `index` up to the root.
pub fn root_from_proof<'p>(
    index: usize,
    leaf: Hash,
    proof: impl IntoIterator<Item = &'p [u8]>,
) -> Result<Hash, Error> {
    let (index, root) = proof.into_iter().fold((index, leaf), |(i, node), sibling| {
        let parent = if i % 2 == 0 {
            join(&node, sibling)
        } else {
            join(sibling, &node)
        };
        (i >> 1, parent)
    });
    if index == 0 {
        Ok(root)
    } else {
        Err(Error::Proof)
    }
}

/// Root of Agave's Merkle tree: pairs left to right, a last odd node is paired with itself.
pub fn merkle_root(leaves: &[Hash]) -> Result<Hash, Error> {
    let mut level = leaves.to_vec();
    if level.is_empty() {
        return Err(Error::Empty);
    }
    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|p| join(&p[0], p.last().unwrap()))
            .collect();
    }
    Ok(level[0])
}

/// The last leaf of the double Merkle tree. Binding `fec_set_count` stops a verifier being
/// fooled about how many FEC sets there are when an odd last leaf is paired with itself.
pub fn parent_leaf(parent_slot: u64, parent_block_id: &Hash, fec_set_count: u32) -> Hash {
    sha256(&[
        &parent_slot.to_le_bytes(),
        parent_block_id,
        &fec_set_count.to_le_bytes(),
    ])
}

/// Alpenglow block_id from the FEC set roots and the block's parent.
pub fn block_id(
    fec_roots: &[Hash],
    parent_slot: u64,
    parent_block_id: &Hash,
) -> Result<Hash, Error> {
    let mut leaves = fec_roots.to_vec();
    leaves.push(parent_leaf(
        parent_slot,
        parent_block_id,
        fec_roots.len() as u32,
    ));
    merkle_root(&leaves)
}

/// Checks a complete slot's data shreds and returns each FEC set's root, in order.
///
/// Requires shreds 0..=last with no gaps or duplicates, the last one flagged
/// last-in-slot, every shred in a set agreeing on that set's root, and each set chaining
/// to the previous set's root. Coding shreds aren't needed: any data shred's proof
/// already yields its set's root.
pub fn fec_roots(slot: u64, data_shreds: &[Shred]) -> Result<Vec<Hash>, Error> {
    let mut shreds: Vec<_> = data_shreds
        .iter()
        .filter(|s| s.kind == Kind::Data)
        .collect();
    shreds.sort_by_key(|s| s.index());
    let last = shreds.last().ok_or(Error::Empty)?;
    if !last.last_in_slot() {
        return Err(Error::Incomplete);
    }
    let mut roots: Vec<Hash> = Vec::new();
    for (i, s) in shreds.iter().enumerate() {
        if s.slot() != slot {
            return Err(Error::WrongSlot {
                expected: slot,
                got: s.slot(),
            });
        }
        if (s.index() as usize) < i {
            return Err(Error::Duplicate(s.index()));
        }
        if s.index() as usize != i {
            return Err(Error::Missing(i as u32));
        }
        let set = i / DATA_SHREDS_PER_FEC_SET;
        if s.fec_set_index() as usize != set * DATA_SHREDS_PER_FEC_SET {
            return Err(Error::FecRootMismatch(s.fec_set_index()));
        }
        let root = s.merkle_root()?;
        match roots.get(set) {
            Some(r) if *r != root => return Err(Error::FecRootMismatch(s.fec_set_index())),
            Some(_) => {}
            None => {
                if set > 0 && s.chained_merkle_root() != roots[set - 1] {
                    return Err(Error::ChainMismatch(s.fec_set_index()));
                }
                roots.push(root);
            }
        }
    }
    Ok(roots)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(b: u8) -> Hash {
        [b; 32]
    }

    #[test]
    fn proofs_agree_with_tree_root() {
        // For every tree size and leaf, the proof Agave would build walks back to the root.
        for n in 1..=13 {
            let leaves: Vec<Hash> = (0..n as u8).map(h).collect();
            let root = merkle_root(&leaves).unwrap();
            // Build each level, then collect siblings the way Agave's make_merkle_proof does.
            let mut levels = vec![leaves.clone()];
            while levels.last().unwrap().len() > 1 {
                let l = levels.last().unwrap();
                levels.push(
                    l.chunks(2)
                        .map(|p| join(&p[0], p.last().unwrap()))
                        .collect(),
                );
            }
            for (i, leaf) in leaves.iter().enumerate() {
                let mut idx = i;
                let mut proof = Vec::new();
                for l in &levels[..levels.len() - 1] {
                    proof.push(l[(idx ^ 1).min(l.len() - 1)][..20].to_vec());
                    idx >>= 1;
                }
                let got = root_from_proof(i, *leaf, proof.iter().map(|p| p.as_slice())).unwrap();
                assert_eq!(got, root, "n={n} i={i}");
            }
        }
    }

    #[test]
    fn block_id_binds_parent_and_count() {
        let roots = [h(1), h(2)];
        let id = block_id(&roots, 9, &h(9)).unwrap();
        assert_ne!(id, block_id(&roots, 8, &h(9)).unwrap());
        assert_ne!(id, block_id(&roots, 9, &h(8)).unwrap());
        assert_ne!(id, block_id(&[h(1), h(3)], 9, &h(9)).unwrap());
        assert_ne!(id, block_id(&[h(2), h(1)], 9, &h(9)).unwrap());
    }
}
