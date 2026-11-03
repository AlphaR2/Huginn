//! Alpenglow light-client checks in pure Rust: finality certificates and block_id.
//! No Agave crates and no C, so the same code runs natively and inside SP1.

pub mod block_proof;
pub mod finality;
#[cfg(feature = "fixture")]
pub mod fixture;
pub mod shred;
