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

- **Plain RPC exposes only the genesis certificate.** `getBlock` has no footer and no certs, and Agave 4.4's RPC has no other Alpenglow method. Current-epoch certificates need Geyser/gRPC or our own node. This moves the Phase 2 data decision up to item 0.9.
- **We can't rebuild the epoch-1064 mainnet set over RPC.** `getVoteAccounts` returns current stakes only (now epoch 1074). Stake changes reorder the ranks. Plan: verify the devnet genesis cert first (17 validators), and get mainnet coverage from a current-epoch cert once 0.9 is settled.

## Trust assumptions (stated, not hidden)

- The validator set posted to Base is trusted input. We post it ourselves.
- We don't re-check BLS proof of possession. We rely on the vote program's check at registration. A later improvement: re-check once per epoch when posting the set.
