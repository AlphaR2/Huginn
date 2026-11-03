//! `huginn`: runs the Huginn program on the real local transaction, then with the same
//! transaction finalized by synthetic validator sets of chosen sizes, and reports cycles.
//! Every run is also verified natively; the committed public values must match.
//!
//!   cargo run --release -- --synthetic 100,1000,1500 --slow 1000
//!
//! With `--prove`, each run is also proven, the proof verified, and its summary (program
//! key, public values, onchain proof bytes) written to `--out`:
//!
//!   cargo run --release -- --prove compressed
//!   cargo run --release -- --prove groth16      # needs Docker running (gnark wrap)

use {
    alpenglow_core::{fixture, proof::TxInput},
    anyhow::Result,
    clap::Parser,
    huginn_prover::{alpenglow_core, tx_input_from_block_file, Huginn, Mode},
    std::path::Path,
};

#[derive(Parser)]
struct Cli {
    /// Block fixture from `ledger-dump` that contains the transaction.
    #[arg(long, default_value = "../../fixtures/local/block_46718.json")]
    block: String,
    /// The transaction's signature, base58 or hex.
    #[arg(
        long,
        default_value = "4F39ig6uWUKU8cAcUYwyCeTasG5U6KgZLoYcbkabhVsbJo3pLWMGkQjsUfUMKDrm7P9HkZrjoFzpRjmAc1YJ8dL"
    )]
    signature: String,
    /// Synthetic fast-path sets of these sizes (~90% signing).
    #[arg(long, value_delimiter = ',')]
    synthetic: Vec<usize>,
    /// Synthetic slow-path sets of these sizes.
    #[arg(long, value_delimiter = ',')]
    slow: Vec<usize>,
    /// Also prove each run: core, compressed or groth16.
    #[arg(long)]
    prove: Option<Mode>,
    /// Where proof summaries go.
    #[arg(long, default_value = "../../fixtures/proofs")]
    out: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    sp1_sdk::utils::setup_logger();
    let cli = Cli::parse();
    let real = tx_input_from_block_file(&cli.block, &cli.signature)?;

    let mut runs = vec![("real, 1 validator, fast".to_string(), real.clone())];
    for &n in &cli.synthetic {
        runs.push((format!("{n} validators, fast"), TxInput { finality: fixture::synthetic(&real.finality, n), ..real.clone() }));
    }
    for &n in &cli.slow {
        runs.push((format!("{n} validators, slow"), TxInput { finality: fixture::synthetic_slow(&real.finality, n), ..real.clone() }));
    }

    let huginn = Huginn::from_env().await;
    println!("{:<28} {:>14} {:>14} {:>14}", "input", "finality", "inclusion", "total cycles");
    for (name, input) in runs {
        let e = huginn.execute(&input).await?;
        println!("{name:<28} {:>14} {:>14} {:>14}", e.finality_cycles, e.inclusion_cycles, e.total_cycles);
        let Some(mode) = cli.prove else { continue };

        let proven = huginn.prove(&input, mode).await?;
        // One file name per run: "1000 validators, fast" -> "1000-validators-fast".
        let slug = name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join("-");
        let dir = Path::new(&cli.out);
        std::fs::create_dir_all(dir)?;
        let bin = dir.join(format!("{slug}.{}.bin", mode.as_str()));
        proven.proof.save(&bin)?;
        let summary = dir.join(format!("{slug}.{}.json", mode.as_str()));
        std::fs::write(&summary, serde_json::to_string_pretty(&proven.summary(&name))? + "\n")?;
        println!(
            "  {} proof: setup {:.1}s, prove {:.1}s, verify {:.2}s -> {}",
            mode.as_str(),
            proven.setup_secs,
            proven.prove_secs,
            proven.verify_secs,
            summary.display()
        );
    }
    Ok(())
}
