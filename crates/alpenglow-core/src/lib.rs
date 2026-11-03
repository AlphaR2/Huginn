//! Alpenglow light-client checks in pure Rust: finality certificates, block_id, and
//! transactions inside blocks. No Agave crates and no C, so the same code runs natively
//! and inside SP1.

pub mod block;
pub mod finality;
#[cfg(feature = "fixture")]
pub mod fixture;
pub mod proof;
pub mod shred;
pub mod tx;
