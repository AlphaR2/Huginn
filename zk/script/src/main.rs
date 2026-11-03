//! Execute mode: runs the program on the real local transaction, then with the same
//! transaction finalized by synthetic validator sets of chosen sizes, and reports cycles.
//! Every run is also verified natively; the committed public values must match.
//!
//!   cargo run --release -- --synthetic 100,1000,1500 --slow 1000

use {
    alpenglow_core::{fixture, proof::TxInput},
    anyhow::{ensure, Context, Result},
    clap::Parser,
    sp1_sdk::{include_elf, prelude::*, Elf, ProverClient, SP1Stdin},
};

const ELF: Elf = include_elf!("huginn-program");

#[derive(Parser)]
struct Cli {
    /// Block fixture from `ledger-dump` that contains the transaction.
    #[arg(long, default_value = "../../fixtures/local/block_46718.json")]
    fixture: String,
    /// RPC copy of the transaction (for its signature).
    #[arg(long, default_value = "../../fixtures/local/tx_46718.json")]
    tx: String,
    /// Synthetic fast-path sets of these sizes (~90% signing).
    #[arg(long, value_delimiter = ',')]
    synthetic: Vec<usize>,
    /// Synthetic slow-path sets of these sizes.
    #[arg(long, value_delimiter = ',')]
    slow: Vec<usize>,
}

fn read(path: &str) -> Result<serde_json::Value> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path).with_context(|| format!("read {path}"))?)?)
}

#[tokio::main]
async fn main() -> Result<()> {
    sp1_sdk::utils::setup_logger();
    let cli = Cli::parse();
    let sig: [u8; 64] = hex::decode(read(&cli.tx)?["signature"].as_str().context("signature")?)?
        .try_into()
        .ok()
        .context("64-byte signature")?;
    let real = fixture::tx_input(&read(&cli.fixture)?, sig);

    let mut runs = vec![("real, 1 validator, fast".to_string(), real.clone())];
    for &n in &cli.synthetic {
        runs.push((format!("{n} validators, fast"), TxInput { finality: fixture::synthetic(&real.finality, n), ..real.clone() }));
    }
    for &n in &cli.slow {
        runs.push((format!("{n} validators, slow"), TxInput { finality: fixture::synthetic_slow(&real.finality, n), ..real.clone() }));
    }

    let client = ProverClient::from_env().await;
    println!("{:<28} {:>14} {:>14} {:>14}", "input", "finality", "inclusion", "total cycles");
    for (name, input) in runs {
        let native = input.verify().context("native verify")?.encode();
        let mut stdin = SP1Stdin::new();
        stdin.write(&input);
        let (public, report) = client.execute(ELF, stdin).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        ensure!(public.as_slice() == native.as_slice(), "zkVM public values differ from native");
        let tracked = |k: &str| report.cycle_tracker.get(k).copied().unwrap_or(0);
        println!("{name:<28} {:>14} {:>14} {:>14}", tracked("finality"), tracked("inclusion"), report.total_instruction_count());
    }
    Ok(())
}
