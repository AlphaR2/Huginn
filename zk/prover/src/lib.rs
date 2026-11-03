//! Huginn prover: proves "this Solana transaction is in a block finalized by this
//! validator set" with SP1, for any destination chain.
//!
//! A relayer (for example `hugin-evm`) depends on this crate:
//!
//! ```ignore
//! let input = huginn_prover::tx_input_from_block_file("block_46718.json", "4F39ig6…")?;
//! let huginn = huginn_prover::Huginn::from_env().await;
//! let proven = huginn.prove(&input, Mode::Groth16).await?;
//! // proven.public_values and proven.onchain_proof go to the destination's verifier.
//! ```
//!
//! Where proving happens follows SP1's own environment: `SP1_PROVER=cpu` (default) or
//! `mock`. The prover network needs sp1-sdk's `network` feature (not enabled yet).

use {
    alpenglow_core::{
        fixture,
        proof::{TxInclusion, TxInput},
    },
    anyhow::{anyhow, bail, ensure, Context, Result},
    sp1_sdk::{
        env::EnvProver, include_elf, prelude::*, Elf, HashableKey, ProverClient, SP1ProofWithPublicValues, SP1Stdin,
    },
    std::{path::Path, str::FromStr, time::Instant},
};

pub use alpenglow_core;

/// The Huginn SP1 program (`zk/program`), compiled by this crate's build script.
pub const ELF: Elf = include_elf!("huginn-program");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// One STARK per chunk of the program; big, fast. Testing only.
    Core,
    /// One STARK, ~1.3 MB. Verifiable off-chain.
    Compressed,
    /// A few hundred bytes, what EVM chains verify. Needs Docker (local) or the network.
    Groth16,
}

impl FromStr for Mode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s {
            "core" => Mode::Core,
            "compressed" => Mode::Compressed,
            "groth16" => Mode::Groth16,
            _ => bail!("mode is core, compressed or groth16, not {s:?}"),
        })
    }
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Core => "core",
            Mode::Compressed => "compressed",
            Mode::Groth16 => "groth16",
        }
    }
}

/// Execute mode: what the program outputs and what it costs, without a proof.
pub struct Execution {
    pub public_values: Vec<u8>,
    pub finality_cycles: u64,
    pub inclusion_cycles: u64,
    pub total_cycles: u64,
}

/// A verified proof and everything a destination needs from it.
pub struct Proven {
    pub mode: Mode,
    /// The program's key, `0x…`. Destination contracts pin it.
    pub program_vkey: String,
    /// The proof's output (`TxOutput::encode`), what contracts decode.
    pub public_values: Vec<u8>,
    /// The bytes an onchain SP1 verifier takes. Empty unless `Mode::Groth16`.
    pub onchain_proof: Vec<u8>,
    /// The full proof, for `save` or off-chain verification.
    pub proof: SP1ProofWithPublicValues,
    pub setup_secs: f64,
    pub prove_secs: f64,
    pub verify_secs: f64,
}

impl Proven {
    /// The JSON written to `fixtures/proofs/` and read by destination tests and relayers.
    pub fn summary(&self, run: &str) -> serde_json::Value {
        serde_json::json!({
            "run": run,
            "mode": self.mode.as_str(),
            "program_vkey": self.program_vkey,
            "public_values": hex::encode(&self.public_values),
            "proof": hex::encode(&self.onchain_proof),
            "setup_secs": self.setup_secs,
            "prove_secs": self.prove_secs,
            "verify_secs": self.verify_secs,
        })
    }
}

pub struct Huginn {
    client: EnvProver,
}

impl Huginn {
    pub async fn from_env() -> Self {
        Huginn { client: ProverClient::from_env().await }
    }

    /// Runs the program without proving. Fails unless its output equals native Rust's.
    pub async fn execute(&self, input: &TxInput) -> Result<Execution> {
        let native = native_output(input)?;
        let (public, report) = self.client.execute(ELF, stdin(input)).await.map_err(|e| anyhow!("{e}"))?;
        ensure!(public.as_slice() == native.as_slice(), "zkVM public values differ from native");
        let tracked = |k: &str| report.cycle_tracker.get(k).copied().unwrap_or(0);
        Ok(Execution {
            public_values: native,
            finality_cycles: tracked("finality"),
            inclusion_cycles: tracked("inclusion"),
            total_cycles: report.total_instruction_count(),
        })
    }

    /// Proves, then verifies the proof and checks its output equals native Rust's.
    pub async fn prove(&self, input: &TxInput, mode: Mode) -> Result<Proven> {
        let native = native_output(input)?;
        let t = Instant::now();
        let pk = self.client.setup(ELF).await.map_err(|e| anyhow!("{e}"))?;
        let setup_secs = t.elapsed().as_secs_f64();
        let vkey = pk.verifying_key().clone();

        let t = Instant::now();
        let request = self.client.prove(&pk, stdin(input));
        let proof = match mode {
            Mode::Core => request.core().await,
            Mode::Compressed => request.compressed().await,
            Mode::Groth16 => request.groth16().await,
        }
        .map_err(|e| anyhow!("{e}"))?;
        let prove_secs = t.elapsed().as_secs_f64();

        let t = Instant::now();
        self.client.verify(&proof, &vkey, None).map_err(|e| anyhow!("{e}"))?;
        let verify_secs = t.elapsed().as_secs_f64();
        ensure!(proof.public_values.as_slice() == native.as_slice(), "proof public values differ from native");

        Ok(Proven {
            mode,
            program_vkey: vkey.bytes32(),
            public_values: native,
            onchain_proof: if mode == Mode::Groth16 { proof.bytes() } else { Vec::new() },
            proof,
            setup_secs,
            prove_secs,
            verify_secs,
        })
    }

    /// The program key, without proving anything.
    pub async fn program_vkey(&self) -> Result<String> {
        Ok(self.client.setup(ELF).await.map_err(|e| anyhow!("{e}"))?.verifying_key().bytes32())
    }
}

fn stdin(input: &TxInput) -> SP1Stdin {
    let mut stdin = SP1Stdin::new();
    stdin.write(input);
    stdin
}

fn native_output(input: &TxInput) -> Result<Vec<u8>> {
    Ok(input.verify().map_err(|e| anyhow!("native verify: {e:?}"))?.encode())
}

/// A transaction signature, as base58 (what explorers show) or 128 hex characters.
pub fn parse_signature(s: &str) -> Result<[u8; 64]> {
    let bytes = if s.len() == 128 { hex::decode(s)? } else { bs58::decode(s).into_vec()? };
    bytes.try_into().ok().context("a signature is 64 bytes")
}

/// The proof input for transaction `signature` in a block file written by `ledger-dump`.
pub fn tx_input_from_block_file(block: impl AsRef<Path>, signature: &str) -> Result<TxInput> {
    let path = block.as_ref();
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?,
    )?;
    let sig = parse_signature(signature)?;
    let block = fixture::block_input(&json);
    let inclusion = TxInclusion::from_block(&block.data_shreds, block.parent_slot, block.parent_block_id, sig)
        .map_err(|e| anyhow!("transaction {signature} in {}: {e:?}", path.display()))?;
    Ok(TxInput { finality: block.finality, inclusion })
}
