# Huginn codebase guide

A walk through every folder and file: what it does, why it exists, which functions to read first, and which tests prove it works. Read top to bottom the first time. After that, jump to any section.

---

## 1. The whole thing in one picture

Huginn answers one question for another chain: **"Did this Solana transaction land in a block that Solana's validators finalized?"** It answers with a short proof the other chain can check cheaply.

The answer is a chain of facts, each one locking the next:

```
 validator set (epoch E)          who may vote, and how much stake each has
        │  hash = commitment       ← stored on the destination chain per epoch
        ▼
 finality certificate             ≥80% of stake signed "block_id X at slot S"  (or the 60%+60% slow path)
        │  BLS signature check
        ▼
 block_id                         a Merkle root that commits to every shred of the block
        │  Merkle proofs
        ▼
 shreds of one component          the raw packets that carry the transaction
        │  deshred + parse
        ▼
 the transaction bytes            signers, programs, instruction data
```

Each arrow is code in `crates/alpenglow-core`. The SP1 program in `zk/program` runs that same code inside a zkVM, so the destination chain only verifies one proof instead of re-running all of it.

**Two kinds of code live here:**

| Kind | Where | Runs where |
|---|---|---|
| **The verifier**: what the proof proves | `crates/alpenglow-core` | natively in tests **and** inside SP1 |
| **Tools and references**: getting data, cross-checking | `crates/alpenglow-lc`, `crates/fetch`, `crates/ledger-dump`, `plugins/footer-dump`, `zk/prover` | your machine only |

A rule that runs through the whole repo: **`alpenglow-core` never trusts itself.** Everything it does is cross-checked against Agave's own code (`alpenglow-lc`) or against real chain data (`fixtures/`).

---

## 2. Suggested reading order

1. `crates/alpenglow-core/src/finality.rs`: how a certificate is checked (~170 lines).
2. `crates/alpenglow-core/src/shred.rs`: how a block_id is rebuilt from shreds.
3. `crates/alpenglow-core/src/block.rs` + `tx.rs`: how shreds turn back into transactions.
4. `crates/alpenglow-core/src/proof.rs`: how the pieces combine into the three statements we prove.
5. `crates/alpenglow-core/tests/proof.rs`: every attack we refuse, one test each.
6. `zk/program/src/main.rs`: the 20 lines that run all of the above in SP1.
7. Then the tools, in data-flow order: `crates/fetch` → `crates/ledger-dump` → `zk/prover` (and `plugins/footer-dump`, now optional).
8. `crates/alpenglow-lc`, the Agave-based reference, when you want to see *why* we believe `alpenglow-core` is right.

---

## 3. Top-level files

| File | What it is |
|---|---|
| `README.md` | One-page pitch, trust model and layout. |
| `PLAN.md` | The design: Revision 1 and 2 (direction), then the original v1 plan with its numbered sections (§8.x is referenced from code comments). |
| `CHECKLIST.md` | Build order. Each item is ticked only once it's done and explained. |
| `docs/findings.md` | **The lab notebook.** Every fact we confirmed against real code or data: pinned versions, byte layouts, surprises, and how each was checked. When a code comment says "see findings", it means here. |
| `docs/codebase.md` | This file. |
| `docs/research/` | Placeholder for the original research docs. |
| `Cargo.toml` | The Rust workspace: four crates (`alpenglow-core`, `alpenglow-lc`, `fetch`, `ledger-dump`), shared pinned dependency versions, and a `[patch]` that swaps in SP1's version of `bls12_381` (see `alpenglow-core`). |
| `Cargo.lock` | Exact versions resolved. Committed on purpose: reproducible builds. |
| `.gitignore` | Ignores build output, the local ledger (`.local-ledger/`, gigabytes) and the growing `fixtures/local/footers.jsonl`. |

---

## 4. `crates/alpenglow-core`: the verifier (runs in SP1)

**Purpose:** everything the proof proves, in plain Rust with **no Agave crates and no C code**, so it compiles for SP1's RISC-V target. Depends only on `bls12_381` (curve math), `sha2` (hashing), `serde` (input encoding) and `thiserror` (error types).

**Why it exists separately from `alpenglow-lc`:** Agave's BLS library (`blst`) is C plus assembly, and SP1 can't accelerate it. SP1 *does* accelerate `bls12_381` 0.8, so we reimplemented the check on that crate. The workspace `Cargo.toml` patches in SP1's exact copy, so native tests and the zkVM compile identical curve code.

### `src/lib.rs`
Just lists the modules. `fixture` is behind a Cargo feature (`fixture`) because it's for test and host code, never the zkVM.

### `src/finality.rs`: checking one certificate
This mirrors Agave's `verify_certificate` step by step.

- **`CertType`**: the four certificate kinds we need: `Notarize`, `FinalizeFast`, `Finalize`, `Genesis`.
  - `payload()` builds the exact bytes validators signed: `tag | slot | [block_id] | shred_version`. FinalizeFast signs the *Notarize* payload, because it's made of ordinary notarization votes.
  - `threshold()`: 60%, 80% or 82%.
- **`decode_bitmap`**: turns the signer bitmap (`version | bit count | bits`) into the list of signing ranks. It only accepts base2; the base3 format is only used by cert types we don't need.
- **`signature_from_compressed` / `_uncompressed`, `pubkey_from_compressed`**: bytes → curve points. Each includes the subgroup check, so junk bytes fail early.
- **`hash_to_g2`**: message → curve point (the standard SHA-256 hash-to-curve with Solana's tag `BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_`).
- **`verify_signature`**: the BLS check `e(pubkey, H(msg)) == e(g1, sig)`, done as one multi-pairing.
- **`verify_cert`**: the whole thing. Decode the bitmap → look up each signer by rank → add up their keys → check the signature → check signed stake ≥ threshold (exact integer math, no floats).

*Tests:* `crates/alpenglow-lc/tests/core_matches_agave.rs` (agreement with Agave), `tests/proof.rs` (attacks).

### `src/shred.rs`: shreds and the block_id
A block travels as **shreds** (1,203-byte data shreds plus coding shreds), grouped into **FEC sets** of 32 data shreds.

- **`Shred::parse`** reads the header: variant byte, slot, index, version, FEC set index, flags and data size.
  - `merkle_root()` uses the proof each shred carries to compute its FEC set's root.
  - `data()` returns the payload bytes.
  - `data_complete()` / `last_in_slot()` read the flags.
- **`root_from_proof`, `merkle_root`** implement Agave's Merkle tree. Two quirks to know: inner nodes hash only **20 bytes** of each child, and an odd last node is paired with itself.
- **`block_id(fec_roots, parent_slot, parent_block_id)`** is the Alpenglow **double Merkle root**. Its leaves are every FEC root plus one last leaf `sha256(parent_slot | parent_block_id | fec_count)`. That last leaf means a block_id also commits to its parent.
- **`fec_roots`** checks a whole block (no gaps, no duplicates, last shred flagged, sets chained) and returns the roots.

*Tests:* unit tests at the bottom, plus `alpenglow-lc/tests/local_block.rs` (211 real blocks matched during 2.4).

### `src/block.rs`: shreds back into block contents
A leader writes the block as a sequence of **components**, each split across consecutive data shreds. The last shred of each component has the DATA_COMPLETE flag.

- **`deshred`** concatenates shred payloads and cuts at each DATA_COMPLETE.
- **`parse_component`** handles both kinds:
  - **entry batch**: `count | entries`, where each entry is `num_hashes | hash | transactions`;
  - **marker**: header (parent slot and block_id), footer (bank hash and finality cert), update-parent, or genesis cert.
- **`Reader`** is a tiny bounds-checked cursor; every read returns `None` instead of panicking. `compact_u16` reads Solana's 1–3-byte lengths and rejects non-canonical encodings.
- **`find_transaction`** finds a transaction by its first signature (its id).
- **`parse_footer`** reads the footer marker. The layout is Agave's `BlockFooterV1`: bank hash, producer time, user agent, then an optional **finalization certificate** (slot, block_id, aggregate signature and bitmap, plus an optional notar aggregate on the slow path). **Block S's footer certifies its parent**, so S's own certificate is in its child's footer. That means shreds alone are enough to prove finality, with no plugin and no provider.

Real layout seen locally: **header → entries → footer → one tick entry**, with each component padded to whole FEC sets. That padding is why tiny blocks still have 3–4 FEC sets.

*Tests:* `tests/block_contents.rs`. It finds our real memo transfer byte-for-byte equal to RPC's copy, and checks that each footer's certificate equals the plugin's copy byte for byte.

### `src/tx.rs`: the transaction format
Parses **legacy and v0** transactions with no Solana crates:
- signatures;
- the optional version byte;
- the 3-byte header;
- account keys;
- the recent blockhash;
- instructions (program index, account indexes, data);
- v0 address-table lookups.

`sanity_check` applies the runtime's structural rules (signature count matches the header, fee payer writable, indexes in range, at most 256 accounts).

Handy accessors: `signers()` and `program_id(ix)`. `message` is exactly the bytes the signatures sign.

**Important limit:** a transaction being *in a finalized block* doesn't mean it *succeeded*. Failed transactions are included too (they pay fees). Proving success needs execution results, which Solana doesn't sign today. See Phase 7.

### `src/proof.rs`: the statements we prove
This ties everything together. Three statements, each building on the last:

1. **`FinalityInput::verify` → `Finality`**: "block_id X at slot S is final under the validator set with hash H."
   - Fast path: one FinalizeFast at 80%.
   - Slow path: Notarize(block) at 60% **and** Finalize(slot) at 60%. Finalize alone names no block, which is the "slow-path trap".
   - Keys are decompressed lazily, only for signers, because decompression is costly in the zkVM.
2. **`BlockInput::verify`**: plus "these data shreds are that entire block".
3. **`TxInput::verify` → `TxOutput`**: plus "this transaction is in that block". **`TxInclusion`** is the efficient part. It needs:
   - every FEC root (32 bytes each), which rebuilds the block_id;
   - only the shreds of the one component holding the transaction, each proven against its FEC root;
   - the single shred before them, flagged DATA_COMPLETE, so the run starts at a real boundary and can't be read "out of frame".

   `from_block` builds the smallest such inclusion from a full block.

Also here:
- **`validator_set_hash`**: the commitment (item 1.1), `sha256("huginn-validator-set-v1" | count | pubkey,stake …)` in rank order.
- **`TxOutput::encode`**: the public values as a **fixed big-endian byte layout**, with offsets documented on the function. Any chain can slice it. Provisional until item 4.1.

*Tests:* `tests/proof.rs` (18 tests; section 9 lists what they cover).

### `src/fixture.rs` (feature `fixture`, host only)
Builds inputs for tests and the SP1 runner:
- `block_input(json)` and `tx_input(json, sig)` turn a `ledger-dump` fixture into proof inputs;
- `synthetic_set(n)` / `sign()` / `synthetic()` / `synthetic_slow()` make **n fake validators** with deterministic secret keys and real signatures. We need them because the local chain has only one validator, and mainnet-sized costs need ~1,000+.

### `tests/`
- `block_contents.rs`: real block 46718 parses into header / entries / footer / tick. Our transaction is found with bytes identical to RPC's.
- `proof.rs`: the three statements on real and synthetic data, plus every attack (section 9).

---

## 5. `crates/alpenglow-lc`: the reference (Agave-based)

**Purpose:** the same checks done with **Agave's own crates** (`agave-bls-cert-verify`, `agave-votor-messages`, `solana-bls-signatures`). It's the yardstick `alpenglow-core` is measured against. It also builds validator sets the way Agave ranks them. It can't run in SP1 (C code inside), and doesn't need to.

| File | What it does |
|---|---|
| `src/lib.rs` | Modules; re-exports `alpenglow_core::shred` so older code keeps working. |
| `src/validator_set.rs` | **Ranking rules**, copied from Agave's `epoch_stakes.rs`: drop zero-stake, missing or invalid keys; drop *every* copy of a duplicated BLS key or node identity; sort by stake descending, then key bytes. **Rank = position.** Also `commitment()` and `to_proof_validators()`, the bridge to `alpenglow-core`. |
| `src/cert.rs` | Calls Agave's `verify_certificate`. `RpcGenesisCert` (the `getAgGenesisCert` response) and `FooterCert` (what the footer plugin writes) become Agave's certificate type. `verify_footer` handles the fast and slow paths. Unit tests use real BLS keys. |
| `examples/find_genesis_signers.rs` | Phase 0 research tool. For the devnet genesis cert, it searched every 11-of-22 subset of today's validators to find who signed. That was how we first proved our payload encoding was right. |
| `tests/devnet_genesis.rs` | The real devnet genesis certificate through Agave's verifier, with tamper tests. |
| `tests/local_footers.rs` | Real local footer certs (sample + an opt-in run over all of them), tamper tests, and the set-commitment check. |
| `tests/local_block.rs` | Real block 44000: certs verify, shreds rebuild the certified block_id, tampering fails. |
| `tests/core_matches_agave.rs` | **The most important test file in the repo.** `alpenglow-core` and Agave must agree on: payload bytes; all ~45k captured local certs (opt-in); the devnet genesis cert; and 400 random valid or tampered certs. That's why we can trust the zkVM code. |

---

## 6. Data tools (your machine only)

### `plugins/footer-dump`: capturing finality certificates
A **Geyser plugin**: a shared library the validator loads, which it calls on events. Ours implements one hook, `notify_block_footer`, and writes one JSON line per block footer (slot, bank hash, and the raw finality certificate in hex) to `fixtures/local/footers.jsonl`.

- **Separate workspace and pinned compiler** (`rust-toolchain.toml` = 1.98.1, Agave 4.4.0's compiler). A plugin shares memory with the validator, and Rust has no stable binary layout between compiler versions.
- `config.json` is what you pass to `--geyser-plugin-config`.
- `src/lib.rs` is ~100 lines: `on_load` opens the output file, `notify_block_footer` writes a line, and `_create_plugin` is the symbol the validator looks up.

### `crates/fetch`: data from RPC
A CLI (`cargo run -p fetch -- <command> --cluster local|devnet|mainnet`):
- `genesis-cert`: saves `getAgGenesisCert`. It refuses the all-zero placeholder a chain born on Alpenglow returns.
- `cluster-info`: the shred version (the most common one across nodes).
- `validator-set`: reads every vote account, parses its BLS key (`src/vote_account.rs` knows the byte offsets in VoteStateV4), ranks them, and writes `validator_set_<epoch>.json` with the **commitment**. RPC only serves *current* stakes, so this must run during each epoch you want fixtures for.

### `crates/ledger-dump`: block fixtures from any shred source
Since block N+1's footer holds the certificate for block N, **shreds are all Huginn needs**. So this crate is built around one small interface (`src/lib.rs`):

```rust
trait ShredSource {
    fn range(&self) -> Result<(u64, u64)>;              // which slots it has
    fn data_shreds(&self, slot) -> Result<Vec<Vec<u8>>>; // the block's shreds
    fn code_shreds(&self, slot) -> Result<Vec<Vec<u8>>>; // optional extra check
}
```

Two sources today:
- **`Ledger`**: an Agave ledger's RocksDB, read-only, so it's safe while the validator runs. That's your local validator, or any node's ledger on devnet or mainnet. It uses the exact RocksDB version Agave 4.4.0 uses.
- **`ShredFiles`**: a directory of `shreds_<slot>.json` files, written by `export`. Use it offline and in tests.

A Jito ShredStream receiver or a repair-protocol client would be a third `impl ShredSource`. Nothing else would change.

**`build_fixture(source, slot, sets, certs)`** is the same for every source. It:
- takes the certificates from the child block's footer (`Certs::Shreds`, the default) or from footer-dump (`Certs::Plugin`);
- checks the coding shreds against their FEC roots, the shred version, both certificates against the epoch's validator set, and the rebuilt block_id;
- returns a fixture **only if every check passes**.

The CLI (`src/main.rs`):

```bash
cargo run --release -p ledger-dump                               # newest certified slot, from .local-ledger
cargo run --release -p ledger-dump -- --slot S                   # a given slot
cargo run --release -p ledger-dump -- --export-shreds S..S+1 --out dir   # save raw shreds
cargo run --release -p ledger-dump -- --shreds dir --slot S      # build from saved shreds, offline
cargo run --release -p ledger-dump -- --certs plugin             # certificates from footer-dump
```

*Tests:* `tests/shred_files.rs` builds a fixture from two committed shred files (`fixtures/local/shreds/`), with no ledger, plugin or RPC. It then proves the memo in it final and included.

---

## 7. `zk/`: the zero-knowledge part

Two separate Cargo workspaces, because the program compiles for RISC-V and the runner pulls in the large `sp1-sdk`.

### `zk/program`: what gets proven
`src/main.rs` (~25 lines):
1. read a `TxInput`;
2. `finality.verify()`;
3. `inclusion.verify(...)`;
4. commit `TxOutput::encode()` as public values.

If anything fails it panics, and then no proof can exist. The `cycle-tracker` lines split the cost report into finality and inclusion.

`Cargo.toml` patches `bls12_381` and `sha2` to SP1's accelerated versions; the tags come from SP1 v6.10.0's own examples. Build with `cargo prove build`.

### `zk/prover`: the library others build on (crate `huginn-prover`)
`build.rs` compiles the program. **`src/lib.rs`** is what a relayer for any destination chain depends on:
- `tx_input_from_block_file(block, signature)`: a `ledger-dump` block file plus a signature (base58 or hex) gives the proof input.
- `Huginn::from_env()`: an SP1 prover. Where it runs follows SP1's env: `SP1_PROVER=cpu` (default) or `mock`.
- `execute(&input)`: cycles, without a proof.
- `prove(&input, Mode::Groth16)`: returns `Proven`:
  - `program_vkey`: what destination contracts pin;
  - `public_values`: the fixed-layout output;
  - `onchain_proof`: the bytes a verifier contract takes;
  - timings.

  Every proof is verified, and its output must equal native Rust's, before it's returned.
- `Proven::summary(...)`: the JSON proof file that destination repos read.

**`src/main.rs`** is the `huginn` CLI on top of it. It runs the real local transaction and synthetic sets (`--synthetic 100,1000,1500`, `--slow 1000`), and with `--prove core|compressed|groth16` writes `fixtures/proofs/<run>.<mode>.json`. Needs `protoc` (`brew install protobuf`); Groth16 also needs Docker and SP1's circuit files in `~/.sp1/circuits/groth16/v6.1.0`.

## 7b. Destinations live in their own repos

huginn knows only Solana. Each destination chain is a **separate codebase** that depends on `huginn-prover` and has its own contracts, settings and scripts. The first one is **`hugin-evm`** (next to this repo):
- Solidity: `SolanaLightClient`, `HuginnOutput`, `SolanaTx`;
- a relayer crate using `huginn-prover`;
- `.env` for the chain, keys and addresses;
- Foundry deploy and submit scripts.

The proof output's layout (`TxOutput::encode`) is the contract between huginn and every destination. Change it only together with its version byte, and update each destination's decoder.

---

## 8. `fixtures/`: real data the tests run on

| Path | Contents | Made by |
|---|---|---|
| `devnet/genesis_cert.json` | Devnet's real Alpenglow genesis certificate (192-byte signature) | `fetch genesis-cert` |
| `devnet/validator_set_1198.json` | Devnet's ranked set at epoch 1198 | `fetch validator-set` |
| `devnet/cluster_info.json`, `local/cluster_info.json` | Shred versions (7016 devnet, 28090 local) | `fetch cluster-info` |
| `mainnet/genesis_cert.json` | Mainnet's genesis certificate (not yet verifiable: the historical set isn't available) | `fetch genesis-cert` |
| `local/validator_set_0.json` | The local chain's set for epoch 0 (1 validator) and its commitment | `fetch validator-set --cluster local` |
| `local/footers_sample.jsonl` | 7 footer lines: 6 with certs, 1 without | footer-dump, sampled |
| `local/footers.jsonl` | **Not committed.** Every footer since the plugin started; grows while the validator runs | footer-dump |
| `local/block_44000.json` | A whole empty block: set, certs, 96 data shreds | `ledger-dump --slot 44000` |
| `local/block_46718.json` | The block holding our memo transfer: 128 data shreds, 4 FEC sets | `ledger-dump --slot 46718` |
| `local/tx_46718.json` | RPC's copy of that transaction, the independent reference for the shred parser | `getTransaction` |
| `local/shreds/` | Raw shreds of slots 133389–133390: a block with a memo transfer, plus its child (whose footer certifies it). Enough to prove the memo with nothing else | `ledger-dump --export-shreds` |
| `proofs/<run>.<mode>.json` | A real proof's summary: program key, public values, timings (the `.bin` proof itself isn't committed) | `zk/prover --prove` |

---

## 9. What the tests prove (attacks we refuse)

| Attack | Refused because | Test |
|---|---|---|
| Forged or flipped signature bit | BLS check / point decoding | `core_matches_agave`, `local_footers` |
| Signer bitmap edited | aggregate key changes → signature fails | `cert.rs` unit tests, `proof.rs` |
| Too little stake (real signatures) | threshold check, exact integer math | `genuine_signatures_below_threshold_fail` |
| **Slow-path trap**: Finalize(slot) passed off as finality of a chosen block | Notarize(block) required; Finalize read as fast fails | `slow_path_trap_…` |
| Wrong shred version (another cluster) | it's inside the signed payload | `shred_version_mismatch_fails` |
| Wrong epoch's validator set | signature fails | `wrong_epoch_set_fails` |
| Inflated stakes in the input | the set hash changes, so the destination chain's stored commitment won't match | `inflating_stakes_changes_the_set_commitment` |
| Shred byte changed / Merkle sibling changed | FEC root no longer matches | `changed_byte_…`, `merkle_proof_sibling_…` |
| Fake FEC root, wrong FEC count, wrong parent | block_id mismatch | `wrong_fec_root_or_parent_fails` |
| Reading a component out of frame | boundary shred must be DATA_COMPLETE | `component_boundary_is_enforced` |
| Shreds from another block / truncated run | not in block / unterminated | `truncated_or_foreign_shreds_fail` |
| Transaction not actually there | not found | `transaction_not_in_component_fails` |
| Malformed input crashing the verifier | every read is bounds-checked | `parsers_never_panic_on_mangled_shreds` |

---

## 10. Words you'll meet

| Term | Meaning |
|---|---|
| **Alpenglow** | Solana's new consensus. Validators send BLS-signed votes; aggregated votes form **certificates**. |
| **Certificate** | One aggregate BLS signature + a bitmap of who signed. FinalizeFast (80%) = final at once; Notarize + Finalize (60% each) = the slow path. |
| **Rank** | A validator's position in the sorted set; bitmaps index by rank. |
| **Shred version** | A number identifying the cluster, inside every signed vote, so votes can't be replayed across clusters. |
| **Shred** | A ~1.2 KB packet of a block. **Data shreds** carry content; **coding shreds** are error-correction copies. |
| **FEC set** | 32 data shreds (+ coding) sharing one Merkle root. |
| **block_id** | The double Merkle root over FEC roots + parent info. It's what votes sign. |
| **Component** | A unit of block content: an entry batch or a marker. |
| **Entry** | A batch of transactions plus a Proof-of-History hash. |
| **Footer** | A marker at the end of every block, carrying the finality certificate for an earlier block. |
| **Geyser plugin** | A library loaded into the validator to receive events. |
| **zkVM / SP1** | Runs a normal program and proves it ran correctly. The proof is checked far more cheaply than re-running. |
| **Cycles** | Instructions executed in the zkVM; proving cost grows with them. |
| **Precompile** | A zkVM shortcut that makes one expensive operation (curve math, SHA-256) cheap. |
| **Public values** | The program's committed output, the part the destination chain reads. |
| **Groth16** | The final tiny proof (~260 bytes) that EVM chains verify cheaply. |
