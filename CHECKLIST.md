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
- [ ] 2.6 Multi-validator local cluster: moved to 7.1

## Phase 3 — inclusion
- [x] 3.1 Shreds → FEC set Merkle root (`alpenglow_lc::shred`, done with 2.4)
- [x] 3.2 FEC roots → double Merkle root = block_id (done with 2.4)
- [x] 3.3 Rebuild entries, find tx, generic parse (legacy + v0): signers, instructions. Real memo transfer (slot 46718) found byte-identical to RPC; inclusion needs only the tx's own component + all FEC roots
- [x] 3.4 Tamper tests on shreds, siblings, tx bytes: plus boundary framing, foreign/truncated shreds, wrong roots/parent, and a no-panic fuzz

## Phase 4 — SP1 + first destination (Base)
- [x] 4.1 SP1 program: finality + inclusion → chain-neutral public values (fixed byte layout, no EVM ABI inside the proof). `zk/program` + `TxOutput::encode` (v1, 106-byte header + tx); proving exposed as the `huginn-prover` library
- [ ] 4.2 Execute mode, cycle count; then Groth16 proof. **Execute mode done** (see 0.10). **Real compressed STARK proof done** (278 s, verified, matches native). Groth16 wrap left (needs Docker + circuit download, or prover network)
- [x] 4.3 `SolanaLightClient.sol` in the separate `hugin-evm` repo (depends on `huginn-prover`) (validator set hash per epoch, pinned SP1 verifier). Base first, same code on any EVM chain. With .env-driven deploy/submit scripts; deployed + submitted on anvil, `isVerified` true
- [x] 4.4 `SolanaTx.sol` library: decode signers / instructions on the destination chain. Legacy + v0, bounded counts; parses the real memo transfer exactly
- [ ] 4.5 Foundry tests with real proof fixtures, gas recorded. 19 tests pass on the real proven memo transfer (stand-in verifier). Submit 105,825 gas on anvil without the verifier (~+270k for Groth16). **Left: a real Groth16 fixture** (blocked on 4.2)

## Phase 5 — the Huginn TUI (in `hugin-evm`, binary `huginn-evm`)
One screen that runs the whole flow step by step: each step shows its status, time, the exact command or call it ran, and its output. Every step is also a plain subcommand.
- [x] 5.1 TUI shell: step list, detail/log panel, settings bar (memo, proof mode, source), keys (run all, run one, change mode, edit memo, quit). `hugin-evm/app/src/tui.rs` (ratatui); a render test prints the screen as text
- [x] 5.2 Solana steps: doctor (tools found?) → send memo → wait finalized → fetch certificate + shreds → prove (mock / compressed / groth16). `app/src/steps.rs`; works on fresh transactions
- [x] 5.3 EVM steps: start anvil → deploy → submit → `isVerified` → forged proof rejected (`InvalidProof`). All 10 steps in ~8 s with mock proofs
- [x] 5.4 The same steps as subcommands: `huginn-evm run [steps…]`, state kept in `proofs/state.json` so steps run one at a time
- [x] 5.5 `hugin-evm/docs/demo.md`: exact steps for the video, and what you should see

## Phase 6 — data sources anyone can plug in
- [ ] 6.1 `BlockSource` interface in huginn; `ledger-dump` becomes the **local-ledger** adapter (also any Agave node's ledger: devnet/mainnet operators)
- [ ] 6.2 **Yellowstone gRPC** (Triton, Helius, QuickNode, …): run the open-source plugin on our local validator, record what it exposes (shreds? footer certs?), build the adapter if possible
- [ ] 6.3 **Files** adapter (offline, tests) and source choice in the TUI
- [ ] 6.4 (Optional, needs access) Jito ShredStream, or a live provider token

## Phase 7 — more real testing
- [ ] 7.1 Multi-validator local cluster (2–3 validators): real multi-signer certificates and bitmaps (was 2.6)
- [ ] 7.2 Groth16 proof verified by SP1's real Solidity verifier on anvil; real gas (needs 4.2)
- [ ] 7.3 Base Sepolia: deploy against Succinct's verifier, submit a real proof. **You:** test wallet + faucet ETH in `.env`
- [ ] 7.4 Example `SolanaControlled.sol`: a contract on Base that only acts on transactions signed by a chosen Solana key

## Phase 8 — publish and record
- [ ] 8.1 Push `huginn`; create the `hugin-evm` GitHub repo; relayer switches to the git dependency. **You:** approve
- [ ] 8.2 Final full TUI run, docs refreshed (`codebase.md` for both repos)
- [ ] 8.3 **You:** video; one-pager

## Phase 9 — Layer 2 research note
- [ ] 9.1 Proving execution/state: bond vs lattice hash vs SIMD
