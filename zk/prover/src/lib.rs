//! Huginn prover: proves "this Solana transaction is in a block finalized by this
//! validator set" with SP1, for any destination chain.
//!
//! A destination app (for example `huginn-evm`) depends on this crate:
//!
//! ```ignore
//! let input = huginn_prover::tx_input_from_block_file("block_46718.json", "4F39ig6…")?;
//! let huginn = huginn_prover::Huginn::from_env().await;
//! let proven = huginn.prove(&input, Mode::Groth16).await?;
//! // proven.public_values and proven.onchain_proof go to the destination's verifier.
//! ```
//!
//! Where proving happens follows SP1's own environment: `SP1_PROVER=cpu` (default) or
//! `mock`, or `network` when this crate is built with `--features network` (the account key
//! comes from `NETWORK_PRIVATE_KEY` in the environment only, never an argument).

use {
    alpenglow_core::{
        fixture,
        proof::{TxInclusion, TxInput},
    },
    anyhow::{anyhow, bail, ensure, Context, Result},
    sp1_sdk::{
        env::{EnvProver, EnvProvingKey},
        include_elf, prelude::*, Elf, HashableKey, ProverClient, SP1ProofWithPublicValues, SP1Stdin,
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
        let proof = match self.prove_on_network(&pk, input, mode).await {
            Some(result) => result?,
            None => {
                let request = self.client.prove(&pk, stdin(input));
                match mode {
                    Mode::Core => request.core().await,
                    Mode::Compressed => request.compressed().await,
                    Mode::Groth16 => request.groth16().await,
                }
                .map_err(|e| anyhow!("{e}"))?
            }
        };
        let prove_secs = t.elapsed().as_secs_f64();

        let t = Instant::now();
        self.client.verify(&proof, &vkey, None).map_err(|e| anyhow!("{e}"))?;
        ensure!(proof.public_values.as_slice() == native.as_slice(), "proof public values differ from native");
        let program_vkey = vkey.bytes32();
        let onchain_proof = if mode == Mode::Groth16 { proof.bytes() } else { Vec::new() };
        // The mock prover makes no real proof (its "Groth16" is a placeholder for the mock
        // verifier), so there is nothing to check; every real prover's proof is checked.
        if mode == Mode::Groth16 && !matches!(self.client, EnvProver::Mock(_)) {
            check_onchain_bytes(&onchain_proof, &native, &program_vkey)?;
        }
        let verify_secs = t.elapsed().as_secs_f64();

        Ok(Proven {
            mode,
            program_vkey,
            public_values: native,
            onchain_proof,
            proof,
            setup_secs,
            prove_secs,
            verify_secs,
        })
    }

    /// With `SP1_PROVER=network`, sends the request with Huginn's auction settings
    /// ([`network_settings`]) instead of the SDK's defaults. `None` for every other prover.
    #[cfg(feature = "network")]
    async fn prove_on_network(
        &self,
        pk: &EnvProvingKey,
        input: &TxInput,
        mode: Mode,
    ) -> Option<Result<SP1ProofWithPublicValues>> {
        let (EnvProver::Network(prover), EnvProvingKey::Network { pk, .. }) = (&self.client, pk) else {
            return None;
        };
        Some(
            async {
                let request = prover
                    .prove(pk, stdin(input))
                    .strategy(sp1_sdk::network::FulfillmentStrategy::Auction)
                    .min_auction_period(network_settings::MIN_AUCTION_PERIOD_SECS)
                    .auction_timeout(network_settings::AUCTION_TIMEOUT)
                    .max_price_per_pgu_buffer(network_settings::PRICE_BUFFER_PCT)
                    .timeout(network_settings::PROOF_TIMEOUT);
                match mode {
                    // The network does not accept core proofs.
                    Mode::Core => bail!("the prover network does not make core proofs; use compressed or groth16"),
                    Mode::Compressed => request.compressed().await,
                    Mode::Groth16 => request.groth16().await,
                }
                .map_err(|e| anyhow!("{e}"))
            }
            .await,
        )
    }

    #[cfg(not(feature = "network"))]
    async fn prove_on_network(
        &self,
        _: &EnvProvingKey,
        _: &TxInput,
        _: Mode,
    ) -> Option<Result<SP1ProofWithPublicValues>> {
        None
    }

    /// The program key, without proving anything.
    pub async fn program_vkey(&self) -> Result<String> {
        Ok(self.client.setup(ELF).await.map_err(|e| anyhow!("{e}"))?.verifying_key().bytes32())
    }
}

/// Auction settings for Succinct's prover network (a marketplace where provers bid to make
/// the proof; the lowest bid wins and is paid in PROVE).
pub mod network_settings {
    use std::time::Duration;

    /// Wait this long for bids before settling, so every prover gets to bid. The SDK default
    /// is 1 s; Succinct recommends 10–15 s when latency doesn't matter.
    pub const MIN_AUCTION_PERIOD_SECS: u64 = 15;
    /// Cancel the request if no prover bids within this time (SDK default: 30 s).
    pub const AUCTION_TIMEOUT: Duration = Duration::from_secs(120);
    /// The most we accept per unit of proving work, as a percentage of the network's current
    /// market price. 120 is the SDK's default; written here so it is a decision, not a default
    /// that can change under us.
    pub const PRICE_BUFFER_PCT: u64 = 120;
    /// Abort if the winning prover hasn't delivered by then. Groth16 for this program takes
    /// minutes; 30 minutes leaves room without letting a stuck request hang the run.
    pub const PROOF_TIMEOUT: Duration = Duration::from_secs(30 * 60);
}

/// Checks the exact bytes a destination contract will receive, with `sp1-verifier`: the
/// same Groth16 check the Solidity verifier does, in plain Rust, against SP1 v6.10.0's pinned
/// Groth16 key. Independent of the SDK's own `verify`, and it also confirms the proof's
/// 4-byte selector is the one Succinct's gateways route for this SP1 version.
fn check_onchain_bytes(onchain_proof: &[u8], public_values: &[u8], program_vkey: &str) -> Result<()> {
    sp1_verifier::Groth16Verifier::verify(
        onchain_proof,
        public_values,
        program_vkey,
        &sp1_verifier::GROTH16_VK_BYTES,
    )
    .map_err(|e| anyhow!("the on-chain proof bytes do not verify off-chain: {e}"))
}

/// The 4-byte selector at the start of every Groth16 proof from this SP1 version. Succinct's
/// gateways route by it; on Base Sepolia it routes to an unfrozen verifier (checked 14 Nov 2026).
pub fn groth16_selector() -> [u8; 4] {
    use sha2::{Digest, Sha256};
    Sha256::digest(*sp1_verifier::GROTH16_VK_BYTES)[..4].try_into().expect("4 bytes")
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The selector Succinct's Base Sepolia gateway was checked to route (to an unfrozen
    /// verifier). If an SP1 upgrade changes it, the gateway route must be re-checked.
    #[test]
    fn groth16_selector_is_the_routed_one() {
        assert_eq!(hex::encode(groth16_selector()), "4388a21c");
    }

    #[test]
    fn check_onchain_bytes_rejects_bad_proofs() {
        let vkey = format!("0x{}", "11".repeat(32));
        // Too short.
        assert!(check_onchain_bytes(&[0u8; 10], b"out", &vkey).is_err());
        // Right length, wrong selector.
        assert!(check_onchain_bytes(&[0u8; 4 + 32 * 8], b"out", &vkey).is_err());
        // Right selector, garbage proof.
        let mut fake = groth16_selector().to_vec();
        fake.extend([7u8; 32 * 8]);
        assert!(check_onchain_bytes(&fake, b"out", &vkey).is_err());
    }
}
