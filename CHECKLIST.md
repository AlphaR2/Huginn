# Huginn build checklist

Workflow: Claude completes one item, explains it in depth, you ask questions until you own it, then say "proceed".

## Phase 0 — go/no-go: can we verify a real Alpenglow certificate (natively, then in SP1)?

- [x] 0.1 Git repo, Cargo workspace, Agave crates pinned to 4.4.0
- [x] 0.2 Genesis certificates fetched from mainnet and devnet → `fixtures/*/genesis_cert.json`
- [x] 0.3 Read Agave's verifier source; decoded the certs; recorded what the plan got right and wrong
- [x] 0.4 Docs: README, PLAN.md (v1 + revision), `docs/findings.md`, research docs (you paste them)
- [x] 0.5 **You:** unblock Rust crate downloads (run `cargo fetch` in the repo from your own terminal)
- [ ] 0.6 Validator set builder: vote accounts → BLS keys → ranked list (devnet first, 17 validators)
- [ ] 0.7 Independent check: verify the devnet genesis cert in JS with `@noble/curves`
- [ ] 0.8 Native Rust verification with `agave-bls-cert-verify`, plus a one-bit tamper test
- [ ] 0.9 Decide how to get a current-epoch certificate (gRPC provider vs own devnet node)
- [ ] 0.10 SP1 execute-mode check → go/no-go written in findings.md

## Phase 1 — finality API (`alpenglow-lc`)
- [ ] 1.1 `ValidatorSet` + commitment hash
- [ ] 1.2 `verify_finality`: fast path (80% notarize) and slow path (60% finalize + 60% notarize)
- [ ] 1.3 Adversarial tests (slow-path trap, shred_version mismatch, wrong epoch, low stake)

## Phase 2 — data fetching (`fetch` CLI)
- [ ] 2.1 Certificates for slot S (footers / cert pool)
- [ ] 2.2 Raw shreds + block_id for slot S
- [ ] 2.3 One command producing a full fixture for any recent devnet slot

## Phase 3 — inclusion
- [ ] 3.1 Shreds → FEC set Merkle root
- [ ] 3.2 FEC roots → double Merkle root = block_id
- [ ] 3.3 Rebuild entries, find tx, generic parse (legacy + v0): signers, instructions
- [ ] 3.4 Tamper tests on shreds, siblings, tx bytes

## Phase 4 — SP1 + Base
- [ ] 4.1 SP1 program: finality + inclusion → generic public values
- [ ] 4.2 Execute mode, cycle count; then Groth16 proof
- [ ] 4.3 `SolanaLightClient.sol` (validator set hash per epoch, pinned SP1 verifier)
- [ ] 4.4 `SolanaTx.sol` library: decode signers / instructions on Base
- [ ] 4.5 Foundry tests with real proof fixtures, gas recorded

## Phase 5 — relayer
- [ ] 5.1 `relayer`: tx signature → fetch → prove → submit

## Phase 6 — demos
- [ ] 6.1 Cross-chain ping (Memo)
- [ ] 6.2 Solana key controlling a Base contract
- [ ] 6.3 Forged proof rejected
- [ ] 6.4 One-pager + video

## Phase 7 — Layer 2 research note
- [ ] 7.1 Proving execution/state: bond vs lattice hash vs SIMD
