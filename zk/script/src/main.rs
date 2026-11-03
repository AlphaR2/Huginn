//! Runs the program on the real local transaction, then with the same transaction
//! finalized by synthetic validator sets of chosen sizes, and reports cycles. Every run is
//! also verified natively; the committed public values must match.
//!
//!   cargo run --release -- --synthetic 100,1000,1500 --slow 1000
//!
//! With `--prove`, each run is also proven, the proof verified, and the proof, public
//! values and program key written to `--out`:
//!
//!   cargo run --release -- --prove compressed
//!   cargo run --release -- --prove groth16      # needs Docker running (gnark wrap)

use {
    alpenglow_core::{fixture, proof::TxInput},
    anyhow::{ensure, Context, Result},
    clap::Parser,
    sp1_sdk::{include_elf, prelude::*, Elf, HashableKey, ProverClient, SP1Stdin},
    std::{path::Path, time::Instant},
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
    /// Also prove each run: core, compressed or groth16.
    #[arg(long)]
    prove: Option<String>,
    /// Where proofs go.
    #[arg(long, default_value = "../../fixtures/proofs")]
    out: String,
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
        if let Some(mode) = &cli.prove {
            prove(&client, mode, &name, stdin_for(&input), &native, &cli.out).await?;
        }
    }
    Ok(())
}

fn stdin_for(input: &TxInput) -> SP1Stdin {
    let mut stdin = SP1Stdin::new();
    stdin.write(input);
    stdin
}

async fn prove(client: &impl Prover, mode: &str, name: &str, stdin: SP1Stdin, native: &[u8], out: &str) -> Result<()> {
    let err = |e: &dyn std::fmt::Display| anyhow::anyhow!("{e}");
    let t = Instant::now();
    let pk = client.setup(ELF).await.map_err(|e| err(&e))?;
    let setup = t.elapsed();
    let vkey = pk.verifying_key().clone();

    let t = Instant::now();
    let request = client.prove(&pk, stdin);
    let proof = match mode {
        "core" => request.core().await,
        "compressed" => request.compressed().await,
        "groth16" => request.groth16().await,
        _ => anyhow::bail!("--prove takes core, compressed or groth16"),
    }
    .map_err(|e| err(&e))?;
    let proving = t.elapsed();

    let t = Instant::now();
    client.verify(&proof, &vkey, None).map_err(|e| err(&e))?;
    let verifying = t.elapsed();
    ensure!(proof.public_values.as_slice() == native, "proof public values differ from native");

    // One file name per run: "1000 validators, fast" -> "1000-validators-fast".
    let slug: String = name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join("-");
    let dir = Path::new(out);
    std::fs::create_dir_all(dir)?;
    let file = dir.join(format!("{slug}.{mode}.bin"));
    proof.save(&file)?;
    let size = std::fs::metadata(&file)?.len();
    let summary = serde_json::json!({
        "run": name,
        "mode": mode,
        "program_vkey": vkey.bytes32(),
        "public_values": hex::encode(native),
        // What an onchain verifier takes (empty for core/compressed).
        "proof": if mode == "groth16" { hex::encode(proof.bytes()) } else { String::new() },
        "setup_secs": setup.as_secs_f64(),
        "prove_secs": proving.as_secs_f64(),
        "verify_secs": verifying.as_secs_f64(),
        "file_bytes": size,
    });
    std::fs::write(dir.join(format!("{slug}.{mode}.json")), serde_json::to_string_pretty(&summary)? + "\n")?;
    println!(
        "  {mode} proof: setup {:.1}s, prove {:.1}s, verify {:.2}s, {size} bytes -> {}",
        setup.as_secs_f64(),
        proving.as_secs_f64(),
        verifying.as_secs_f64(),
        file.display()
    );
    Ok(())
}
