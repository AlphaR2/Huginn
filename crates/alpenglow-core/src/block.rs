//! From data shreds back to the block's contents.
//!
//! A leader serializes the block as a sequence of **block components** and cuts each one
//! across consecutive data shreds; the last shred of a component carries the
//! DATA_COMPLETE flag. A component is either:
//! - an **entry batch**: `count u64 LE (> 0) | Entry * count`, where
//!   `Entry = num_hashes u64 | hash [32] | tx_count u64 | VersionedTransaction * tx_count`;
//! - a **block marker**: `0u64 | version u16 (=1) | variant u8 | len u16 | data[len]`.
//!   Variants: 0 footer, 1 header (parent slot + parent block_id), 2 update-parent,
//!   3 genesis certificate.
//!
//! Mirrors Agave v4.4.0 `entry/src/block_component.rs`, `entry/src/entry.rs` and
//! `ledger/src/shredder.rs::deshred`.

use crate::{
    shred::{Hash, Kind, Shred},
    tx::{self, Transaction},
};

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("data shreds are not consecutive at index {0}")]
    Gap(u32),
    #[error("shred {0}: data size out of range")]
    DataSize(u32),
    #[error("component does not end in a DATA_COMPLETE shred")]
    Unterminated,
    #[error("component bytes are malformed at offset {0}")]
    Malformed(usize),
    #[error("component has {0} trailing bytes")]
    Trailing(usize),
    #[error("unknown marker version {0} or variant {1}")]
    UnknownMarker(u16, u8),
    #[error(transparent)]
    Tx(#[from] tx::Error),
}

/// Where a transaction sits in the block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TxPosition {
    pub component: usize,
    pub entry: usize,
    pub tx: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Component<'a> {
    Entries(Vec<Entry<'a>>),
    Marker(Marker<'a>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry<'a> {
    pub num_hashes: u64,
    pub hash: Hash,
    pub transactions: Vec<Transaction<'a>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Marker<'a> {
    Footer(&'a [u8]),
    /// The block's parent, as the leader declared it. Also bound into the block_id.
    Header { parent_slot: u64, parent_block_id: Hash },
    UpdateParent { parent_slot: u64, parent_block_id: Hash },
    GenesisCertificate(&'a [u8]),
}

/// The entry/marker bytes a data shred carries: `[88 .. size)`, size from the header.
pub fn shred_data<'a>(s: &Shred<'a>) -> Result<&'a [u8], Error> {
    s.data().ok_or(Error::DataSize(s.index()))
}

/// Splits a run of consecutive data shreds into the byte strings of the components they
/// carry. The run must start at a component boundary (index 0, or right after a
/// DATA_COMPLETE shred) and end with a DATA_COMPLETE shred.
pub fn deshred(shreds: &[Shred]) -> Result<Vec<Vec<u8>>, Error> {
    let mut out = Vec::new();
    let mut current = Vec::new();
    let mut prev: Option<u32> = None;
    for s in shreds.iter().filter(|s| s.kind() == Kind::Data) {
        if prev.is_some_and(|p| p.checked_add(1) != Some(s.index())) {
            return Err(Error::Gap(s.index()));
        }
        prev = Some(s.index());
        current.extend_from_slice(shred_data(s)?);
        if s.data_complete() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        return Err(Error::Unterminated);
    }
    Ok(out)
}

/// Little cursor over component bytes; every read is bounds-checked.
pub(crate) struct Reader<'a> {
    pub bytes: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, pos: 0 }
    }
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.bytes.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }
    pub fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    pub fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }
    pub fn u64(&mut self) -> Option<u64> {
        self.take(8).map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    }
    pub fn hash(&mut self) -> Option<Hash> {
        self.take(32).map(|b| b.try_into().unwrap())
    }
    /// Solana's compact-u16 ("short_vec") length: 1-3 bytes, 7 bits each, canonical only.
    pub fn compact_u16(&mut self) -> Option<usize> {
        let mut value = 0usize;
        for i in 0..3 {
            let b = self.u8()?;
            value |= ((b & 0x7f) as usize) << (7 * i);
            if b & 0x80 == 0 {
                // Reject non-minimal encodings (a zero continuation byte) and overflow.
                if (i > 0 && b == 0) || value > u16::MAX as usize {
                    return None;
                }
                return Some(value);
            }
        }
        None
    }
    pub fn rest(&self) -> usize {
        self.bytes.len() - self.pos
    }
}

/// Parses one component. The whole byte string must be consumed.
pub fn parse_component(bytes: &[u8]) -> Result<Component<'_>, Error> {
    let mut r = Reader::new(bytes);
    let bad = |r: &Reader| Error::Malformed(r.pos);
    let count = r.u64().ok_or_else(|| bad(&r))?;
    let component = if count == 0 {
        let version = r.u16().ok_or_else(|| bad(&r))?;
        let variant = r.u8().ok_or_else(|| bad(&r))?;
        let len = r.u16().ok_or_else(|| bad(&r))? as usize;
        let data = r.take(len).ok_or_else(|| bad(&r))?;
        Component::Marker(parse_marker(version, variant, data)?)
    } else {
        // Each entry is at least 48 bytes, so a count can't exceed what's left.
        if count > (r.rest() / 48) as u64 {
            return Err(bad(&r));
        }
        let mut entries = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let num_hashes = r.u64().ok_or_else(|| bad(&r))?;
            let hash = r.hash().ok_or_else(|| bad(&r))?;
            let n = r.u64().ok_or_else(|| bad(&r))?;
            if n > r.rest() as u64 {
                return Err(bad(&r));
            }
            let mut transactions = Vec::with_capacity(n as usize);
            for _ in 0..n {
                transactions.push(tx::parse(&mut r)?);
            }
            entries.push(Entry { num_hashes, hash, transactions });
        }
        Component::Entries(entries)
    };
    if r.rest() != 0 {
        return Err(Error::Trailing(r.rest()));
    }
    Ok(component)
}

fn parse_marker(version: u16, variant: u8, data: &[u8]) -> Result<Marker<'_>, Error> {
    let parent = |data: &[u8]| -> Result<(u64, Hash), Error> {
        // Versioned: u8 tag (=1), then parent_slot u64 | parent_block_id [32].
        let mut r = Reader::new(data);
        let (Some(1), Some(slot), Some(id)) = (r.u8(), r.u64(), r.hash()) else {
            return Err(Error::Malformed(0));
        };
        if r.rest() != 0 {
            return Err(Error::Trailing(r.rest()));
        }
        Ok((slot, id))
    };
    match (version, variant) {
        (1, 0) => Ok(Marker::Footer(data)),
        (1, 1) => parent(data).map(|(parent_slot, parent_block_id)| Marker::Header { parent_slot, parent_block_id }),
        (1, 2) => parent(data).map(|(parent_slot, parent_block_id)| Marker::UpdateParent { parent_slot, parent_block_id }),
        (1, 3) => Ok(Marker::GenesisCertificate(data)),
        _ => Err(Error::UnknownMarker(version, variant)),
    }
}

/// Finds a transaction by its first signature (the transaction id).
pub fn find_transaction<'a>(components: &'a [Component<'a>], signature: &[u8; 64]) -> Option<(TxPosition, &'a Transaction<'a>)> {
    for (c, comp) in components.iter().enumerate() {
        let Component::Entries(entries) = comp else { continue };
        for (e, entry) in entries.iter().enumerate() {
            for (t, tx) in entry.transactions.iter().enumerate() {
                if tx.signatures.first() == Some(signature) {
                    return Some((TxPosition { component: c, entry: e, tx: t }, tx));
                }
            }
        }
    }
    None
}
