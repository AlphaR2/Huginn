//! Bytes on the wire for Agave's repair protocol (v4.4.x, `core/src/repair/serve_repair.rs`).
//!
//! Everything is wincode with the default configuration: integers little-endian, enum
//! variants a `u32` index in declaration order, `Vec<u8>` a `u64` length then the bytes.
//! A request is `tag u32 | signature 64 | sender 32 | recipient 32 | timestamp u64 (ms) |
//! nonce u32 | body`. The signature covers the tag and everything after the signature.

use sha2::{Digest, Sha256};

pub type Hash = [u8; 32];
pub type Pubkey = [u8; 32];

/// `RepairProtocol` variant indices.
pub const TAG_PONG: u32 = 7;
pub const TAG_WINDOW_INDEX: u32 = 8;
pub const TAG_HIGHEST_WINDOW_INDEX: u32 = 9;
pub const TAG_PARENT_AND_FEC_SET_COUNT: u32 = 12;
pub const TAG_FEC_SET_ROOT: u32 = 13;
pub const TAG_WINDOW_INDEX_FOR_BLOCK_ID: u32 = 14;

/// `BlockIdRepairResponse` variant indices.
pub const RESP_PARENT_FEC_SET_COUNT: u32 = 0;
pub const RESP_FEC_SET_ROOT: u32 = 1;
pub const RESP_PING: u32 = 2;

/// `RepairResponse::Ping` (answer to shred requests): `0u32 | from 32 | token 32 | sig 64`.
pub const PING_PACKET_BYTES: usize = 4 + 32 + 32 + 64;
pub const NONCE_BYTES: usize = 4;

pub enum Body {
    WindowIndex {
        slot: u64,
        shred_index: u64,
    },
    HighestWindowIndex {
        slot: u64,
        shred_index: u64,
    },
    ParentAndFecSetCount {
        slot: u64,
        block_id: Hash,
    },
    FecSetRoot {
        slot: u64,
        block_id: Hash,
        fec_set_index: u32,
    },
    WindowIndexForBlockId {
        slot: u64,
        shred_index: u32,
        block_id: Hash,
    },
}

impl Body {
    fn tag(&self) -> u32 {
        match self {
            Body::WindowIndex { .. } => TAG_WINDOW_INDEX,
            Body::HighestWindowIndex { .. } => TAG_HIGHEST_WINDOW_INDEX,
            Body::ParentAndFecSetCount { .. } => TAG_PARENT_AND_FEC_SET_COUNT,
            Body::FecSetRoot { .. } => TAG_FEC_SET_ROOT,
            Body::WindowIndexForBlockId { .. } => TAG_WINDOW_INDEX_FOR_BLOCK_ID,
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Body::WindowIndex { slot, shred_index }
            | Body::HighestWindowIndex { slot, shred_index } => {
                out.extend_from_slice(&slot.to_le_bytes());
                out.extend_from_slice(&shred_index.to_le_bytes());
            }
            Body::ParentAndFecSetCount { slot, block_id } => {
                out.extend_from_slice(&slot.to_le_bytes());
                out.extend_from_slice(block_id);
            }
            Body::FecSetRoot {
                slot,
                block_id,
                fec_set_index,
            } => {
                out.extend_from_slice(&slot.to_le_bytes());
                out.extend_from_slice(block_id);
                out.extend_from_slice(&fec_set_index.to_le_bytes());
            }
            Body::WindowIndexForBlockId {
                slot,
                shred_index,
                block_id,
            } => {
                out.extend_from_slice(&slot.to_le_bytes());
                out.extend_from_slice(&shred_index.to_le_bytes());
                out.extend_from_slice(block_id);
            }
        }
    }
}

/// An unsigned request: the signature field is zero. [`sign_request`] fills it.
pub fn request(
    body: &Body,
    sender: &Pubkey,
    recipient: &Pubkey,
    timestamp_ms: u64,
    nonce: u32,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(160);
    out.extend_from_slice(&body.tag().to_le_bytes());
    out.extend_from_slice(&[0u8; 64]);
    out.extend_from_slice(sender);
    out.extend_from_slice(recipient);
    out.extend_from_slice(&timestamp_ms.to_le_bytes());
    out.extend_from_slice(&nonce.to_le_bytes());
    body.write(&mut out);
    out
}

/// The bytes the sender signs: the tag, then everything after the signature field.
pub fn signed_part(request: &[u8]) -> Vec<u8> {
    [&request[..4], &request[68..]].concat()
}

pub fn put_signature(request: &mut [u8], signature: &[u8; 64]) {
    request[4..68].copy_from_slice(signature);
}

/// What we answer to a ping: `hash = sha256("SOLANA_PING_PONG" | token)`, signed by us.
pub fn pong_hash(token: &[u8; 32]) -> Hash {
    Sha256::digest([b"SOLANA_PING_PONG".as_slice(), token].concat()).into()
}

/// `RepairProtocol::Pong`: `7u32 | from 32 | hash 32 | signature 64` (signature over the hash).
pub fn pong(from: &Pubkey, hash: &Hash, signature: &[u8; 64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 32 + 32 + 64);
    out.extend_from_slice(&TAG_PONG.to_le_bytes());
    out.extend_from_slice(from);
    out.extend_from_slice(hash);
    out.extend_from_slice(signature);
    out
}

/// A parsed answer. The nonce is the last four bytes of every response packet except pings
/// answering shred requests.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    /// `RepairResponse::Ping`: sign the token and send a pong, then ask again.
    Ping {
        token: [u8; 32],
    },
    Parent {
        fec_set_count: u32,
        parent_slot: u64,
        parent_block_id: Hash,
        proof: Vec<u8>,
    },
    FecRoot {
        root: [u8; 20],
        proof: Vec<u8>,
    },
    /// A data shred, without the trailing nonce.
    Shred(Vec<u8>),
}

/// Reads a packet we got back for a request with this body kind and nonce.
pub fn parse_reply(kind: &Body, packet: &[u8], nonce: u32) -> Option<Reply> {
    // A ping has no nonce and is exactly this long: `RepairResponse::Ping` (variant 0) for
    // shred requests, `BlockIdRepairResponse::Ping` (variant 2) for block-id requests.
    // No real reply has this length, so the length is checked first.
    if packet.len() == PING_PACKET_BYTES
        && (packet[..4] == 0u32.to_le_bytes() || packet[..4] == RESP_PING.to_le_bytes())
    {
        let token: [u8; 32] = packet[36..68].try_into().ok()?;
        return Some(Reply::Ping { token });
    }
    if packet.len() <= NONCE_BYTES {
        return None;
    }
    let (data, n) = packet.split_at(packet.len() - NONCE_BYTES);
    if u32::from_le_bytes(n.try_into().ok()?) != nonce {
        return None;
    }
    match kind {
        Body::ParentAndFecSetCount { .. } | Body::FecSetRoot { .. } => parse_block_id_reply(data),
        _ => Some(Reply::Shred(data.to_vec())),
    }
}

fn parse_block_id_reply(d: &[u8]) -> Option<Reply> {
    let tag = u32::from_le_bytes(d.get(..4)?.try_into().ok()?);
    let b = &d[4..];
    match tag {
        RESP_PARENT_FEC_SET_COUNT => {
            let fec_set_count = u32::from_le_bytes(b.get(..4)?.try_into().ok()?);
            let parent_slot = u64::from_le_bytes(b.get(4..12)?.try_into().ok()?);
            let parent_block_id: Hash = b.get(12..44)?.try_into().ok()?;
            let len = u64::from_le_bytes(b.get(44..52)?.try_into().ok()?) as usize;
            let proof = b.get(52..52 + len)?.to_vec();
            (b.len() == 52 + len).then_some(Reply::Parent {
                fec_set_count,
                parent_slot,
                parent_block_id,
                proof,
            })
        }
        RESP_FEC_SET_ROOT => {
            let root: [u8; 20] = b.get(..20)?.try_into().ok()?;
            let len = u64::from_le_bytes(b.get(20..28)?.try_into().ok()?) as usize;
            let proof = b.get(28..28 + len)?.to_vec();
            (b.len() == 28 + len).then_some(Reply::FecRoot { root, proof })
        }
        RESP_PING => {
            // from 32 | token 32 | signature 64
            let token: [u8; 32] = b.get(32..64)?.try_into().ok()?;
            Some(Reply::Ping { token })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_layout_and_signed_part() {
        let r = request(
            &Body::FecSetRoot {
                slot: 7,
                block_id: [9; 32],
                fec_set_index: 64,
            },
            &[1; 32],
            &[2; 32],
            1234,
            55,
        );
        assert_eq!(&r[..4], &13u32.to_le_bytes());
        assert_eq!(r.len(), 4 + 64 + 32 + 32 + 8 + 4 + 8 + 32 + 4);
        let s = signed_part(&r);
        assert_eq!(s.len(), r.len() - 64);
        assert_eq!(&s[..4], &r[..4]);
        assert_eq!(&s[4..36], &[1; 32]); // sender follows the tag in the signed part
    }

    #[test]
    fn block_id_replies_parse() {
        let mut d = Vec::new();
        d.extend_from_slice(&RESP_FEC_SET_ROOT.to_le_bytes());
        d.extend_from_slice(&[5u8; 20]);
        d.extend_from_slice(&40u64.to_le_bytes());
        d.extend_from_slice(&[6u8; 40]);
        d.extend_from_slice(&77u32.to_le_bytes());
        let body = Body::FecSetRoot {
            slot: 1,
            block_id: [0; 32],
            fec_set_index: 0,
        };
        assert_eq!(
            parse_reply(&body, &d, 77),
            Some(Reply::FecRoot {
                root: [5; 20],
                proof: vec![6; 40]
            })
        );
        assert_eq!(parse_reply(&body, &d, 78), None, "wrong nonce is ignored");
    }

    #[test]
    fn both_kinds_of_ping_parse() {
        for tag in [0u32, RESP_PING] {
            let mut p = Vec::new();
            p.extend_from_slice(&tag.to_le_bytes());
            p.extend_from_slice(&[1u8; 32]); // from
            p.extend_from_slice(&[2u8; 32]); // token
            p.extend_from_slice(&[3u8; 64]); // signature
            assert_eq!(p.len(), PING_PACKET_BYTES);
            let body = Body::FecSetRoot {
                slot: 1,
                block_id: [0; 32],
                fec_set_index: 0,
            };
            assert_eq!(
                parse_reply(&body, &p, 1),
                Some(Reply::Ping { token: [2; 32] })
            );
        }
    }

    #[test]
    fn shred_reply_drops_the_nonce() {
        let mut p = vec![3u8; 1203];
        p.extend_from_slice(&9u32.to_le_bytes());
        let body = Body::WindowIndex {
            slot: 1,
            shred_index: 0,
        };
        assert_eq!(parse_reply(&body, &p, 9), Some(Reply::Shred(vec![3; 1203])));
    }
}
