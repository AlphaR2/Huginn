//! The *handover*: how a light client that trusts epoch N's validator set learns epoch N+1's set
//! from data that Solana itself produced, so no owner has to type it in.
//!
//! Everything that needs "the next set" goes through one trait, [`NextSetSource`]. There are two
//! sources, and swapping one for the other touches only this crate (and its Solidity twin,
//! `huginn-evm/src/sources/`):
//!
//! - **A. [`RecorderSource`]**: a small Solana program records each member's frozen stake
//!   (`sol_get_epoch_stake`) into an account; keys come from the epoch-boundary block's
//!   account changes. Design in `docs/security.md` ("Validator-set handover"). **Not built yet.**
//! - **B. [`EpochStakesSource`]**: SIMD-0676 would have the runtime publish the whole set as one
//!   account in the first block of each epoch. The account layout is parsed here
//!   ([`EpochStakesAccount::parse`]); fetching and proving it waits for the proposal to land.
//!
//! Both produce a [`NextSet`] (what the light client will store) plus [`Evidence`] (what the
//! proof has to show).

use alpenglow_core::proof::{validator_set_hash, Validator};

/// Agave keeps at most this many validators in an epoch's set (SIMD-0357).
pub const MAX_VALIDATORS: usize = 2000;

/// A validator set in certificate rank order, ready to commit to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NextSet {
    /// The epoch whose certificates this set checks.
    pub epoch: u64,
    /// Rank order: the order certificate bitmaps index into.
    pub validators: Vec<Validator>,
    pub total_stake: u64,
}

impl NextSet {
    /// What the light client stores: `alpenglow_core::proof::validator_set_hash`.
    pub fn commitment(&self) -> [u8; 32] {
        validator_set_hash(&self.validators)
    }
}

/// What a proof must show for the light client to accept a [`NextSet`] without an owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// A: the recorder account's contents, proven through the footer bank hash and lattice-hash
    /// delta of the block that wrote it, plus each member's BLS key from the boundary block.
    Recorder { record_slot: u64, boundary_slot: u64 },
    /// B: the SIMD-0676 account for `account_epoch` (it governs `account_epoch + 1`), proven in
    /// the first block of `account_epoch`, which that epoch's (trusted) set certified.
    EpochStakesAccount { account_epoch: u64, block_slot: u64, data: Vec<u8> },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HandoverError {
    #[error("{0} is not built yet: {1}")]
    NotBuilt(&'static str, &'static str),
    #[error("epoch-stakes account: {0}")]
    BadAccount(String),
}

/// The one interface: "give me epoch `epoch`'s set and the evidence for it".
pub trait NextSetSource {
    fn name(&self) -> &'static str;
    fn next_set(&self, epoch: u64) -> Result<(NextSet, Evidence), HandoverError>;
}

/// Source A (see the crate docs). Gated on quests Q4 (recorder on devnet) and Q5 (keys from the
/// boundary delta), and on hardening the delta check (observer W2).
pub struct RecorderSource;

impl NextSetSource for RecorderSource {
    fn name(&self) -> &'static str {
        "recorder"
    }
    fn next_set(&self, _epoch: u64) -> Result<(NextSet, Evidence), HandoverError> {
        Err(HandoverError::NotBuilt(
            "the recorder source",
            "waits on quests Q4 and Q5 and the delta hardening (W2)",
        ))
    }
}

/// Source B (see the crate docs). Gated on SIMD-0676 being accepted and activated.
pub struct EpochStakesSource;

impl NextSetSource for EpochStakesSource {
    fn name(&self) -> &'static str {
        "simd-0676 epoch stakes"
    }
    fn next_set(&self, _epoch: u64) -> Result<(NextSet, Evidence), HandoverError> {
        Err(HandoverError::NotBuilt(
            "the SIMD-0676 source",
            "the proposal is not merged or activated; the account parser is ready (EpochStakesAccount::parse)",
        ))
    }
}

/// One entry of a SIMD-0676 epoch-stakes account (160 bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpochStakesEntry {
    pub vote_pubkey: [u8; 32],
    pub node_pubkey: [u8; 32],
    pub delegated_stake: u64,
    pub bls_signer_index: u16,
    pub bls_pubkey_compressed: [u8; 48],
}

/// A parsed SIMD-0676 epoch-stakes account. Layout from the proposal text (recorded in
/// `quests/q12-simd-0676/README.md`); recheck when the proposal is merged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpochStakesAccount {
    pub version: u32,
    pub epoch: u64,
    pub total_stake: u64,
    /// As stored: sorted by vote pubkey.
    pub entries: Vec<EpochStakesEntry>,
}

const HEADER: usize = 32;
const ENTRY: usize = 160;

fn le_u16(b: &[u8]) -> u16 {
    u16::from_le_bytes(b[..2].try_into().unwrap())
}
fn le_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}
fn le_u64(b: &[u8]) -> u64 {
    u64::from_le_bytes(b[..8].try_into().unwrap())
}

impl EpochStakesAccount {
    /// Parses and checks the account. Refuses anything inconsistent rather than guessing:
    /// wrong length, more than 2,000 entries, entries not sorted by vote pubkey, a stake sum
    /// that differs from the header's total, or signer indices that are not exactly 0..n.
    pub fn parse(data: &[u8]) -> Result<Self, HandoverError> {
        let bad = |m: String| HandoverError::BadAccount(m);
        if data.len() < HEADER {
            return Err(bad(format!("{} bytes, shorter than the 32-byte header", data.len())));
        }
        let n = le_u32(&data[4..]) as usize;
        if n > MAX_VALIDATORS {
            return Err(bad(format!("{n} entries, more than {MAX_VALIDATORS}")));
        }
        if data.len() != HEADER + n * ENTRY {
            return Err(bad(format!("{} bytes, expected 32 + {n} x 160", data.len())));
        }
        let entries: Vec<EpochStakesEntry> = data[HEADER..]
            .chunks_exact(ENTRY)
            .map(|e| EpochStakesEntry {
                vote_pubkey: e[0..32].try_into().unwrap(),
                node_pubkey: e[32..64].try_into().unwrap(),
                delegated_stake: le_u64(&e[64..]),
                bls_signer_index: le_u16(&e[84..]),
                bls_pubkey_compressed: e[96..144].try_into().unwrap(),
            })
            .collect();
        if entries.windows(2).any(|w| w[0].vote_pubkey >= w[1].vote_pubkey) {
            return Err(bad("entries are not strictly sorted by vote pubkey".into()));
        }
        let total = le_u64(&data[16..]);
        let sum = entries.iter().try_fold(0u64, |t, e| t.checked_add(e.delegated_stake));
        if sum != Some(total) {
            return Err(bad(format!("entry stakes sum to {sum:?}, header says {total}")));
        }
        let mut seen = vec![false; n];
        for e in &entries {
            let i = e.bls_signer_index as usize;
            if i >= n || std::mem::replace(&mut seen[i], true) {
                return Err(bad(format!("signer index {i} out of range or repeated")));
            }
        }
        Ok(Self { version: le_u32(data), epoch: le_u64(&data[8..]), total_stake: total, entries })
    }

    /// The set this account governs: epoch `self.epoch + 1`, in signer-index (rank) order.
    pub fn next_set(&self) -> NextSet {
        let mut ranked: Vec<&EpochStakesEntry> = self.entries.iter().collect();
        ranked.sort_by_key(|e| e.bls_signer_index);
        NextSet {
            epoch: self.epoch + 1,
            validators: ranked
                .into_iter()
                .map(|e| Validator { bls_pubkey: e.bls_pubkey_compressed.to_vec(), stake: e.delegated_stake })
                .collect(),
            total_stake: self.total_stake,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic account: `stakes[i]` for vote pubkey [i+1; 32], signer index `rank[i]`.
    fn account(epoch: u64, stakes: &[u64], rank: &[u16]) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&1u32.to_le_bytes());
        d.extend_from_slice(&(stakes.len() as u32).to_le_bytes());
        d.extend_from_slice(&epoch.to_le_bytes());
        d.extend_from_slice(&stakes.iter().sum::<u64>().to_le_bytes());
        d.extend_from_slice(&[0; 8]);
        for (i, (&s, &r)) in stakes.iter().zip(rank).enumerate() {
            let mut e = [0u8; 160];
            e[0..32].copy_from_slice(&[i as u8 + 1; 32]);
            e[32..64].copy_from_slice(&[0xa0 + i as u8; 32]);
            e[64..72].copy_from_slice(&s.to_le_bytes());
            e[84..86].copy_from_slice(&r.to_le_bytes());
            e[96..144].copy_from_slice(&[0xb0 + i as u8; 48]);
            e[144..152].copy_from_slice(&u64::MAX.to_le_bytes());
            e[152..160].copy_from_slice(&u64::MAX.to_le_bytes());
            d.extend_from_slice(&e);
        }
        d
    }

    #[test]
    fn parses_and_ranks_by_signer_index() {
        // Stored by vote pubkey; ranks put the 30-stake validator first.
        let a = EpochStakesAccount::parse(&account(7, &[10, 30, 20], &[2, 0, 1])).unwrap();
        assert_eq!((a.epoch, a.total_stake, a.entries.len()), (7, 60, 3));
        let s = a.next_set();
        assert_eq!(s.epoch, 8, "the account for epoch 7 governs epoch 8");
        assert_eq!(s.validators.iter().map(|v| v.stake).collect::<Vec<_>>(), vec![30, 20, 10]);
        assert_eq!(s.validators[0].bls_pubkey, vec![0xb1; 48]);
        assert_eq!(s.commitment(), validator_set_hash(&s.validators));
    }

    #[test]
    fn refuses_inconsistent_accounts() {
        let good = account(1, &[5, 6], &[1, 0]);
        let mut short = good.clone();
        short.pop();
        assert!(EpochStakesAccount::parse(&short).is_err(), "length");
        let mut total = good.clone();
        total[16] ^= 1;
        assert!(EpochStakesAccount::parse(&total).is_err(), "stake sum");
        assert!(EpochStakesAccount::parse(&account(1, &[5, 6], &[0, 0])).is_err(), "repeated signer index");
        assert!(EpochStakesAccount::parse(&account(1, &[5, 6], &[0, 2])).is_err(), "signer index out of range");
        let mut unsorted = good.clone();
        unsorted[32..64].copy_from_slice(&[9; 32]); // first vote pubkey now above the second
        assert!(EpochStakesAccount::parse(&unsorted).is_err(), "order");
        let mut huge = good;
        huge[4..8].copy_from_slice(&2001u32.to_le_bytes());
        assert!(EpochStakesAccount::parse(&huge).is_err(), "too many entries");
    }

    #[test]
    fn both_sources_answer_through_one_interface() {
        let sources: Vec<Box<dyn NextSetSource>> = vec![Box::new(RecorderSource), Box::new(EpochStakesSource)];
        for s in &sources {
            assert!(matches!(s.next_set(5), Err(HandoverError::NotBuilt(..))), "{}", s.name());
        }
    }
}
