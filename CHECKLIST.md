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
- [ ] 0.10 SP1 execute-mode check → go/no-go written in findings.md

## Phase 1 — finality API (`alpenglow-lc`)
- [ ] 1.1 `ValidatorSet` + commitment hash
- [ ] 1.2 `verify_finality`: fast path (80% notarize) and slow path (60% finalize + 60% notarize)
- [ ] 1.3 Adversarial tests (slow-path trap, shred_version mismatch, wrong epoch, low stake)

## Phase 2 — data fetching (local Alpenglow validator)
- [ ] 2.1 **You:** install Agave v4.4.0 and Rust 1.98.1; run `solana-test-validator --alpenglow`; `fetch --cluster local` reads its genesis cert and validator set
- [ ] 2.2 `footer-dump` Geyser plugin: writes each block footer's raw finalization certs to `fixtures/local/footers/`
- [ ] 2.3 Verify a local footer cert against the exact same-epoch set (fast path and slow path)
- [ ] 2.4 Raw shreds + block_id for slot S, read from the local ledger
- [ ] 2.5 One command producing a full fixture for any recent local slot
- [ ] 2.6 (Later) Multi-validator local cluster, so bitmaps have more than one bit and ranking is exercised

## Phase 3 — inclusion
- [ ] 3.1 Shreds → FEC set Merkle root
- [ ] 3.2 FEC roots → double Merkle root = block_id
- [ ] 3.3 Rebuild entries, find tx, generic parse (legacy + v0): signers, instructions
- [ ] 3.4 Tamper tests on shreds, siblings, tx bytes

## Phase 4 — SP1 + first destination (Base)
- [ ] 4.1 SP1 program: finality + inclusion → chain-neutral public values (fixed byte layout, no EVM ABI inside the proof)
- [ ] 4.2 Execute mode, cycle count; then Groth16 proof
- [ ] 4.3 `SolanaLightClient.sol` in `contracts/evm/` (validator set hash per epoch, pinned SP1 verifier). Base first, same code on any EVM chain
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
