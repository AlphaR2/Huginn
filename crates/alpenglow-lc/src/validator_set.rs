//! The ranked validator list for one epoch.
//!
//! Certificate bitmaps are indexed by rank, so our ordering must match Agave's exactly.
//! The rules mirror `BLSPubkeyToRankMap::new` in `agave/runtime/src/epoch_stakes.rs` (v4.4.0).

use {
    serde::{Deserialize, Serialize},
    solana_bls_signatures::pubkey::{PubkeyAffine, PubkeyCompressed},
    std::collections::HashMap,
};

pub const BLS_PUBKEY_LEN: usize = 48;

/// One vote account as read from the chain, before any filtering.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub vote_pubkey: [u8; 32],
    pub node_pubkey: [u8; 32],
    pub stake: u64,
    pub bls_pubkey: Option<[u8; BLS_PUBKEY_LEN]>,
}

/// One validator in rank order. Rank = index in `ValidatorSet::entries`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RankedValidator {
    #[serde(with = "hex::serde")]
    pub bls_pubkey: [u8; BLS_PUBKEY_LEN],
    pub stake: u64,
    /// Not used by verification. Kept so fixtures can be cross-checked against explorers.
    #[serde(with = "hex::serde")]
    pub vote_pubkey: [u8; 32],
    #[serde(with = "hex::serde")]
    pub node_pubkey: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatorSet {
    pub epoch: u64,
    pub total_stake: u64,
    pub entries: Vec<RankedValidator>,
}

/// Whether Agave would accept these bytes as a BLS public key (a valid compressed G1 point).
pub fn is_valid_bls_pubkey(bytes: &[u8; BLS_PUBKEY_LEN]) -> bool {
    PubkeyAffine::try_from(PubkeyCompressed(*bytes)).is_ok()
}

impl ValidatorSet {
    /// The set as the proof takes it: keys and stakes in rank order.
    pub fn to_proof_validators(&self) -> Vec<alpenglow_core::proof::Validator> {
        self.entries
            .iter()
            .map(|e| alpenglow_core::proof::Validator { bls_pubkey: e.bls_pubkey.to_vec(), stake: e.stake })
            .collect()
    }

    /// The commitment a destination chain stores for this epoch (item 1.1). See
    /// `alpenglow_core::proof::validator_set_hash` for the exact bytes hashed.
    pub fn commitment(&self) -> [u8; 32] {
        alpenglow_core::proof::validator_set_hash(&self.to_proof_validators())
    }

    /// Builds the ranked list exactly as Agave does:
    /// 1. skip zero stake, missing or invalid BLS key;
    /// 2. drop every entry whose BLS key or node pubkey appears more than once;
    /// 3. sort by stake descending, then BLS key bytes ascending.
    pub fn from_candidates(epoch: u64, candidates: impl IntoIterator<Item = Candidate>) -> Self {
        let kept: Vec<(Candidate, [u8; BLS_PUBKEY_LEN])> = candidates
            .into_iter()
            .filter(|c| c.stake > 0)
            .filter_map(|c| {
                let key = c.bls_pubkey?;
                is_valid_bls_pubkey(&key).then_some((c, key))
            })
            .collect();

        // Counted after the filters above, as in Agave: a zero-stake duplicate does not
        // knock out a staked entry.
        let mut bls_counts = HashMap::new();
        let mut node_counts = HashMap::new();
        for (c, key) in &kept {
            *bls_counts.entry(*key).or_insert(0u32) += 1;
            *node_counts.entry(c.node_pubkey).or_insert(0u32) += 1;
        }

        let mut entries: Vec<RankedValidator> = kept
            .into_iter()
            .filter(|(c, key)| bls_counts[key] == 1 && node_counts[&c.node_pubkey] == 1)
            .map(|(c, key)| RankedValidator {
                bls_pubkey: key,
                stake: c.stake,
                vote_pubkey: c.vote_pubkey,
                node_pubkey: c.node_pubkey,
            })
            .collect();
        entries.sort_by(|a, b| b.stake.cmp(&a.stake).then(a.bls_pubkey.cmp(&b.bls_pubkey)));

        let total_stake = entries.iter().fold(0u64, |t, e| t.saturating_add(e.stake));
        Self { epoch, total_stake, entries }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use solana_bls_signatures::keypair::Keypair;

    /// A real compressed BLS public key, deterministic per seed.
    fn key(seed: u8) -> [u8; 48] {
        let kp = Keypair::derive(&[seed; 32]).unwrap();
        PubkeyCompressed::from(&*kp.public).0
    }

    fn cand(v: u8, n: u8, stake: u64, key: Option<[u8; 48]>) -> Candidate {
        Candidate { vote_pubkey: [v; 32], node_pubkey: [n; 32], stake, bls_pubkey: key }
    }

    #[test]
    fn real_key_is_valid_garbage_and_identity_are_not() {
        assert!(is_valid_bls_pubkey(&key(1)));
        assert!(!is_valid_bls_pubkey(&[0xffu8; 48]));
        let mut identity = [0u8; 48];
        identity[0] = 0xc0; // compressed point at infinity
        assert!(!is_valid_bls_pubkey(&identity));
    }

    #[test]
    fn skips_zero_stake_missing_and_invalid_keys() {
        let set = ValidatorSet::from_candidates(
            1,
            [
                cand(1, 1, 0, Some(key(1))),
                cand(2, 2, 5, None),
                cand(3, 3, 5, Some([0xff; 48])),
                cand(4, 4, 7, Some(key(1))),
            ],
        );
        assert_eq!(set.entries.len(), 1);
        assert_eq!(set.entries[0].vote_pubkey, [4; 32]);
        assert_eq!(set.total_stake, 7);
    }

    #[test]
    fn zero_stake_duplicate_does_not_remove_staked_entry() {
        let set = ValidatorSet::from_candidates(1, [cand(1, 1, 0, Some(key(1))), cand(2, 2, 9, Some(key(1)))]);
        assert_eq!(set.entries.len(), 1);
    }

    #[test]
    fn duplicate_bls_key_drops_all_copies() {
        let set = ValidatorSet::from_candidates(1, [cand(1, 1, 5, Some(key(1))), cand(2, 2, 9, Some(key(1)))]);
        assert!(set.entries.is_empty());
    }

    #[test]
    fn duplicate_node_pubkey_drops_all_copies() {
        let set =
            ValidatorSet::from_candidates(1, [cand(1, 7, 5, Some(key(1))), cand(2, 7, 9, Some(key(2)))]);
        assert!(set.entries.is_empty());
    }

    #[test]
    fn sorts_by_stake_then_key() {
        let (lo, hi) = if key(1) < key(2) { (key(1), key(2)) } else { (key(2), key(1)) };
        let set = ValidatorSet::from_candidates(
            1,
            [cand(1, 1, 5, Some(hi)), cand(2, 2, 5, Some(lo)), cand(3, 3, 9, Some(key(3)))],
        );
        let order: Vec<_> = set.entries.iter().map(|e| e.bls_pubkey).collect();
        assert_eq!(order, vec![key(3), lo, hi]);
    }
}
