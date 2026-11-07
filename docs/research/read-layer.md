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

### Open questions (unverified)
- The exact wire format of `BlockIdRepairResponse` and the nonce position (in `repair_response`, not read).
- Whether the response to `FecSetRoot` contains enough to rebuild the double Merkle tree without the shreds themselves (it returns each root and a proof; the final count leaf needs `ParentFecSetCount`).
- How long peers keep a slot (`minimumLedgerSlot` on the local validator was about 4,800 slots behind; a normal validator keeps far more, but the exact default is unverified).
- Whether this is in the Agave 4.4.0 release or only on master. The local 4.4.0 node reports `serveRepair`, but the request set itself is unverified for 4.4.0.
- Devnet and mainnet peers may rate-limit or drop requests from unknown identities; the ping/pong handshake needs UDP reachability both ways.

## Other routes

| Route | Cost | Verdict |
|---|---|---|
| Own non-voting node, read its ledger (`ledger-dump`) | A server; no per-use fee | Works today; retention-limited; heavy for mainnet |
| Block-id repair client (above) | Engineering only | Best candidate: free, past slots, any cluster with reachable peers |
| Live raw-shred feeds (Raiden Surge $49/day, Helius $1,000/month/IP) | Money | Wrong shape: live only, no replay; devnet and coding-shred coverage not stated |
| Rebuild shreds from `getBlock` entries | none | Not deterministic from entries: also needs `reference_tick`, `chained_merkle_root`, batch boundaries, `shred_version` (per `merkle.rs`); but a block_id is a commitment, so a guess-and-check is conceivable. Long shot |
| Patched validator that exports shreds | A server and maintenance | Possible; not needed if the blockstore is readable |
| Relayer service | Someone runs the node | Business model, not a source |

## Proposed next step

Build a repair client crate (`shred-repair`): get `serveRepair` addresses from `getClusterNodes`, complete the ping/pong handshake with a throwaway keypair, fetch `ParentFecSetCount`, `FecSetRoot` (all sets) and `WindowIndexForBlockId` (target data shreds), verify against the certified `block_id`, and feed the same `ShredSource` as the ledger path. Test it first against the local validator (its `serveRepair` is `127.0.0.1:8008`).
