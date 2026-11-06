//! Minimal reader for vote account data: just the node pubkey and the BLS key.
//!
//! Layout (bincode, `solana-vote-interface` VoteStateV4):
//!   0..4     version tag u32 LE (3 = V4)
//!   4..36    node_pubkey
//!   36..132  authorized_withdrawer, inflation_rewards_collector, block_revenue_collector
//!   132..136 two u16 commission fields
//!   136..144 pending_delegator_rewards u64
//!   144      Option tag for bls_pubkey_compressed (0 = None, 1 = Some)
//!   145..193 bls_pubkey_compressed [u8; 48]
//! Older versions (tags 0..=2) also start with node_pubkey at 4 but have no BLS key.

use anyhow::{bail, Result};

pub const VERSION_V4: u32 = 3;
const NODE: std::ops::Range<usize> = 4..36;
const BLS_TAG: usize = 144;
const BLS: std::ops::Range<usize> = 145..193;

#[derive(Debug, PartialEq, Eq)]
pub struct VoteAccountInfo {
    pub version: u32,
    pub node_pubkey: [u8; 32],
    pub bls_pubkey: Option<[u8; 48]>,
}

pub fn parse(data: &[u8]) -> Result<VoteAccountInfo> {
    if data.len() < NODE.end {
        bail!("vote account too short: {} bytes", data.len());
    }
    let version = u32::from_le_bytes(data[0..4].try_into()?);
    if version > VERSION_V4 {
        bail!("unknown vote state version {version}");
    }
    let node_pubkey = data[NODE].try_into()?;
    let bls_pubkey = if version == VERSION_V4 {
        if data.len() < BLS.end {
            bail!("V4 vote account too short: {} bytes", data.len());
        }
        match data[BLS_TAG] {
            0 => None,
            1 => Some(data[BLS].try_into()?),
            t => bail!("bad Option tag {t} for bls_pubkey_compressed"),
        }
    } else {
        None
    };
    Ok(VoteAccountInfo {
        version,
        node_pubkey,
        bls_pubkey,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(bls: Option<[u8; 48]>) -> Vec<u8> {
        let mut d = vec![0u8; 3762];
        d[0..4].copy_from_slice(&VERSION_V4.to_le_bytes());
        d[NODE].copy_from_slice(&[7; 32]);
        if let Some(k) = bls {
            d[BLS_TAG] = 1;
            d[BLS].copy_from_slice(&k);
        }
        d
    }

    #[test]
    fn reads_v4_with_and_without_key() {
        let a = parse(&v4(Some([9; 48]))).unwrap();
        assert_eq!(a.node_pubkey, [7; 32]);
        assert_eq!(a.bls_pubkey, Some([9; 48]));
        assert_eq!(parse(&v4(None)).unwrap().bls_pubkey, None);
    }

    #[test]
    fn older_versions_have_no_key() {
        let mut d = v4(Some([9; 48]));
        d[0..4].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(parse(&d).unwrap().bls_pubkey, None);
    }

    #[test]
    fn rejects_bad_tag_and_unknown_version() {
        let mut d = v4(None);
        d[BLS_TAG] = 2;
        assert!(parse(&d).is_err());
        d[0..4].copy_from_slice(&9u32.to_le_bytes());
        assert!(parse(&d).is_err());
    }
}
