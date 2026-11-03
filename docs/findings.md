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
| Solana CLI | 3.1.10 | **Needs upgrade to 4.4.x** (not blocking: we call RPC directly) |
| SP1 (`cargo prove`) | not installed | Needed for 0.10 |
| Foundry | not installed | Needed for Phase 4 |
| Anchor | 1.1.2 | Only needed if we build the optional outbox |

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
