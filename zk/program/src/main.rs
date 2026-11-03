//! Huginn's zkVM program. Reads a `TxInput` (a finality certificate plus one transaction's
//! inclusion), runs exactly the checks the native tests run, and commits the public values
//! in their fixed byte layout. If any check fails the program panics, so no proof exists.

#![no_main]
sp1_zkvm::entrypoint!(main);

use alpenglow_core::proof::{TxInput, TxOutput};

pub fn main() {
    let input: TxInput = sp1_zkvm::io::read();

    // Cycle-tracker markers split the report into the two halves of the statement.
    println!("cycle-tracker-report-start: finality");
    let finality = input.finality.verify().expect("finality");
    println!("cycle-tracker-report-end: finality");

    println!("cycle-tracker-report-start: inclusion");
    let tx = input
        .inclusion
        .verify(finality.slot, &finality.block_id, finality.shred_version)
        .expect("inclusion");
    println!("cycle-tracker-report-end: inclusion");

    sp1_zkvm::io::commit_slice(&TxOutput { finality, tx }.encode());
}
