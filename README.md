# Huginn

Verify Solana transactions on any chain, using Solana's own validators.

Huginn lets a contract on another chain check, by itself, that a Solana transaction landed in a finalized block, at a given slot, signed by given keys, with given instructions. The checks are:

1. **Finality:** Alpenglow certificates (aggregated BLS signatures) checked against the epoch's validator set.
2. **Inclusion:** a Merkle path from the transaction's shreds up to the certified `block_id`.

Both run inside an SP1 zkVM program, which outputs one Groth16 proof with chain-neutral public values. Any chain that can verify a BN254 Groth16 proof can use it.

**First target: Base.** The same Solidity contracts work on other EVM chains (Ethereum, Arbitrum, Optimism, ...). Non-EVM destinations need only their own verifier wrapper.

> Status: Phase 0 (go/no-go). Nothing here is production-ready. See [`CHECKLIST.md`](CHECKLIST.md).

## What is trusted

- Solana consensus (at most 20% malicious stake, under Alpenglow's assumptions).
- The validator set posted to each destination chain every epoch. **For now we post it ourselves.** It is not yet derived trustlessly.
- SP1's proof system and the pinned verifier contract.
- BLS proof-of-possession checks done by Solana's vote program when keys are registered. Huginn does not re-check them.

## What it does not prove (yet)

Program state or execution results, for example "this transaction succeeded" or "this PDA holds X". Solana doesn't sign those. See Layer 2 in [`PLAN.md`](PLAN.md).

## Layout

```
crates/alpenglow-lc/   light client: certificates, validator set, inclusion, tx parsing (no network, no zkVM)
crates/fetch/          CLI that pulls certs, validator sets and shreds into fixtures/
sp1/                   (later) zkVM program + host script, separate workspaces
contracts/evm/         (later) Foundry: SolanaLightClient, SolanaTx, examples. Base first
fixtures/              real data from mainnet and devnet
docs/                  findings and research
```

## Docs

- [`PLAN.md`](PLAN.md): direction (Revisions 1 and 2) and the original v1 plan
- [`CHECKLIST.md`](CHECKLIST.md): build order and progress
- [`docs/findings.md`](docs/findings.md): what we confirmed against real code and data, and the corrections to the plan

## Build

```bash
cargo build --workspace
```

Pinned toolchain and crate versions are listed in `docs/findings.md`.
