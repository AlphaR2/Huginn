//! Execute mode: runs the program on real and synthetic inputs and reports cycles.
//! Each run is also checked natively, and the committed output must match.
//!
//!   cargo run --release -- --fixture ../../fixtures/local/block_44000.json --synthetic 100,1000,1500

use {
    alpenglow_core::{
        block_proof::{BlockProofInput, BlockProofOutput},
        fixture,
    },
    anyhow::{ensure, Context, Result},
    clap::Parser,
    sp1_sdk::{include_elf, prelude::*, Elf, ProverClient, SP1Stdin},
};

const ELF: Elf = include_elf!("huginn-program");

#[derive(Parser)]
struct Cli {
    /// Block fixture from `ledger-dump`.
    #[arg(long, default_value = "../../fixtures/local/block_44000.json")]
    fixture: String,
    /// Also run with synthetic sets of these sizes (same block, N validators, ~90% signing).
    #[arg(long, value_delimiter = ',')]
    synthetic: Vec<usize>,
}

#[tokio::main]
async fn main() -> Result<()> {
    sp1_sdk::utils::setup_logger();
    let cli = Cli::parse();
    let json = serde_json::from_str(&std::fs::read_to_string(&cli.fixture).context("read fixture")?)?;
    let real = fixture::from_block_fixture(&json);

    let mut runs = vec![("real (1 validator)".to_string(), real.clone())];
    for &n in &cli.synthetic {
        runs.push((format!("synthetic {n} validators"), fixture::synthetic(&real, n)));
    }

    let client = ProverClient::from_env().await;
    println!("{:<28} {:>9} {:>14} {:>14} {:>12}", "input", "signers", "finality", "inclusion", "total");
    for (name, input) in runs {
        let native = input.verify().context("native verify")?;
        let mut stdin = SP1Stdin::new();
        stdin.write(&input);
        let (mut public, report) = client.execute(ELF, stdin).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        let committed: BlockProofOutput = public.read();
        ensure!(committed == native, "zkVM output differs from native");
        let tracked = |k: &str| report.cycle_tracker.get(k).copied().unwrap_or(0);
        println!(
            "{name:<28} {:>9} {:>14} {:>14} {:>12}",
            signer_count(&input),
            tracked("finality"),
            tracked("inclusion"),
            report.total_instruction_count()
        );
    }
    Ok(())
}

fn signer_count(input: &BlockProofInput) -> u32 {
    input.final_aggregate.bitmap[3..].iter().map(|b| b.count_ones()).sum()
}
