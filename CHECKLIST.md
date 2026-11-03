# Huginn build checklist

Workflow: Claude completes one item, explains it in depth, you ask questions until you own it, then say "proceed".

## Phase 0 — go/no-go: can we verify a real Alpenglow certificate (natively, then in SP1)?

- [x] 0.1 Git repo, Cargo workspace, Agave crates pinned to 4.4.0
- [x] 0.2 Genesis certificates fetched from mainnet and devnet → `fixtures/*/genesis_cert.json`
- [x] 0.3 Read Agave's verifier source; decoded the certs; recorded what the plan got right and wrong
- [x] 0.4 Docs: README, PLAN.md (v1 + revision), `docs/findings.md`, research docs (you paste them)
- [x] 0.5 **You:** unblock Rust crate downloads (run `cargo fetch` in the repo from your own terminal)
- [x] 0.6 Validator set builder: vote accounts → BLS keys → ranked list (devnet epoch 1198: 22 ranked)
- [x] 0.7 Check the devnet genesis cert signature on real data (subset search over today's keys; MATCH, 11 of 22, shred_version 7016)
- [x] 0.8 Native Rust verification with `agave-bls-cert-verify`, plus tamper tests (`alpenglow_lc::cert`; real devnet genesis cert verifies)
- [x] 0.9 Decide how to get a current-epoch certificate → **local Alpenglow validator** (free; gives footers and shreds). A paid gRPC provider only if a live-network demo needs it
- [x] 0.10 SP1 execute-mode check → go/no-go written in findings.md: **GO**. Pure-Rust verifier identical to Agave (45,660 real + 400 random certs); finality + tx inclusion ≈ 47M cycles at 1,000 validators (fast), 52M (slow), 67M at 1,500

## Phase 1 — finality API (`alpenglow-lc`)
- [x] 1.1 `ValidatorSet` + commitment hash: `proof::validator_set_hash` (domain-separated SHA-256 over rank-ordered key+stake); `fetch` writes it; local epoch 0 = `b1f25236…7d3c11`
- [x] 1.2 `verify_finality`: fast path (80% notarize) and slow path (60% finalize + 60% notarize): `FinalityInput::verify → Finality { path, stake, set hash }`
- [x] 1.3 Adversarial tests (slow-path trap, shred_version mismatch, wrong epoch, low stake), plus stake inflation vs commitment and malformed sets (`alpenglow-core/tests/proof.rs`)

## Phase 2 — data fetching (local Alpenglow validator)
- [x] 2.1 Local Alpenglow validator running (Agave v4.4.0, Rust 1.98.1 installed); `fetch --cluster local` wrote `fixtures/local/` (shred 28090, 1 ranked validator)
- [x] 2.2 `footer-dump` Geyser plugin (`plugins/footer-dump`, Rust 1.98.1) writes each footer's raw certs to `fixtures/local/footers.jsonl`
- [x] 2.3 Verify a local footer cert against the exact same-epoch set: `cert::verify_footer`; all 40,999 captured certs pass, slow path synthetic
- [x] 2.4 Raw shreds + block_id for slot S, read from the local ledger: `ledger-dump --slot S` (RocksDB read-only) → `fixtures/local/block_44000.json`; rebuilt block_id = certified block_id for 211/211 slots
- [x] 2.5 One command producing a full fixture for any recent local slot: `cargo run --release -p ledger-dump` (newest certified slot, or `--slot S`); verifies both certs and the block_id before writing a self-contained `block_<S>.json`
- [ ] 2.6 (Later) Multi-validator local cluster, so bitmaps have more than one bit and ranking is exercised

## Phase 3 — inclusion
- [x] 3.1 Shreds → FEC set Merkle root (`alpenglow_lc::shred`, done with 2.4)
- [x] 3.2 FEC roots → double Merkle root = block_id (done with 2.4)
- [x] 3.3 Rebuild entries, find tx, generic parse (legacy + v0): signers, instructions. Real memo transfer (slot 46718) found byte-identical to RPC; inclusion needs only the tx's own component + all FEC roots
- [x] 3.4 Tamper tests on shreds, siblings, tx bytes: plus boundary framing, foreign/truncated shreds, wrong roots/parent, and a no-panic fuzz

## Phase 4 — SP1 + first destination (Base)
- [ ] 4.1 SP1 program: finality + inclusion → chain-neutral public values (fixed byte layout, no EVM ABI inside the proof)
- [ ] 4.2 Execute mode, cycle count; then Groth16 proof. **Execute mode done** (see 0.10). **Real compressed STARK proof done** (278 s, verified, matches native). Groth16 wrap left (needs Docker + circuit download, or prover network)
- [ ] 4.3 `SolanaLightClient.sol` in the separate `hugin-evm` repo (depends on `huginn-prover`) (validator set hash per epoch, pinned SP1 verifier). Base first, same code on any EVM chain
- [ ] 4.4 `SolanaTx.sol` library: decode signers / instructions on the destination chain
- [ ] 4.5 Foundry tests with real proof fixtures, gas recorded

## Phase 5 — relayer
- [ ] 5.1 `relayer`: tx signature → fetch → prove → submit, with destination as a plug-in (EVM first)

## Phase 6 — demos
- [ ] 6.1 Cross-chain ping (Memo)
- [ ] 6.2 Solana key controlling a contract on Base
- [ ] 6.3 Forged proof rejected
- [ ] 6.4 One-pager + video

## Phase 7 — Layer 2 research note
- [ ] 7.1 Proving execution/state: bond vs lattice hash vs SIMD
