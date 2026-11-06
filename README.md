# Huginn

**Verify Solana transactions on any chain, using Solana's own validators.**

A contract on another chain can check, by itself, that a Solana transaction landed in a finalized block, at a given slot, signed by given keys, with given instructions. No bridge committee: the check is Solana's own validator signatures, proven in a zero-knowledge program.

- **Finality:** Alpenglow certificates (aggregated BLS signatures) checked against the epoch's validator set.
- **Inclusion:** a Merkle path from the transaction's shreds up to the certified `block_id`.
- **One proof:** both run inside an SP1 zkVM program that outputs a Groth16 proof any EVM chain can verify.

Base is the first destination. The EVM contracts and the guided app live in [`huginn-evm`](https://github.com/AlphaR2/huginn-evm).

> **Status: experimental.** It works end to end on a local Alpenglow chain (a transaction is sent, finalized, proven and verified in a contract). Real Groth16 on a public chain and multi-validator real certificates are not done yet. See [`docs/security.md`](docs/security.md). Do not put value behind it.

## Read next

| If you want to | Read |
|---|---|
| understand how it works | [`docs/architecture.md`](docs/architecture.md) |
| run it | [`docs/run.md`](docs/run.md) |
| find your way around the code | [`docs/codebase.md`](docs/codebase.md) |
| judge the risks | [`docs/security.md`](docs/security.md) |
| see the research and design history | [`docs/research/`](docs/research/) |

## Quick check

```bash
cargo test --release     # the verifier, against real blocks and Agave's own code
```

Full setup (tools, validator, builds, the TUI): [`docs/run.md`](docs/run.md).

## Layout

```
crates/alpenglow-core/   the verifier: certificates, block_id, shreds, transactions. Pure Rust, runs inside SP1
crates/alpenglow-lc/     cross-checks against Agave's own crates; validator set ranking
crates/fetch/            CLI: genesis certs, shred version, validator sets from RPC
crates/ledger-dump/      block fixtures from a ledger or shred files, every check done first
plugins/footer-dump/     optional Geyser plugin that captures block footers
zk/program/              the SP1 program
zk/prover/               huginn-prover crate and `huginn` CLI
fixtures/                real data from mainnet, devnet and a local Alpenglow validator
docs/                    architecture, setup, security, research
```

## License

Apache-2.0, as declared in `Cargo.toml`.
