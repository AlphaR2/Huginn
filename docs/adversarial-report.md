# Adversarial review: huginn and huginn-evm

Reviewed 2026-11-04 against the working trees of huginn and huginn-evm (`hugin-evm` on disk, @ `3c6bf84`).
Reviewer: Claude (outside review, not part of the original build).

## What the system is trying to do

Let a contract on any EVM chain check, without a bridge or multisig, that a Solana transaction
was in a block Solana's own validators finalized. `huginn` does the Solana side in an SP1
program: Alpenglow BLS certificate check (finality), Merkle path from shreds to `block_id`
(inclusion), one Groth16 proof out. `huginn-evm` is the first destination: `SolanaLightClient`
(proof check + validator-set pinning), `SolanaTx` (parse the proven transaction), and
`SolanaControlled` (a contract a Solana key controls through Memo transactions).
Layer 1 only: it proves *inclusion*, never *success* or program state.

## How the work was done, and what was and was not run

| Part | Method | Run? |
|---|---|---|
| `alpenglow-core` (certificate, Merkle, shred, tx parser) | read every line; 20 new adversarial tests | **Yes**, 45/45 pass (18 + 5 existing, 20 new, 2 unit) |
| Transaction parser vs Agave | differential fuzz, 3,000,000 inputs against `solana-transaction`/`solana-message` 5.1.0 (the versions Huginn pins) | **Yes** |
| Solidity contracts | read every line; 8 proof-of-concept tests in `hugin-evm/test/Adversarial.t.sol` | **No.** Foundry is not installed on this machine, and a download was blocked. The PoCs are written but unexecuted; run `forge test --match-contract Adversarial -vv` |
| Relayer / TUI | read `steps.rs`, `main.rs`, `tui.rs` | No (needs SP1 toolchain, Foundry, Solana CLI, a running validator) |
| SP1 circuit, Groth16 verifier, ledger-dump, footer-dump plugin | not reviewed | - |

**The Solidity findings are from code reading and are not yet confirmed by a run.**

## Verdict

The cryptographic core is carefully built. I tried to break it in about 25 ways and it held
everywhere it is supposed to. The serious risks are not in the proof logic. They are in
**who is trusted** (one owner key) and in **how `SolanaControlled` behaves** once a proof is valid.
Do not put value behind either until findings 1 to 3 are decided.

## Findings

Severity is my judgement of impact if this shipped as is.

### 1. HIGH: `SolanaControlled` has no ordering, expiry, or deployment binding
`SolanaControlled.sol:execute`. A valid proof only has to be new (`executed[sigHash]`). Anyone can relay any controller-signed command, at any time, in any order, to any contract with the same `target` name.
- **Out of order.** Controller signs "set A", then "set B". An attacker relays B then A. Final state is A. (`test_POC_commandsCanBeRelayedOutOfOrder`)
- **Across deployments.** A command signed for greeter v1 also runs on a v2 with the same controller and target name. (`..._oldCommandReplaysOnANewDeployment`)
- **No expiry.** A command never relayed stays valid forever. (`..._unrelayedCommandNeverExpires`)

The README tells integrators this is "the easiest way" to build, so every app inherits it. For a greeter it is cosmetic. For "pause", "set owner" or "withdraw" it is a real exploit.
**Fix:** put a per-controller nonce in the memo (`huginn:<chain>:<contract address>:<nonce>:<cmd>`), require nonce = last + 1, and bind the contract's address, not a free-text name.

### 2. HIGH (documented, but worse than the docs say): the owner key is the whole root of trust
`SolanaLightClient.storeValidatorSet`. The owner stores the hash of the validator set for each epoch. An owner can store the hash of a set whose BLS keys it holds, prove any transaction signed by anyone, and every consumer accepts it. (`test_POC_ownerKeyForgesAnyControllerCommand`)
Things the README does not say:
- **Write-once makes a typo permanent.** A wrong set hash for epoch N makes epoch N unprovable forever. (`..._wrongSetForAnEpochBricksItForever`)
- **Nothing can be repointed.** `verifier`, `programVKey` and each app's `huginn` pointer are immutable, so recovery is redeploying the client and every consumer.
- **`setOwner` is one step**, so a mistyped address strands the client. (`..._setOwnerHasNoTwoStepConfirmation`)
- A single EOA owner, no timelock, no way for consumers to see a pending change.

**Fix, near term:** multisig owner, two-step ownership, a delay between "propose set" and "usable" that consumers can watch, and a way to supersede a bad epoch. **Real fix:** prove epoch-to-epoch handover in SP1 (already on your roadmap) so the owner is not needed.

### 3. MEDIUM: a failed Solana transaction still commands the EVM contract
Documented in the code, but it is a sharp edge in the base class. Solana puts failed transactions in blocks, and the proof shows inclusion only. A controller who sends "memo + transfer" intending the command to run only if the transfer succeeds will see the EVM command run even when the transfer failed. The relayer's *finalize* step reports a failure to the person watching, but nothing on-chain checks it.
**Fix:** document it loudly in `SolanaControlled`'s API comments, and design commands as self-contained. Real fix needs Layer 2 (prove success).

### 4. MEDIUM: one unparseable transaction blocks proofs for every other transaction in its component
**Verified by test** (`one_unparseable_transaction_poisons_its_whole_component`). `block::parse_component` fails the whole component if any transaction fails to parse. The Agave SDK Huginn pins already defines **V1 transactions** (prefix `0x81`, signatures *after* the message, up to 4096 bytes), and Huginn rejects them. If V1 is or becomes active on a cluster, anyone can land a cheap V1 transaction next to a target and make the target unprovable (censorship, not forgery). **I did not check whether V1 is active on any live cluster.**
**Fix:** implement the V1 layout in `tx::parse`, and decide a policy for unknown future versions (they cannot be skipped without knowing their length).

### 5. LOW-MEDIUM: the replay guard is the first signature, not the message
`executed[keccak(signature)]`. If Solana ever accepts two valid signatures for one message (ed25519 malleability), one command executes twice. (`..._replayGuardIsKeyedOnSignatureNotMessage` shows the contract-level property.) I could not confirm whether current Agave sigverify is strict about this; treat as unverified hardening. **Fix:** key on a hash of the message bytes.

### 6. LOW: relayer can sign a real-chain deploy with the public Anvil key
`steps.rs:is_local_evm` decides "local" by whether the RPC URL *contains* `127.0.0.1` or `localhost`, then uses Anvil's public test key if `PRIVATE_KEY` is empty. An SSH tunnel to a remote node on `localhost:8545`, plus an empty `PRIVATE_KEY` and a set `SP1_VERIFIER`, deploys with a publicly known key, making *everyone* the owner (finding 2). `Deploy.s.sol` only checks chain id when `SP1_VERIFIER` is empty. Also `cast send --private-key <key>` puts the real key in the process list (`steps.rs:692`).
**Fix:** use chain id 31337 as "local", never fall back to a default key on any other chain, pass keys via env or keystore.

### 7. LOW: `verify` ignores the conflicting-block record
`submit` reverts `ConflictingBlock`, but `verify` (used by apps) never reads `finalizedBlock`. Only matters if Solana itself forks finality. (`..._applicationsBypassTheConflictingBlockCheck`)

### 8. INFO: parser differences from Agave, no exploit
Huginn (and `SolanaTx.sol`) accept a v0 lookup entry that loads zero accounts; Agave refuses it. Across 3M random transactions this was the **only** difference: 13,207 cases, 0 field mismatches, 0 valid transactions Huginn rejects. Duplicate account keys also parse, and Agave refuses them at runtime. A transaction Agave refuses cannot be in a finalized block, so this cannot be exploited through proofs.
Also: `SolanaTx.sol` says it is "safe on unproven input". It is memory-safe, but signer claims on unproven bytes are unauthenticated.

## What held up (negative results, all by test)

- **Rogue key.** Demonstrated: with one key in the set that has no proof of possession, an attacker forges 100% finality alone (`rogue_key_forges_finality_...`). This is not a code bug: Huginn relies on Solana's vote program having checked proof of possession and on the owner posting only those keys. It is a hard dependency; keep it in the trust list.
- Identity key / cancelling keys + identity signature: rejected.
- Exact 60% and 80% thresholds (59 fails, 60 and 80 pass); `u64` stake overflow refused; threshold math safe with huge stakes.
- Notarize/Finalize certificates cannot be swapped; shred-version cross-cluster signatures fail; claiming extra signers fails.
- Bitmap framing (base3, wrong length, extra bytes, bits past the set) rejected. Spare bits in the last byte are malleable but harmless.
- Merkle odd-leaf duplication (CVE-2012-2459 style): the raw tree *is* malleable, and the count leaf in `block_id` blocks it. Shown for 1 to 9 FEC sets.
- Transaction parser: non-canonical lengths, huge counts, trailing bytes, bad indexes all rejected; 400k mutations never panic.
- The only thing a contract trusts from the proof is checked three ways on-chain: program key, shred version, set hash for the epoch of the slot.

## Not tested, and where the residual risk is

1. **Real Groth16 has never been verified**, neither on-chain nor in tests (`Groth16.t.sol` is skipped until a proof exists). The deployed path on a real chain is untested end to end.
2. **Real certificates have one signer.** Multi-signer bitmaps and validator ranking are tested only with synthetic sets and one devnet genesis certificate. The match between your rank order and Agave's on a large live set is the least-tested assumption.
3. `slotsPerEpoch` is a fixed immutable, so a cluster with epoch warmup would map slots to the wrong epoch.
4. Not reviewed: the SP1 program as compiled, the vendored `src/sp1/` verifier (README says byte-identical), `ledger-dump`, the Geyser plugin.

## Usability, as a first-time user

I could not launch the TUI here, so this is a read-through of the docs and the screens' own text, from the point of view of a Solidity developer who wants "my contract reacts to a Solana transaction".

**Ease of use: 2 / 5 to get running, 4 / 5 once running.** **Intuitiveness: 4 / 5.**

What works well
- The mental model is right on screen: type a message, watch four stages, get a plain-sentence result card.
- `d` shows the exact command behind every step, so nothing is magic and it teaches the pipeline.
- "practice / real (STARK) / real (Groth16)" are good names, and each says how long it takes.
- The `doctor` step names what is missing and gives a fix line. Failures say "It stopped here" plus a next action.
- `inherit SolanaControlled; implement _execute(command)` is a very small API for the common case.
- Every step is also a CLI command and state is saved, so it scripts and resumes.

What gets in the way
- **Setup is heavy.** Two repos side by side, Rust, SP1 (pinned 6.10.0), Foundry, `protoc`, the Solana CLI (Agave 4.4), a local validator left running (ledger grows by GBs a day), three separate builds, and `.env` edits. The honest first-run time is hours, not minutes.
- **Setup instructions are spread across three docs** (README, `demo.md`, `run.md`) and they disagree on small things. The `doctor` fix line says to start the validator with the footer plugin, while the docs say the plugin is optional.
- **Naming is confusing:** the folder here is `hugin-evm`, docs and URLs say `huginn-evm`, and the CLI tool, repo and crate use three spellings.
- **The practice-mode result is misleading.** A green "Done. A contract on <chain> trusts your Solana message." appears even though the stand-in verifier accepts anyone's fake proof. The caveat is one dim line underneath. A newcomer will read "trusted".
- **Sharp edges are invisible in the API:** no nonce, failed transactions still count, `SolanaTx.account()` reverts for accounts loaded through lookup tables. None of it shows up where a developer writes code.
- Contract errors are bare custom errors (`InvalidProof`, `NoCommand`); the TUI translates some, but a contract developer calling `execute` directly gets only the selector.
- Terminal must be at least 150 x 34, per the demo doc; I could not see what happens when it is smaller.
- A real chain needs 32 GB for Groth16 or a rented VPS, so a developer cannot get from "demo works" to "my contract on Base Sepolia" on a laptop.

Suggested fixes, in order of payoff: make the practice-mode card say "practice proof: a real chain would reject this"; one "Getting started" page with a single path and a `doctor`-first step; rename for consistency; surface the failed-tx and ordering warnings in `SolanaControlled`'s doc comment and the demo.

## Files added

- `crates/alpenglow-core/tests/adversarial.rs`: 20 tests, all pass.
- `hugin-evm/test/Adversarial.t.sol`: 8 PoCs, **not yet run**.
- This report.
- The differential fuzzer lives outside the repo in the session scratchpad (`.../scratchpad/difffuzz`) and can be copied in if you want it kept.

## Status after the first round of fixes (2026-11-04)

Added by the maintainers after the review. The Solidity PoCs now run: `forge test` in huginn-evm, 39 pass (5 Groth16 tests skipped until a real proof exists). One PoC had a test bug: `expectRevert` with a bare selector for an error that has arguments. It was fixed when the test was flipped. The Rust side: 94 tests across the huginn workspace pass (the 45 above are `alpenglow-core` alone).

| # | Finding | Status |
|---|---|---|
| 1 | `SolanaControlled`: no ordering, expiry, deployment binding | **Fixed.** Memo is `huginn:<chain id>:<contract address>:<nonce>:<command>`; the nonce must be exactly `nonce + 1`. An empty command cancels a signed, unrelayed one (there is still no time-based expiry). PoCs flipped to `test_FIXED_*` |
| 2 | Owner key is the root of trust | **Open.** Next: two-step ownership, sets that take effect after a delay and can be replaced before then, a multisig owner. Real fix: prove the set handover in SP1. `test_POC_*` still pass |
| 3 | Failed transactions still command | **Documented** in `SolanaControlled`'s comment and huginn-evm's README ("Using it"). Real fix: prove success (Layer 2) |
| 4 | One unparseable transaction blocks its component | **Fixed (2026-11-05).** v1 *is* live: mainnet since epoch 1035, also testnet and devnet, so this was a real censorship vector. `tx.rs` now parses v1 and matches Agave exactly on 3 million random and mutated transactions (300,000 of them as the test `tx_matches_agave.rs`). Inclusion parses a component only up to the target (`find_in_component`), so nothing after it can block it. `SolanaTx.sol` reads v1 too. Left: a format Huginn doesn't know, placed *before* the target, still hides it; that needs a new Solana transaction format, and the parser must follow Agave's releases. PoC flipped (`fixed_a_neighbouring_transaction_no_longer_blocks_the_target`) |
| 5 | Replay guard keyed on the signature | **Fixed** by finding 1: the guard is the nonce inside the signed message |
| 6 | anvil key whenever the RPC URL says localhost; key on the command line | **Fixed.** The test key is used only when the chain reports chain id 31337. Every key goes to forge through the environment (`cast send --private-key` replaced by `script/Execute.s.sol`) |
| 7 | `verify` ignores the conflicting-block record | **Fixed.** `verify` reverts with `ConflictingBlock` too. PoC flipped |
| 8 | Parser differences from Agave | No change needed in code. `SolanaTx`'s comment now says unproven input reverts cleanly but means nothing. The one over-accept (a v0 lookup table loading no accounts) is now refused too |
| UX | Practice-mode card overclaims | **Fixed.** Practice: "The flow works, but nothing was proven". STARK: checked on this machine, not on-chain. Only Groth16 says the contract checked a real proof |
| UX | `doctor` says the plugin is needed | **Fixed** |
| UX | Naming, one setup guide | Open |

The deployment binding changed the app's order: the contracts are deployed **before** the memo is sent (the memo names the greeter's address), so the guided view has five stages (Set up, Send, Finalize, Prove, Verify).
