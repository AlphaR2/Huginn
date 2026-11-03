//! Solana transaction wire format, legacy and v0, parsed without any Solana crates.
//!
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
//! `short_vec` is a compact-u16 length (1-3 bytes) followed by that many items.
//! The signatures sign exactly the `message` bytes.

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
    /// `None` for legacy, `Some(0)` for v0.
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
    let (Some(num_required_signatures), Some(num_readonly_signed), Some(num_readonly_unsigned)) = (r.u8(), r.u8(), r.u8())
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
        instructions.push(Instruction { program_id_index, accounts, data });
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
            address_table_lookups.push(AddressTableLookup { table, writable, readonly });
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
    let lookup_keys: usize = tx.address_table_lookups.iter().map(|l| l.writable.len() + l.readonly.len()).sum();
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
