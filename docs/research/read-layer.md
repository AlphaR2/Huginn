# The Solana read layer, and where Huginn's shreds can come from

Researched 2026-11-07 from Agave and Yellowstone source on GitHub (master, which may be ahead of v4.4.0) and by probing a local Agave 4.4.0 Alpenglow validator. Anything marked **unverified** was not tested.

## What Huginn needs from the chain

For one transaction in block N:

1. **Finality:** the certificate for N. It sits in the footer of block N+1 (BLS signature, signer bitmap, `block_id`).
2. **Inclusion:** the `block_id` of N rebuilt from the FEC set roots, and a Merkle path from the transaction's data shred to its set's root. Both come from shreds.

Nothing else Solana signs commits to the transaction list: the footer's `bank_hash` covers account changes, and PoH entry hashes are not tied to `block_id` without the shreds.

## The block lifecycle under Alpenglow (as Huginn sees it)

1. The leader serializes entries into components, cuts them into data shreds, groups 32 data + 32 coding shreds per FEC set, and builds one Merkle tree per set (chained: each set's tree includes the previous set's root). The leader signs each set's root.
2. Rotor spreads shreds to validators. Each validator reassembles the block and replays it.
3. `block_id` = double Merkle root over all FEC set roots plus a final leaf `sha256(parent_slot | parent_block_id | fec_count)`. A validator's blockstore keeps this tree (`DoubleMerkleMeta`) the moment a slot is full.
4. Validators send BLS votes off-chain. 60% of stake notarizes the `block_id` (slow path, then a Finalize round); 80% finalizes in one round (fast path).
5. The aggregated certificate is put in the footer of a later block (N+1 for block N).

## Layer 1: normal JSON-RPC

62 request types in `rpc-client-types/src/request.rs`. The ones that matter here:

| Method | Gives | Useful for Huginn |
|---|---|---|
| `getBlock` (confirmed/finalized) | `blockhash`, `previousBlockhash`, `parentSlot`, `blockHeight`, `blockTime`, `rewards`, transactions | Finding the slot and neighbours. **No `block_id`, no footer, no certificate, no shreds** (probed on the local validator). |
| `getTransaction`, `getSignatureStatuses` | slot, status, metadata | Locating a transaction and whether it succeeded |
| `getBlocks`, `getSlot`, `getEpochSchedule`, `getEpochInfo` | slot lists, epoch geometry | `SLOTS_PER_EPOCH`, picking the next block |
| `getVoteAccounts` | stake and (v4) BLS keys | The validator set (`fetch validator-set`) |
| `getAgGenesisCert` | the migration genesis certificate | Genesis only |
| `getClusterNodes` | each node's `gossip`, `tvu`, **`serveRepair`**, `rpc`, `shredVersion` | **Finding peers to ask for shreds.** Probed: `serveRepair` is present |
| `getMaxShredInsertSlot`, `getMaxRetransmitSlot`, `minimumLedgerSlot`, `getFirstAvailableBlock` | node progress and how far back it keeps | Retention checks |
| `getVatActiveSetRankMap` | named in the request list | **Not found** on the local 4.4.0 validator; unverified elsewhere. If it exists on live clusters it would give Agave's own rank order |

Conclusion: RPC alone yields the transaction and its position, but neither finality nor inclusion data.

## Layer 2: Yellowstone gRPC (`geyser.proto`)

RPCs: `Subscribe`, `SubscribeDeshred`, `SubscribeGossip`, `SubscribeReplayInfo`, `Ping`, `GetLatestBlockhash`, `GetBlockHeight`, `GetSlot`, `IsBlockhashValid`, `GetVersion`.

`Subscribe` filters: `accounts`, `slots`, `transactions`, `transactions_status`, `blocks`, `blocks_meta`, `entry`, **`block_footer`**, plus `accounts_data_slice`, `from_slot` (replay from a past slot), `commitment`.

| Update | Fields that matter |
|---|---|
| `slot` | `slot`, `parent`, `status` (processed, confirmed, finalized, first shred received, completed, created bank, dead), `bank_id` |
| `block` | `blockhash`, `transactions[]`, `entries[]`, `rewards`, `parent_slot`, `executed_transaction_count`, optionally account updates |
| `entry` | `index`, `num_hashes`, `hash`, `executed_transaction_count`, `starting_transaction_index`. With `include_update_parent`: an `entry_update_parent` marker (`parent_slot`, `parent_block_id`) |
| **`block_footer`** | `bank_hash`, `block_producer_time_nanos`, `block_user_agent`, and with `include_certificates`: `block_final_cert` (final + notar aggregates), `skip_reward_cert`, `notar_reward_cert`. Each aggregate has `signature_kind` (compressed BLS12-381 G2), `signature`, **`block_id`**, `signer_bitmap` |

So gRPC provides **finality (step 1) fully**, with the `block_id`, without running a node or our Geyser plugin, and `from_slot` allows replay of recent past slots (provider retention applies). It matches our `footer-dump` plugin's output.

`SubscribeDeshred` streams transactions as they are formed from shreds, before execution, with `completed_data_set_starting_shred_index` / `..._ending_shred_index_exclusive` and an `update_parent_fec_set_index` marker. This tells you **which shred indices carry a transaction** but gives no shred bytes or Merkle data. A search result says the open-source server returns `UNIMPLEMENTED` for it (Triton extension only); unverified.

`SubscribeGossip` streams the cluster's contact table, which also lists peer addresses.

What gRPC does **not** give: shred bytes, FEC set roots, Merkle proofs. That is inclusion (step 2).

## Layer 3: the repair protocol (the missing piece)

Validators serve shreds to any node that asks. In Agave's `core/src/repair/serve_repair.rs` and `repair_handler.rs` there are requests keyed by `block_id`, built for Alpenglow:

| Request | Answer |
|---|---|
| `ParentFecSetCount` (slot, block_id) | `fec_set_count`, `(parent_slot, parent_block_id)` and a proof of that parent info against the `block_id` |
| `FecSetRoot` (slot, block_id, fec_set_index) | that set's Merkle root and its **proof against the `block_id`** |
| `WindowIndexForBlockId` (slot, shred index, block_id) | the raw data shred bytes |
| `WindowIndex`, `HighestWindowIndex`, `Orphan` | older slot/index based shred requests |

Rules (from the source excerpts I read):
- Requests are **signed** with a node keypair and carry `sender`, `recipient`, `timestamp` (±10 minutes) and a `nonce`.
- **Unstaked senders are served.** Stake only orders requests when a node is overloaded (1024 requests per iteration, then sorted by whitelist and stake).
- A **ping/pong handshake** first: an unverified sender gets a ping challenge and its request is dropped until it replies.
- Outbound budget 12 MB/s per node; one packet per request except `Orphan`.
- A node answers only if its blockstore has the slot and a `DoubleMerkleMeta` for that `block_id`; unknown block ids get no answer.
- Peers: `getClusterNodes` lists each node's `serveRepair` UDP address.

This fits Huginn: the certificate gives the `block_id`; `ParentFecSetCount` gives the set count; `FecSetRoot` for each set gives all roots with proofs against that id, so the whole `block_id` can be rebuilt; `WindowIndexForBlockId` fetches only the data shreds that hold the target transaction. No subscription, no replay problem, works for past slots, and each answer can be checked against the certified `block_id`, so peers are untrusted.

### Verified against Agave v4.4.1 source (2026-11-07)

Read from a shallow clone of `v4.4.1` (`5bb3040`). `git diff v4.4.0 v4.4.1` shows **no change** under `core/src/repair` or `ledger/src`, so the same holds for 4.4.0, the version the local validator and the pinned crates use.

**The requests exist in 4.4.x.** `RepairProtocol` (`core/src/repair/serve_repair.rs`), in wire order (a `u32` little-endian variant tag, serialized with wincode, so fields follow in declaration order):

| Tag | Variant | Fields after the header |
|---|---|---|
| 7 | `Pong` | (answer to a ping) |
| 8 | `WindowIndex` | `slot u64`, `shred_index u64` |
| 9 | `HighestWindowIndex` | `slot`, `shred_index` |
| 10 | `Orphan` | `slot` |
| 11 | `AncestorHashes` | `slot` (rejected as unsupported when signature-checked) |
| 12 | `ParentAndFecSetCount` | `slot u64`, `block_id [u8;32]` |
| 13 | `FecSetRoot` | `slot u64`, `block_id`, `fec_set_index u32` |
| 14 | `WindowIndexForBlockId` | `slot u64`, `shred_index u32`, `block_id` |

(Tags 0 to 6 are unsigned legacy variants, always refused. Tag numbers follow declaration order and the `random_range(7..=14)` in the ABI sampler; confirm with a round-trip test when writing the client.)

**Request header** (`RepairRequestHeader`, directly after the tag): `signature` (64) | `sender` (32) | `recipient` (32) | `timestamp u64` (ms) | `nonce u32`.
- Signed data = the 4 tag bytes + everything after the signature. The signature sits at bytes 4..68 (`repair_proto_to_bytes`).
- The receiver checks: `recipient` is its own identity (else `IdMismatch`); timestamp within 10 minutes (`TimeSkew`); signature valid for `sender` (`SigVerify`).

**Handshake.** A request from an unverified (identity, address) pair is **dropped** and the node replies with a signed `Ping` (`RepairResponse::Ping` or `BlockIdRepairResponse::Ping`). The client signs a `Pong` over the ping's token with the same identity and sends it back; after that, requests from that address are served. The pong is cached for 1280 seconds. A pong is a `RepairProtocol::Pong` (tag 7).

**Responses.**
- `WindowIndexForBlockId` returns **the raw data shred bytes with a 4-byte little-endian nonce appended** (`repair_response_packet_from_bytes`). That is the same shred layout `alpenglow-core::shred` already parses. A packet is at most `PACKET_DATA_SIZE` (1232 bytes), and a data shred with a proof is 1203 + headers, so it fits.
- `ParentAndFecSetCount` and `FecSetRoot` return a wincode-serialized `BlockIdRepairResponse`: `ParentFecSetCount { fec_set_count u32, parent_info (slot, parent_block_id), parent_proof bytes }`, `FecSetRoot { fec_set_root (20-byte entry), fec_set_proof bytes }` or `Ping { ping }`. The nonce is passed to the same packet builder; I did not read the exact byte placement for these two, assume it is appended the same way and confirm with a capture.
- Everything is **self-authenticating against the `block_id`** (`BlockIdRepairType::verify_response`): the parent proof verifies the leaf `hash(parent_slot | parent_block_id | fec_set_count)` as the last leaf of the double tree; each `FecSetRoot` proof verifies the set's root at leaf `fec_set_index / 32`. Roots are 20-byte entries (the double tree commits to the truncated root). Proof entries are 20 bytes, matching what `shred.rs` uses.

**What the server needs and who is served.**
- Answers only if the blockstore has a `DoubleMerkleMeta` for that `(slot, block_id)` (a complete block). Otherwise it stays silent (no error packet). Lookups are by `block_id`, so alternate versions of a slot are also found.
- Unstaked and unknown senders are accepted after the handshake. Stake and a whitelist only decide who is dropped first when over 1024 requests per iteration. Outbound budget 12 MB/s (leaders pay 10x).
- **Retention** is the validator's `--limit-ledger-size`. The source's legacy default is 200,000,000 shreds (`LEGACY_DEFAULT_MAX_LEDGER_SHREDS`). At 256 shreds per slot (our local blocks), that is roughly 780,000 slots, about 3.6 days at 0.4 s per slot. Real blocks are bigger or smaller, so the real window is a rough estimate. The local test validator keeps far less.

**Reference client.** Agave's own `core/src/repair/block_id_repair_service.rs` is the working client for exactly these requests: `send_requests`, `process_block_id_repair_response`, `process_block_id_repair_ping_response`, `retry_timed_out_requests`. It is the template for ours.

### Tested against a real Agave 4.4.0 node (2026-11-07)

`crates/shred-repair` implements the client. Against the local Alpenglow validator (`127.0.0.1:8008`) with a random throwaway identity:
- **Handshake works.** The first request is answered with a ping; after our pong the node serves us.
- **By slot:** `HighestWindowIndex` and `WindowIndex` fetched all 96 data shreds of slot 43419 and of its child 43420. `ledger-dump --shreds` on those files printed `MATCH` (computed `block_id` == certified `block_id`), with no ledger read.
- **By block id:** `ParentAndFecSetCount` and `FecSetRoot` returned 3 sets and a parent, every proof checked out against the `block_id`; `WindowIndexForBlockId` then returned the 96 data shreds, which matched the proven roots and again gave `MATCH`.
- **A wrong block id gets no answer** (the server stays silent), as the source says.
- **Corrections to my reading of the source:** the block-id ping (`BlockIdRepairResponse::Ping`, variant 2) has **no trailing nonce**, unlike the other replies; shred replies and the parent/root replies do end in the 4-byte little-endian nonce.

### Tested against public devnet (2026-11-07)

Devnet runs Agave 4.4.0 and 147 of its nodes advertise a `serveRepair` address (`shred-repair peers --rpc https://api.devnet.solana.com`). With a fresh throwaway identity from a laptop on a home network (no VPS, no port forwarding):
- **The first public validator tried served us.** All 192 data shreds of slot 519,635,459 and of its child, about 66 seconds for both, through the same handshake as locally.
- **`ledger-dump` printed `MATCH`:** devnet shred version 7016, epoch 1202's 22-validator set (`fetch validator-set --cluster devnet`), a real **fast-path certificate with 17 of the 21 ranked validators in its bitmap**, and the block id rebuilt from 6 FEC sets equal to the certified one. This is the first real multi-validator certificate Huginn has verified on a block from a live network.
- **By block id:** `shred-repair shape` against the same peer proved the 6 set roots and the parent against the block id, no shred downloaded.
- Kept as a permanent test: `fixtures/devnet/shreds/` and `crates/ledger-dump/tests/devnet_block.rs`.

**What the real network showed that my local chain hid:** a footer carries the certificate of the *newest final block when it was written*, not necessarily the parent. Slot ...459's footer carried the cert for ...457. Finalizing a block finalizes its ancestors, so a parent may never get a certificate of its own. `ledger-dump` therefore no longer needs the parent's cert: the parent block id is read from the block's own header (and bound into the certified `block_id`, so only the true one rebuilds it). The child's footer still gave the cert for the block itself, which is all the proof needs.

### Still unverified
- Whether every devnet or mainnet peer behaves the same (rate limits, identity filters). One public devnet peer, one run.
- How far back peers keep slots (this run fetched slots about 40 slots old, about 16 seconds).
- Whether the child's footer always carries the cert for the block. Here it did; under load it may arrive a few blocks later (the code would need to look at later blocks).
- Mainnet.
- Only data shreds are fetched (`WindowIndex` serves data shreds only); every data shred's proof already yields its set's root, so coding shreds are not needed.

## Other routes

| Route | Cost | Verdict |
|---|---|---|
| Own non-voting node, read its ledger (`ledger-dump`) | A server; no per-use fee | Works today; retention-limited; heavy for mainnet |
| Block-id repair client (above) | Engineering only | Best candidate: free, past slots, any cluster with reachable peers |
| Live raw-shred feeds (Raiden Surge $49/day, Helius $1,000/month/IP) | Money | Wrong shape: live only, no replay; devnet and coding-shred coverage not stated |
| Rebuild shreds from `getBlock` entries | none | Not deterministic from entries: also needs `reference_tick`, `chained_merkle_root`, batch boundaries, `shred_version` (per `merkle.rs`); but a block_id is a commitment, so a guess-and-check is conceivable. Long shot |
| Patched validator that exports shreds | A server and maintenance | Possible; not needed if the blockstore is readable |
| Relayer service | Someone runs the node | Business model, not a source |

## Status

Built as `crates/shred-repair` (see above). It does the following: get `serveRepair` addresses from `getClusterNodes`, complete the ping/pong handshake with a throwaway keypair, fetch `ParentFecSetCount`, `FecSetRoot` (all sets) and `WindowIndexForBlockId` (target data shreds), verify against the certified `block_id`, and feed the same `ShredSource` as the ledger path. Test it first against the local validator (its `serveRepair` is `127.0.0.1:8008`).
