//! Huginn's zkVM program. Reads a `BlockProofInput`, runs the same checks the native tests
//! run, and commits the output. If any check fails the program panics, so no proof exists.

#![no_main]
sp1_zkvm::entrypoint!(main);

use alpenglow_core::block_proof::{validator_set_hash, BlockProofInput, BlockProofOutput};

pub fn main() {
    let input: BlockProofInput = sp1_zkvm::io::read();

    // Cycle-tracker markers split the report into the two halves of the check.
    println!("cycle-tracker-report-start: finality");
    let (signed_stake, total_stake) = input.verify_finality().expect("finality");
    println!("cycle-tracker-report-end: finality");

    println!("cycle-tracker-report-start: inclusion");
    input.verify_inclusion().expect("inclusion");
    println!("cycle-tracker-report-end: inclusion");

    println!("cycle-tracker-report-start: set_hash");
    let validator_set_hash = validator_set_hash(&input.validators);
    println!("cycle-tracker-report-end: set_hash");

    sp1_zkvm::io::commit(&BlockProofOutput {
        validator_set_hash,
        shred_version: input.shred_version,
        slot: input.slot,
        block_id: input.block_id,
        signed_stake,
        total_stake,
    });
}
