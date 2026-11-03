# Huginn

Verify Solana transactions on any chain, using Solana's own validators.

Huginn lets a contract on another chain check, by itself, that a Solana transaction landed in a finalized block, at a given slot, signed by given keys, with given instructions. The checks are:

1. **Finality:** Alpenglow certificates (aggregated BLS signatures) checked against the epoch's validator set.
2. **Inclusion:** a Merkle path from the transaction's shreds up to the certified `block_id`.

Both run inside an SP1 zkVM program, which outputs one Groth16 proof with chain-neutral public values. Any chain that can verify a BN254 Groth16 proof can use it.

**First target: Base.** Destinations are separate codebases that depend on the `huginn-prover` crate. The EVM one is [`hugin-evm`](../hugin-evm): Solidity contracts, a relayer, deploy/submit scripts. The same code works on any EVM chain (Ethereum, Arbitrum, Optimism, ...). A non-EVM chain gets its own repo the same way.

> Status: native verifier complete (finality + transaction inclusion, on real local Alpenglow data); SP1 cycle measurement in progress. Nothing here is production-ready. See [`CHECKLIST.md`](CHECKLIST.md).

## What is trusted

- Solana consensus (at most 20% malicious stake, under Alpenglow's assumptions).
- The validator set posted to each destination chain every epoch. **For now we post it ourselves.** It is not yet derived trustlessly.
- SP1's proof system and the pinned verifier contract.
- BLS proof-of-possession checks done by Solana's vote program when keys are registered. Huginn does not re-check them.

## What it does not prove (yet)

Program state or execution results, for example "this transaction succeeded" or "this PDA holds X". Solana doesn't sign those. See Layer 2 in [`PLAN.md`](PLAN.md).

## Layout

```
crates/alpenglow-core/  the verifier: certificates, block_id, transactions. Pure Rust, runs inside SP1
crates/alpenglow-lc/    reference checks on Agave's own crates; validator set ranking
crates/fetch/           CLI: genesis certs, shred version, validator sets (with commitment) from RPC
crates/ledger-dump/     CLI: one block's shreds from a local ledger -> verified, self-contained fixture
plugins/footer-dump/    Geyser plugin that captures every block footer's finality certificate
zk/program/             SP1 program: proves finality + one transaction, commits fixed-layout public values
zk/prover/              huginn-prover crate (prove a tx; what relayers depend on) + `huginn` CLI
fixtures/               real data from mainnet, devnet and the local Alpenglow validator
docs/                   findings, codebase guide, research
```

## Docs

- [`PLAN.md`](PLAN.md): direction (Revisions 1 and 2) and the original v1 plan
- [`CHECKLIST.md`](CHECKLIST.md): build order and progress
- [`docs/findings.md`](docs/findings.md): what we confirmed against real code and data, and the corrections to the plan
- [`docs/codebase.md`](docs/codebase.md): a guided tour of every folder and file

## Build

```bash
cargo build --workspace
```

Pinned toolchain and crate versions are listed in `docs/findings.md`.
