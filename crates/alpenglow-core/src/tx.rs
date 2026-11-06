//! Solana transaction wire format, legacy, v0 and v1, parsed without any Solana crates.
//!
//! The first byte says which: below 0x80 it is the signature count of a legacy or v0
//! transaction; 0x81 starts a v1 transaction; anything else is not a transaction.
//!
//! Legacy and v0:
//! ```text
//! transaction = signatures: short_vec<[u8; 64]> | message
//! message     = [0x80 | version]?            (absent = legacy; present = v0 when version 0)
//!               header: num_required_signatures u8 | num_readonly_signed u8 | num_readonly_unsigned u8
//!               account_keys: short_vec<[u8; 32]>
//!               recent_blockhash: [u8; 32]
//!               instructions: short_vec<{ program_id_index u8, accounts short_vec<u8>, data short_vec<u8> }>
//!               v0 only: address_table_lookups: short_vec<{ table [u8; 32], writable short_vec<u8>, readonly short_vec<u8> }>
//! ```
//!
//! v1 (SIMD-0385, live on mainnet since epoch 1035): the message comes first and the
//! signatures last, counts are single bytes, and there are no lookup tables.
//! ```text
//! transaction = message | signatures: [[u8; 64]; num_required_signatures]
//! message     = 0x81
//!               header: num_required_signatures u8 | num_readonly_signed u8 | num_readonly_unsigned u8
//!               config_mask u32 LE           (which config values follow)
//!               recent_blockhash: [u8; 32]
//!               num_instructions u8 | num_addresses u8
//!               account_keys: [[u8; 32]; num_addresses]
//!               config values: priority fee u64 (bits 0+1), compute unit limit u32 (bit 2),
//!                              loaded accounts data size u32 (bit 3), heap size u32 (bit 4)
//!               instruction headers: [{ program_id_index u8, num_accounts u8, data_len u16 LE }; num_instructions]
//!               then per instruction: accounts [u8; num_accounts] | data [u8; data_len]
//! ```
//! Mirrors `solana-message` 5.1.0 `versions/v1` and `solana-transaction` 5.1.0
//! `versioned` (`SchemaRead for VersionedTransaction`).
//!
//! `short_vec` is a compact-u16 length (1-3 bytes) followed by that many items.
//! The signatures sign exactly the `message` bytes, version byte included.

use crate::block::Reader;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("transaction is malformed at offset {0}")]
    Malformed(usize),
    #[error("unsupported message version {0}")]
    Version(u8),
    #[error("transaction is not well formed: {0}")]
    Invalid(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instruction<'a> {
    pub program_id_index: u8,
    pub accounts: &'a [u8],
    pub data: &'a [u8],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddressTableLookup<'a> {
    pub table: [u8; 32],
    pub writable: &'a [u8],
    pub readonly: &'a [u8],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transaction<'a> {
    /// The whole transaction, exactly as it appears in the block.
    pub raw: &'a [u8],
    /// The bytes the signatures sign.
    pub message: &'a [u8],
    pub signatures: Vec<[u8; 64]>,
    /// `None` for legacy, `Some(0)` for v0, `Some(1)` for v1.
    pub version: Option<u8>,
    pub num_required_signatures: u8,
    pub num_readonly_signed: u8,
    pub num_readonly_unsigned: u8,
    pub account_keys: Vec<[u8; 32]>,
    pub recent_blockhash: [u8; 32],
    pub instructions: Vec<Instruction<'a>>,
    pub address_table_lookups: Vec<AddressTableLookup<'a>>,
}

impl Transaction<'_> {
    /// The accounts that signed (the first `num_required_signatures` static keys).
    pub fn signers(&self) -> &[[u8; 32]] {
        &self.account_keys[..self.num_required_signatures as usize]
    }

    /// An instruction's program id, when it is a static key (always true for program ids:
    /// Solana forbids loading program ids through lookup tables).
    pub fn program_id(&self, ix: &Instruction) -> Option<&[u8; 32]> {
        self.account_keys.get(ix.program_id_index as usize)
    }
}

/// Parses one transaction from `r`, leaving `r` just past it.
pub(crate) fn parse<'a>(r: &mut Reader<'a>) -> Result<Transaction<'a>, Error> {
    let start = r.pos;
    let bad = |r: &Reader| Error::Malformed(r.pos);

    match *r.bytes.get(r.pos).ok_or_else(|| bad(r))? {
        V1_PREFIX => return parse_v1(r),
        // Agave reads a byte below 0x80 as the signature count; nothing else is valid.
        b if b & 0x80 != 0 => return Err(Error::Version(b & 0x7f)),
        _ => {}
    }
    let n = r.compact_u16().ok_or_else(|| bad(r))?;
    let mut signatures = Vec::with_capacity(n.min(r.rest() / 64));
    for _ in 0..n {
        signatures.push(r.take(64).ok_or_else(|| bad(r))?.try_into().unwrap());
    }

    let message_start = r.pos;
    let first = *r.bytes.get(r.pos).ok_or_else(|| bad(r))?;
    let version = if first & 0x80 != 0 {
        r.pos += 1;
        match first & 0x7f {
            0 => Some(0),
            v => return Err(Error::Version(v)),
        }
    } else {
        None
    };
    let (Some(num_required_signatures), Some(num_readonly_signed), Some(num_readonly_unsigned)) =
        (r.u8(), r.u8(), r.u8())
    else {
        return Err(bad(r));
    };
    let keys = r.compact_u16().ok_or_else(|| bad(r))?;
    let mut account_keys = Vec::with_capacity(keys.min(r.rest() / 32));
    for _ in 0..keys {
        account_keys.push(r.hash().ok_or_else(|| bad(r))?);
    }
    let recent_blockhash = r.hash().ok_or_else(|| bad(r))?;
    let ixs = r.compact_u16().ok_or_else(|| bad(r))?;
    let mut instructions = Vec::with_capacity(ixs.min(r.rest()));
    for _ in 0..ixs {
        let program_id_index = r.u8().ok_or_else(|| bad(r))?;
        let n = r.compact_u16().ok_or_else(|| bad(r))?;
        let accounts = r.take(n).ok_or_else(|| bad(r))?;
        let n = r.compact_u16().ok_or_else(|| bad(r))?;
        let data = r.take(n).ok_or_else(|| bad(r))?;
        instructions.push(Instruction {
            program_id_index,
            accounts,
            data,
        });
    }
    let mut address_table_lookups = Vec::new();
    if version.is_some() {
        let n = r.compact_u16().ok_or_else(|| bad(r))?;
        for _ in 0..n {
            let table = r.hash().ok_or_else(|| bad(r))?;
            let w = r.compact_u16().ok_or_else(|| bad(r))?;
            let writable = r.take(w).ok_or_else(|| bad(r))?;
            let ro = r.compact_u16().ok_or_else(|| bad(r))?;
            let readonly = r.take(ro).ok_or_else(|| bad(r))?;
            address_table_lookups.push(AddressTableLookup {
                table,
                writable,
                readonly,
            });
        }
    }

    let tx = Transaction {
        raw: &r.bytes[start..r.pos],
        message: &r.bytes[message_start..r.pos],
        signatures,
        version,
        num_required_signatures,
        num_readonly_signed,
        num_readonly_unsigned,
        account_keys,
        recent_blockhash,
        instructions,
        address_table_lookups,
    };
    sanity_check(&tx)?;
    Ok(tx)
}

const V1_PREFIX: u8 = 0x81;
const V1_MAX_SIGNATURES: u8 = 12;
const V1_MAX_INSTRUCTIONS: u8 = 64;
const V1_MAX_ADDRESSES: u8 = 64;
/// Config mask bits: priority fee (both bits 0 and 1), compute unit limit, loaded
/// accounts data size, heap size.
const V1_PRIORITY_FEE: u32 = 0b11;
const V1_KNOWN_BITS: u32 = 0b11111;

/// A v1 transaction, from its 0x81 byte. Accepts exactly what Agave's `SchemaRead`
/// accepts and `v1::Message::validate` passes.
fn parse_v1<'a>(r: &mut Reader<'a>) -> Result<Transaction<'a>, Error> {
    let start = r.pos;
    let bad = |r: &Reader| Error::Malformed(r.pos);
    r.pos += 1; // the 0x81 version byte, part of the signed message

    let (Some(num_required_signatures), Some(num_readonly_signed), Some(num_readonly_unsigned)) =
        (r.u8(), r.u8(), r.u8())
    else {
        return Err(bad(r));
    };
    let mask = u32::from_le_bytes(r.take(4).ok_or_else(|| bad(r))?.try_into().unwrap());
    if mask & !V1_KNOWN_BITS != 0 {
        return Err(Error::Invalid("v1: unknown config mask bits"));
    }
    let fee_bits = mask & V1_PRIORITY_FEE;
    if fee_bits != 0 && fee_bits != V1_PRIORITY_FEE {
        return Err(Error::Invalid("v1: half a priority fee"));
    }
    let recent_blockhash = r.hash().ok_or_else(|| bad(r))?;
    let num_instructions = r.u8().ok_or_else(|| bad(r))?;
    let num_addresses = r.u8().ok_or_else(|| bad(r))?;
    let mut account_keys = Vec::with_capacity((num_addresses as usize).min(r.rest() / 32));
    for _ in 0..num_addresses {
        account_keys.push(r.hash().ok_or_else(|| bad(r))?);
    }

    // Config values, in bit order. Only the heap size is checked (Agave's `validate`).
    if fee_bits == V1_PRIORITY_FEE {
        r.take(8).ok_or_else(|| bad(r))?;
    }
    for bit in [0b100, 0b1000] {
        if mask & bit != 0 {
            r.take(4).ok_or_else(|| bad(r))?;
        }
    }
    if mask & 0b10000 != 0 {
        let heap = u32::from_le_bytes(r.take(4).ok_or_else(|| bad(r))?.try_into().unwrap());
        if !heap.is_multiple_of(1024) || !(32 * 1024..=256 * 1024).contains(&heap) {
            return Err(Error::Invalid("v1: bad heap size"));
        }
    }

    let headers = r
        .take(4 * num_instructions as usize)
        .ok_or_else(|| bad(r))?;
    let mut instructions = Vec::with_capacity(num_instructions as usize);
    // Indexed rather than `as_chunks`, which the zkVM's Rust toolchain may not have.
    for i in 0..num_instructions as usize {
        let h = &headers[4 * i..4 * i + 4];
        let accounts = r.take(h[1] as usize).ok_or_else(|| bad(r))?;
        let data = r
            .take(u16::from_le_bytes([h[2], h[3]]) as usize)
            .ok_or_else(|| bad(r))?;
        instructions.push(Instruction {
            program_id_index: h[0],
            accounts,
            data,
        });
    }
    let message_end = r.pos;
    let mut signatures = Vec::with_capacity(num_required_signatures as usize);
    for _ in 0..num_required_signatures {
        signatures.push(r.take(64).ok_or_else(|| bad(r))?.try_into().unwrap());
    }

    let tx = Transaction {
        raw: &r.bytes[start..r.pos],
        message: &r.bytes[start..message_end],
        signatures,
        version: Some(1),
        num_required_signatures,
        num_readonly_signed,
        num_readonly_unsigned,
        account_keys,
        recent_blockhash,
        instructions,
        address_table_lookups: Vec::new(),
    };
    sanity_check_v1(&tx, num_instructions)?;
    Ok(tx)
}

/// `v1::Message::validate`, rule for rule.
fn sanity_check_v1(tx: &Transaction, num_instructions: u8) -> Result<(), Error> {
    if tx.num_required_signatures > V1_MAX_SIGNATURES {
        return Err(Error::Invalid("v1: more than 12 signatures"));
    }
    if num_instructions > V1_MAX_INSTRUCTIONS {
        return Err(Error::Invalid("v1: more than 64 instructions"));
    }
    let keys = tx.account_keys.len();
    if keys > V1_MAX_ADDRESSES as usize {
        return Err(Error::Invalid("v1: more than 64 addresses"));
    }
    if keys < tx.num_required_signatures as usize + tx.num_readonly_unsigned as usize {
        return Err(Error::Invalid("v1: not enough addresses for the header"));
    }
    if tx.num_readonly_signed >= tx.num_required_signatures {
        return Err(Error::Invalid("fee payer must be writable"));
    }
    let mut sorted = tx.account_keys.clone();
    sorted.sort_unstable();
    if sorted.windows(2).any(|w| w[0] == w[1]) {
        return Err(Error::Invalid("v1: duplicate addresses"));
    }
    for ix in &tx.instructions {
        if ix.program_id_index == 0 || ix.program_id_index as usize >= keys {
            return Err(Error::Invalid("bad program id index"));
        }
        if ix.accounts.iter().any(|&a| a as usize >= keys) {
            return Err(Error::Invalid("account index out of range"));
        }
    }
    Ok(())
}

/// The structural rules the runtime enforces before executing (Agave `sanitize`), limited
/// to what this crate exposes: a parse that passes these is a transaction a validator
/// would accept into a block.
fn sanity_check(tx: &Transaction) -> Result<(), Error> {
    let req = tx.num_required_signatures as usize;
    if tx.signatures.len() != req {
        return Err(Error::Invalid("signature count != num_required_signatures"));
    }
    if req == 0 || req > tx.account_keys.len() {
        return Err(Error::Invalid("bad num_required_signatures"));
    }
    if tx.num_readonly_signed as usize >= req {
        return Err(Error::Invalid("fee payer must be writable"));
    }
    if tx
        .address_table_lookups
        .iter()
        .any(|l| l.writable.is_empty() && l.readonly.is_empty())
    {
        return Err(Error::Invalid("lookup table loads no accounts"));
    }
    let lookup_keys: usize = tx
        .address_table_lookups
        .iter()
        .map(|l| l.writable.len() + l.readonly.len())
        .sum();
    let total_keys = tx.account_keys.len() + lookup_keys;
    if total_keys > 256 {
        return Err(Error::Invalid("more than 256 accounts"));
    }
    if (tx.num_readonly_unsigned as usize) > tx.account_keys.len() - req {
        return Err(Error::Invalid("too many readonly unsigned"));
    }
    for ix in &tx.instructions {
        // Program ids must be static keys and not the fee payer.
        if ix.program_id_index == 0 || ix.program_id_index as usize >= tx.account_keys.len() {
            return Err(Error::Invalid("bad program id index"));
        }
        if ix.accounts.iter().any(|&a| a as usize >= total_keys) {
            return Err(Error::Invalid("account index out of range"));
        }
    }
    Ok(())
}
