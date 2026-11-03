# Findings

A running log of what we confirmed against real code and data. Where this file disagrees with `PLAN.md`, this file wins. Newest entries go at the top of each section.

## Pinned versions

| Item | Version | Notes |
|---|---|---|
| rustc | 1.99.1 | |
| agave-bls-cert-verify | =4.4.0 | Needs Cargo feature `agave-unstable-api` |
| agave-votor-messages | =4.4.0 | |
| solana-bls-signatures | =4.0.2 | Same version Agave 4.4.0 resolves to |
| solana-signer-store | 0.1.0 | Transitive. Bitmap encoding |
| rayon | 1.12.0 | Transitive. Unused at runtime if `thread_pool = None` |
| wincode | 0.6.3 | Transitive. Vote payload serialization |
| Solana CLI | 3.1.10 | **Upgrade to v4.4.0 for 2.1** (`solana-test-validator --alpenglow`) |
| SP1 (`cargo prove`) | not installed | Needed for 0.10 |
| Foundry | not installed | Needed for Phase 4 |
| rustc for Geyser plugin | 1.98.1 | Must match Agave 4.4.0's toolchain (dylib ABI) |
| Anchor | 1.1.2 | Only needed if we build the optional outbox |
| SP1 | 6.10.0 (2026-10-23) | `sp1-zkvm`, `sp1-sdk`, `sp1-build` =6.10.0. Patches: `bls12_381` patch-0.8.0-sp1-6.10.0, `sha2` patch-sha2-0.10.8/0.9.9-sp1-6.10.0 |
| bls12_381 (alpenglow-core) | =0.8.0 | The version SP1 patches. `experimental` for hash_to_curve, which needs a digest-0.9 hasher (sha2 0.9) |
| rocksdb (ledger-dump) | =0.24.0 | librocksdb-sys 0.17.3+10.4.2, same as Agave 4.4.0. Features `lz4`, `bindgen-runtime` (finds macOS libclang without env vars) |

## Environment

- **2026-11-01 (later):** Since the session moved into the repo folder, Claude's sandbox can't reach Solana public RPC either. The user runs `cargo run -p fetch -- ...` and commits the fixtures; Claude works from them.
- **2026-11-01:** Claude's sandbox can't reach `static.crates.io`. The user runs `cargo fetch`, and Claude builds from the local cache. `raw.githubusercontent.com`, the npm registry and Solana public RPC are reachable.

## Confirmed facts

### Clusters
- **Alpenglow is live on devnet.** `getAgGenesisCert` answers on both clusters. *(PLAN §6 Confirm: resolved)*
- Mainnet genesis cert: slot 459,652,999, epoch 1064. That's one slot before the activation at 459,653,000.
- Devnet genesis cert: slot 504,148,999, epoch 1167.
- At the time of writing: mainnet epoch 1074, devnet epoch 1197. Both use 432,000 slots per epoch.

### Genesis certificate contents
| | mainnet | devnet |
|---|---|---|
| bitmap encoding | base2 (version 0) | base2 |
| bitmap length (= ranked set size) | 672 | 17 |
| signers | 446 | 11 |
| signature size from RPC | 192 bytes (uncompressed G2) | 192 bytes |

- The 446 signers match the research. The set size of **672** replaces the research figure of "~654 participating".

### Certificate verification API (`agave_bls_cert_verify::cert_verify::verify_certificate`)
```rust
verify_certificate(cert: UnverifiedCertificate, max_validators: usize, total_stake: NonZero<u64>,
                   rank_map: impl FnMut(usize) -> Option<(NonZero<u64>, PopVerified<PubkeyAffine>)>,
                   thread_pool: Option<&ThreadPool>) -> Result<Certificate, Error>
```
- The validator set comes in through the `rank_map` closure (rank → stake and pubkey). The crate holds no ledger state of its own.
- `thread_pool: None` takes the sequential path, so rayon is never called at runtime.
- `verify_certificate` checks the bitmap decoding, aggregates the pubkeys, verifies the signature and then the stake threshold.

### Vote payloads (`votor-messages/src/wire.rs`, `VotePayloadToSign`)
- Wincode encoding: `tag u8 | slot u64 LE | [block_id 32] | shred_version u16 LE`.
  - Block votes are 43 bytes. Slot-only votes are 11 bytes.
- Tags: 1 Notar, 2 Finalize, 3 Skip, 4 NotarFallback, 5 SkipFallback, **6 Genesis**. *(PLAN §8.2 is missing tag 6)*
- Hash-to-curve DST: `BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_`. *(confirmed)*
- `verify_signature` hashes the raw payload bytes directly. There is no extra pre-hash step.

### Local Alpenglow validator: first run (item 2.1, 2026-11-03)
- Agave v4.4.0 `solana-test-validator --alpenglow --ledger .local-ledger --reset` runs. Shred version **28090**, one validator with about 1M SOL stake. Every slot reaches finalized.
- `getAgGenesisCert` returns an **all-zero placeholder** (slot 0, zero block_id, signature and bitmap). The cluster started on Alpenglow, so it never migrated and no real genesis cert exists. `fetch genesis-cert` now refuses it.
- **footer-dump plugin works (2.2):** the validator loads it with `--geyser-plugin-config plugins/footer-dump/config.json`. In the first 371 footers (slots 903..1276):
  - 370 carry a fast-path cert for slot N−1. The final aggregate has a 96-byte signature and bitmap `00 0100 01` (base2, 1 bit, rank 0).
  - One (slot 980) has no cert, which Triton's guide also calls normal.
  - `notar_aggregate` is always null.
  - 3 slots in the range have no footer, presumably no block was produced.
- Footers carry certs: `agave-ledger-tool -l .local-ledger slot N -vvv`. **It needs `-vvv`**; `-vv` hides block markers.
  - Across slots 1..293, the footer of slot N always holds the finalization cert for slot **N−1**. Slot 1 has none.
  - All are **fast finalization** (`notar_aggregate validators: 0`, so FinalizeFast). With 100% of stake on one validator, the slow path never occurs locally. Slow-path real data needs the multi-validator cluster (2.6); until then it's covered by synthetic tests.
  - Footers also carry a NotarReward cert for an older slot (e.g. slot 20 → slot 12).

### Local footer certificates: verified exactly (item 2.3, 2026-11-03)
- `cert::verify_footer` decompresses the 96-byte signature, then runs Agave's `verify_certificate` once per part:
  - fast path: FinalizeFast(block) at 80%;
  - slow path: Notarize(block) **and** Finalize(slot), each at 60%. Finalize(slot) alone doesn't say which block, so both must pass.
- Decompression (`SignatureAffine::try_from(SignatureCompressed)`, blstrs `from_compressed`) also checks the point is in G2. Junk bytes fail before any pairing.
- **All 40,999 certs** captured in epoch 0 (slots 903..~41,900) verify against `fixtures/local/validator_set_0.json` with shred version 28090. Nothing is approximate: same chain, same epoch, exact stakes. One footer (slot 980) has no cert.
- Real-data tamper tests (`tests/local_footers.rs`, sample in `fixtures/local/footers_sample.jsonl`) — each fails:
  - a signature bit flipped;
  - block_id or slot changed;
  - wrong shred version;
  - the fast cert read as Finalize(slot);
  - a different BLS key at rank 0;
  - an empty bitmap.
- Slow path is synthetic only (unit tests in `cert.rs`). These check that 61.8% passes, 49% fails, a notarization for a different block fails, and a slow footer read as fast fails.
- The local epoch is 432,000 slots, so `validator_set_0.json` stays valid for about 48 hours of validator runtime.

### SP1 go/no-go (item 0.10, in progress, 2026-11-03)
- **Agave's BLS stack can't run in SP1 as is.** `solana-bls-signatures` uses `blstrs` → `blst`, which is C and assembly. SP1 has no acceleration for it.
- SP1 *does* accelerate BLS12-381 through its patched zkcrypto `bls12_381` 0.8.0. The precompiles cover G1/G2 add and double, decompression, and Fp/Fp2 arithmetic. No pairing precompile was found in the docs.
- **So `crates/alpenglow-core` reimplements the check** on `bls12_381` + `sha2`, with no Agave crates and no C. It holds `finality` (payload, base2 bitmap, hash_to_curve with the Agave DST, a pairing check in one multi-Miller loop, and the stake threshold by cross-multiplication) and `shred` (moved from alpenglow-lc). `alpenglow-lc` keeps Agave's verifier as the reference.
- **Equivalence with Agave (`alpenglow-lc/tests/core_matches_agave.rs`):**
  - payload bytes for Notarize, FinalizeFast, Finalize and Genesis;
  - all 45,660 captured local footer certs;
  - the real devnet genesis cert (192-byte uncompressed signature);
  - 400 random synthetic certs over 24 validators (all four types, every signer-set size, tampered signatures, bitmaps, shred versions, oversized bitmaps).

  Core and Agave accept and reject exactly the same inputs.
- `block_proof::BlockProofInput::verify` is the whole check the SP1 program runs (finality, then inclusion). It outputs `{validator_set_hash, shred_version, slot, block_id, signed_stake, total_stake}`. The layout is provisional until 1.1 and 4.1.
- **The SP1-patched `bls12_381` differs from crates.io 0.8.0:** its hash-to-curve takes a digest-0.10 hasher and a message given as a list of byte slices. So the main workspace `[patch]`es in the SP1 copy too (`Cargo.toml`), and native tests and the zkVM compile identical curve code. `sha2-09` is gone.
- SP1 toolchain installed: `cargo-prove` v6.10.0 (built 2026-10-23), `succinct` rustc 1.96.0-dev. `zk/program` builds with `cargo prove build`. Its lock resolves `sha2` to the patched 0.10.8; the unused 0.9.9 patch was removed.
- **`zk/script` needs `protoc`** (`brew install protobuf`): `sp1-prover-types` runs prost-build unconditionally, even with no sdk features. Cycle counts are pending on that.

### Finality API and set commitment (items 1.1–1.3, 2026-11-03)
- `alpenglow_core::proof::FinalityInput::verify` returns `Finality { validator_set_hash, shred_version, slot, block_id, path: Fast|Slow, signed_stake, total_stake }`. On the slow path, `signed_stake` is the Finalize part's.
- **Set commitment (1.1):** `sha256("huginn-validator-set-v1" | count u32 LE | per rank: bls_pubkey[48] | stake u64 LE)`. Rank order is included, because bitmaps index by rank.
  - Local epoch 0: `b1f25236519c10b064498b6507ed0b87b88ad4df728a0a348e9434769d7d3c11`.
  - `fetch validator-set` writes it as `commitment`.
  - Any change to a key, a stake, the order or the count changes it. That's what stops a prover from inflating stakes in its own input: the destination chain compares against the hash it stored.
- **Adversarial tests (1.3)**, all refused:
  - the slow-path trap (Finalize(slot) read as FinalizeFast, or paired with a Notarize for another block);
  - a shred version mismatch;
  - the wrong epoch's set (other keys, or the same keys reordered);
  - real signatures below 80% on the fast path (accepted at the same stake on the slow path, as it should be);
  - zero total stake, a bad key length, an off-curve key, a bitmap longer than the set.
- Synthetic sets (`fixture::synthetic_set`): deterministic secret keys `i+2`, aggregate signature `H(m)·Σsk`. Used because the local chain has one validator.

### Transactions from shreds (items 3.3–3.4, 2026-11-03)
Sources: Agave v4.4.0 `entry/src/block_component.rs`, `entry/src/entry.rs`, `ledger/src/shredder.rs` (`deshred`), `ledger/src/blockstore.rs` (`get_slot_components_with_shred_info`).
- **Components:** consecutive data shreds up to and including one flagged DATA_COMPLETE (flags `0b0100_0000`) deshred into one wincode `BlockComponent`. The data is `shred[88..size]`, with `size` at header bytes 86..88.
  - Entry batch: `count u64 (>0) | Entry*`, where `Entry = num_hashes u64 | hash[32] | tx_count u64 | VersionedTransaction*` (wincode `BincodeLen` = u64 lengths).
  - Marker: `0u64 | version u16 (=1) | variant u8 | len u16 | data`. Variants: 0 footer, 1 header, 2 update-parent, 3 genesis cert. Header/update-parent data = `tag u8 (=1) | parent_slot u64 | parent_block_id[32]`.
- **Observed local layout:** header → entry batches → footer → one final tick entry.
  - **Every component is padded to whole FEC sets**: DATA_COMPLETE falls on shreds 31, 63, 95, 127. That's why near-empty blocks still have 3–4 FEC sets.
  - The header's parent equals the parent leaf of the double Merkle root (checked on 46718).
- **Real transaction:** a 0.5 SOL transfer with the memo "huginn: hello from solana", sent from the local faucet, landed in slot 46718 (legacy, success).
  - Parsed out of the block's shreds, its 275 bytes equal `getTransaction`'s (`fixtures/local/tx_46718.json`).
  - It sits in component 2 (shreds 32..63), entry 0, tx 0.
- **Transaction parser** (`tx.rs`): legacy and v0, compact-u16 lengths (canonical only), plus the runtime's structural rules (signature count = header, writable fee payer, program ids static and not the payer, indexes in range, at most 256 accounts).
- **Efficient inclusion (`TxInclusion`):**
  - The block_id is rebuilt from **all FEC roots** (32 bytes each) plus the parent.
  - Only the transaction's **own component** shreds are given; each proves itself against its FEC root with its embedded Merkle proof.
  - The **one shred before the run** must be in the block and flagged DATA_COMPLETE, so the run starts at a real boundary and can't be read out of frame.
  - For block 46718 that's 32 of 128 shreds. On mainnet, proof cost scales with the transaction's component, not the block.
- **Inclusion ≠ success.** Failed transactions are in blocks too (they pay fees). Proving success needs execution results (status/bank hash), which no certificate signs today → Phase 7.
- **Public values v1** (`TxOutput::encode`, provisional for 4.1): fixed big-endian layout with offsets documented in code: version, set hash, shred version, slot, block_id, path, stakes, position, tx length, then the tx bytes exactly as in the block.

### Block_id from raw shreds (item 2.4, 2026-11-03)
Sources: Agave v4.4.0 `ledger/src/shred/{merkle.rs,merkle_tree.rs,wire.rs}`, `ledger/src/blockstore.rs` (`get_block_id`, `build_double_merkle_tree`), `ledger/src/blockstore/column.rs`.
- **Alpenglow block_id = double Merkle root.** Its leaves, in order:
  - each FEC set's Merkle root;
  - then `sha256(parent_slot u64 LE | parent_block_id | fec_set_count u32 LE)`.

  A TowerBFT block_id was just the last shred's FEC root; Agave picks the rule per slot (`should_use_double_merkle_block_id`).
- **FEC set tree:**
  - Leaves are `sha256("\x00SOLANA_MERKLE_SHREDS_LEAF" | shred[64..proof_offset])`: everything after the signature, including the chained root.
  - Inner nodes are `sha256("\x01SOLANA_MERKLE_SHREDS_NODE" | left[..20] | right[..20])`. Only 20 bytes of each child are hashed, which matters for the 80-bit note in PLAN §8.
  - An odd last node pairs with itself. The double tree uses the same `MerkleTree`, so the same rules apply.
- **Shred layout:**
  - Data shreds are 1203 bytes (88-byte headers). Coding shreds are 1228 bytes (89-byte headers).
  - Variant byte 64: high nibble 0x9/0xb = data, 0x6/0x7 = code (the second value of each pair = resigned); low nibble = proof entries.
  - After the data buffer: chained root (32), proof (20 × n), then the optional retransmitter signature.
  - 32 data shreds per FEC set, so `fec_set_count = last_index / 32 + 1`.
- **Chaining:** FEC set *i* chains to set *i−1*'s root. Set 0 chains to the **parent's last FEC set root**, not the parent's block_id. Checked: slot 44000 set 0 = slot 43999's last FEC root.
- **The parent leaf binds the parent.** `(parent_slot, parent_block_id)` come from the block header marker inside the block. Since that leaf is hashed into the block_id, a certified block_id also certifies its parent's block_id. That is the basis for walking back (PLAN §8.7).
- **Reading the ledger:**
  - `ledger-dump` opens `<ledger>/rocksdb` **read-only** with only the `data_shred` and `code_shred` column families. It works while the validator runs.
  - Keys are `slot BE u64 | index BE u64`; values are the raw shred bytes.
  - The test validator prunes old slots: this ledger held 39166..44016.
- **Result:**
  - Slots 43800..44010 (211 blocks): the rebuilt block_id equals the footer-certified one for every slot.
  - Every coding shred agrees with its set's root.
  - A typical local block is 96 data + 96 coding shreds in 3 FEC sets, even with almost no transactions.
- Fixture `fixtures/local/block_44000.json` (228 KB) holds the 96 data shreds (coding shreds aren't needed) plus the certs for 44000 and its parent. `tests/local_block.rs` checks finality and block_id end to end; each of these fails:
  - one changed byte;
  - a missing, duplicate or truncated shred set;
  - a wrong parent slot or parent id;
  - a dropped FEC root.
- Not checked: the leader's Ed25519 signature on each FEC root. It isn't needed, because the BLS certificate already commits to the block_id.

### One-command block fixture (item 2.5, 2026-11-03)
- Run `cargo run --release -p ledger-dump` with the validator running.
  - It picks the newest certified slot still in the ledger; `--slot S` picks a specific one, and `--range` shows what's available.
  - It writes `fixtures/local/block_<S>.json` with: epoch, shred version, that epoch's validator set, the footer certs for S and its parent, and S's data shreds.
- **It refuses to write unless all of these hold:**
  - both certs verify against the epoch's set;
  - every shred carries the cluster's shred version;
  - the shreds rebuild the certified block_id.
- **Epoch:** `slot / 432000`. `solana-test-validator` has no warmup epochs (`getEpochInfo` reports 432,000 slots in epoch 0); override with `--slots-per-epoch`.
- **Validator sets:** the set comes from `fixtures/local/validator_set_<epoch>.json`. RPC only serves current stakes, so `fetch validator-set --cluster local` must run during each epoch we want fixtures for. A missing set is a clear error, not a guess.
- **Known gap:** a block whose parent is in the previous epoch would need two sets. The tool refuses such blocks for now.
- `block_44000.json` was regenerated in this format, and `tests/local_block.rs` now reads its set and shred version from the fixture itself.

### Decision: local Alpenglow validator (item 0.9, 2026-11-03)
- **Chosen:** `solana-test-validator --alpenglow` (Agave v4.4.0, released 2026-10-28, macOS builds available). It activates Alpenglow at genesis and produces real footers with real BLS finalization certs, using the same code as devnet.
- **Why:**
  - It's free.
  - We control the genesis, so ranks and stakes are exact.
  - The shreds are on local disk, which Phase 3 needs and no gRPC provider streams.
  - The real-network crypto is already covered by the devnet genesis cert (0.7/0.8).
- **Rejected for now:**
  - Triton ($125 prepaid, the only provider confirmed to send certificates).
  - Helius (its footer stream has no certs yet; helius-labs/yellowstone-grpc #74 is open).
  - Plain `getBlock` (its footers have no certs).
  - Revisit only if a live-network demo needs one.
- **Local facts:**
  - The test validator derives the BLS key from the authorized voter keypair (`BLSKeypair::derive_from_signer(voter, BLS_KEYPAIR_DERIVE_SEED)`).
  - There is a single validator, so bitmaps have 1 bit. Ranking needs a multi-validator cluster later (2.6).
- **Getting raw cert bytes:**
  - `agave-ledger-tool slot -vv` prints only the signer counts, not the signatures or bitmaps.
  - So we write a small Geyser plugin on `agave-geyser-plugin-interface =5.0.0` (the version Agave 4.4.0 pins) that implements `notify_block_footer`.
  - Plugins are Rust dylibs with no stable ABI, so **build them with rustc 1.98.1**, Agave 4.4.0's `rust-toolchain.toml`. Our workspace uses 1.99.1.

### Where current certificates live (item 0.9)
Sources: Agave v4.4.0 `entry/src/block_component.rs`, `runtime/src/block_component_processor.rs`, `runtime/src/validated_block_finalization.rs`, `geyser-plugin-manager/src/entry_notifier.rs`; Yellowstone gRPC `geyser.proto` (master, commit #903 2026-10-06).

- **Every Alpenglow block ends with a block footer**, a block marker inside the block's own shreds. Its fields:
  - `bank_hash` (a hook for Layer 2),
  - producer time and user agent,
  - an optional **`block_final_cert`**,
  - optional skip and notar reward certs.
- `block_final_cert` proves that **some earlier block** is finalized: `{slot, block_id, final_aggregate, notar_aggregate?}`. A given footer may carry none.
  - **Fast finalization:** no `notar_aggregate`. One cert, `FinalizeFast(Block{slot, block_id})`, needs 80%.
  - **Slow finalization:** two certs, both at 60%: `Notarize(Block{slot, block_id})` from `notar_aggregate` and `Finalize(slot)` from `final_aggregate`. **`Finalize` names only the slot. The Notarize cert is what binds the block_id, so Huginn must verify both.**
  - Signatures in the footer are **compressed (96 bytes)**, unlike the 192 bytes from the genesis RPC. Agave decompresses them before verifying.
  - Bitmaps must be base2, and rank against the set of **the epoch of the certified slot** (`epoch_stakes_from_slot(cert slot)`).
- Validators re-verify the footer cert while replaying the block (`ValidatedBlockFinalizationCert::try_from_footer`). An invalid one makes the whole block invalid.
- **Geyser:** Agave 4.4 has a `notify_block_footer` plugin hook that passes the full footer, certs included.
- **Yellowstone gRPC** (the Geyser plugin most providers run) streams it:
  - subscribe with filter `block_footer: { include_certificates: true }`;
  - you get `SubscribeUpdateBlockFooter { slot, bank_hash, block_final_cert { slot, final_aggregate, notar_aggregate } }`;
  - each aggregate holds the signature, `block_id` and the signer-store bitmap verbatim.
  - Typed cert messages landed 2026-10-06. The latest release is `v16.2.0+solana.4.3.2` (2026-10-30). **A provider must run a recent build with footer certs enabled.** Not yet checked with any provider.
- **Providers (checked 2026-11-03, from public pages):**
  - Triton One maintains Yellowstone. Its Alpenglow guide confirms `block_footer` with `include_certificates: true` on Dragon's Mouth and Fumarole, and says JSON-RPC footers come in a later release.
  - Pricing is prepaid: $0.08/GB streaming, $125 minimum deposit, valid for 12 months.
  - At about 2.5 footers/s and roughly 1 KB each, footers cost cents per month. For our use the deposit is the real cost.
  - Still unconfirmed: footer certs on Triton **devnet** specifically.
  - Helius has devnet LaserStream from the $49/mo Developer plan, but footer cert support isn't documented.
- Still not exposed: plain RPC (`getBlock`), `agave-ledger-tool`, and the blockstore read API. There are no footer helpers in v4.4.0.
- **getBlock footers (searched 2026-11-03):**
  - No Agave PR adds footers to `getBlock`.
  - Triton's Superbank RPC and Old Faithful (yellowstone-faithful #409) serve `footer: {blockProducerTimeNanos, blockUserAgent}` only, **without certificates**.
  - Agave's BigTable upload stores raw block markers, certs included (agave #13423). The public RPC route therefore isn't coming soon for certs.
  - **Local route:** `Blockstore::get_complete_block_with_components` and `agave-ledger-tool slot -vv` print footers (agave #12532). So a local validator's footers can be read without writing a Geyser plugin.
- **Phase 3 warning:** proving that a transaction is inside a block_id needs the **shreds** (Merkle paths up to the last data shred's root). Yellowstone streams entries and transactions, not shreds. Rebuilding exact shreds from entries is unproven. So inclusion proofs will probably need our own node, or a provider that serves shreds. Decide in Phase 3.

### Full certificate verification in Rust (item 0.8)
- **2026-11-02:** `alpenglow_lc::cert::verify` wraps Agave's `verify_certificate` with no thread pool. The real devnet genesis cert passes end to end: bitmap decode, aggregation, signature, and 82% threshold (`tests/devnet_genesis.rs`).
- That run uses a reconstructed rank map:
  - The signer positions come from the real bitmap: `[0,1,2,3,4,7,9,10,14,15,16]` of 17.
  - The keys are the 0.7 signers.
  - Stakes are today's (97.1% signed), so **the stake check there is a stand-in. The signature check is exact.**
- The same cert verified against **today's** ranking fails with `VerificationFailed`. Ranks shift between epochs, so the verifier needs the cert's own epoch set.
- Tamper cases rejected, on both synthetic and real data:
  - a bitmap bit added or removed,
  - one signature bit flipped (fails at point decoding, `VerifySig(_)`),
  - a different slot, shred version or cert type,
  - genuine signatures holding 81.8% < 82%,
  - a bitmap longer than the set.
- **The RPC genesis cert has no `shred_version` field.** It is part of the signed payload, so the verifier must get it from somewhere else. For Huginn this means the posted epoch data must include the shred version (or the relayer passes it and a wrong value just fails).
- `verify_certificate` only looks up the ranks set in the bitmap, plus `max_validators` for decoding and `total_stake` for the threshold. Non-signers matter only through `total_stake`.

### Devnet genesis certificate signature: verified (item 0.7)
- **2026-11-02:** `find_genesis_signers` found the 11 signers among today's 22 ranked devnet keys (epoch 1198). The search covered C(22,11) = 705,432 subsets in about 103s.
- This confirms against real cluster data:
  - the payload encoding (Agave's `get_vote_payload_to_sign`, tag 6, 43 bytes),
  - the DST,
  - the 192-byte uncompressed signature parsing,
  - that signatures aggregate the same way (plain sum).
- Payload: `0607b40c1e0000000013f09ed28a3f91b07ef7d727d829e41caa2b976b6b71ea6feaae75113de31677681b`.
- Devnet `shred_version` = **7016** (153 of 153 nodes, `fixtures/devnet/cluster_info.json`). It was the same at genesis, otherwise the signature would not verify.
- Signers by today's rank: 0, 1, 2, 3, 4, 7, 10, 11, 17, 18, 19. All 11 keys are unchanged since epoch 1167.
- **Not covered:** the ranking order and stake threshold for a historical epoch. Those wait for a current-epoch cert (0.8/0.9).

### Thresholds (`CertificateType::threshold`)
- 60%: Finalize, Skip, Notarize, NotarizeFallback.
- 80%: FinalizeFast.
- **82%: Genesis** (`GENESIS_VOTE_THRESHOLD`).

### Bitmap (`solana-signer-store`)
- Layout: `version u8 | bit count u16 LE | payload`.
  - Version 0 (base2): plain bits, LSB first.
  - Version 1 (base3): two bitmaps (primary and fallback votes), packed five symbols per byte.
- `decode(bytes, max_len)` rejects a bit count larger than `max_len`.

### Ranked validator list (`runtime/src/epoch_stakes.rs`, `BLSPubkeyToRankMap::new`)
1. Skip vote accounts with zero stake or no BLS key.
2. Drop entries whose **BLS key is duplicated**, and also entries whose **node identity pubkey is duplicated**. *(PLAN §8.3 missed the node-pubkey rule)*
3. Sort by stake (highest first), then by compressed BLS key bytes (ascending).
4. Rank = position in that list. Total stake = the sum over the filtered list only.
5. Keys are wrapped with `PopVerified::new_unchecked`, because the vote program already checked proof of possession when each key was registered.

### Vote account BLS key (`solana-vote-interface` `VoteStateV4`)
- Version tag 3 (u32 LE).
- Fields in order: `node_pubkey`, `authorized_withdrawer`, `inflation_rewards_collector`, `block_revenue_collector` (32 bytes each), two u16 commission fields, `pending_delegator_rewards` u64, then `bls_pubkey_compressed: Option<[u8;48]>`.
- So the option tag sits at **byte offset 144**, and the key at bytes **145..193**.
- **Stored compressed (48 bytes).** *(PLAN §8.4 Confirm: resolved)* Still to check against a live account in item 0.6.

## Open problems

- **(Partly resolved by 0.9.)** **Plain RPC exposes only the genesis certificate.** `getBlock` has no footer and no certs, and Agave 4.4's RPC has no other Alpenglow method. Current-epoch certificates need Geyser/gRPC or our own node. This moves the Phase 2 data decision up to item 0.9.
- **We can't rebuild the epoch-1064 mainnet set over RPC.** `getVoteAccounts` returns current stakes only (now epoch 1074). Stake changes reorder the ranks. Plan: verify the devnet genesis cert first (17 validators), and get mainnet coverage from a current-epoch cert once 0.9 is settled.

## Trust assumptions (stated, not hidden)

- The validator set posted to each destination chain is trusted input. We post it ourselves.
- We don't re-check BLS proof of possession. We rely on the vote program's check at registration. A later improvement: re-check once per epoch when posting the set.
