# Huginn — Plan

This file has two parts:

1. **Revision 1 (current direction)**, below. It replaces the goal of v1.
2. **Version 1 (original plan)**, kept unchanged further down. Most of its reference material (sections 8.1 to 8.7, risks, references) still applies.

Corrections to v1 found by reading Agave's source and real data are listed in [`docs/findings.md`](docs/findings.md). Where the two disagree, findings.md wins. The build order lives in [`CHECKLIST.md`](CHECKLIST.md).

---

## Revision 1 (1 Nov 2026): from escrow demo to general Solana → Base messaging

We're changing the goal. Instead of a cross-chain escrow, we're building a general tool: a Base contract that can verify, by itself, that something really happened on Solana. Escrow and refunds become one app on top of it, later.

**What it proves (Layer 1, building now):** that any Solana transaction landed in a finalized block, at a given slot, signed by given keys, with given instructions. This is checked against Solana's own validators through Alpenglow certificates, plus a proof that the transaction is inside the block. It fully covers messages signed by a key: user intents, approvals, oracle posts, governance votes.

**What it can't prove yet (Layer 2, research):** program state and execution results, like "this PDA says FILLED". Solana doesn't sign those. Options:
- an optimistic claim with a bond,
- a ZK proof over the block's account changes (lattice hash),
- a small protocol change (SIMD).

**What changes in the build:**
- **Base:** a generic `SolanaLightClient` and a `SolanaTx` library that any contract can use, instead of an escrow.
- **Solana:** no custom program needed (Memo works), plus an optional small outbox with a standard message format.
- **Offchain:** one generic relayer. Give it a transaction signature, and it fetches the data, proves it and submits it.
- **Demos:** a cross-chain ping, a Solana key controlling a Base contract, and a forged proof being rejected.

**What stays the same:** the certificate check, data fetching, inclusion proof and SP1 wrapper. That's most of the hard work, and it was already general.

**Out of focus for now:** Base → Solana. It's doable today, but it's a separate project.

### How v1 sections map to Huginn

| v1 section | Status |
|---|---|
| 1–2 goal, escrow flow | Replaced by Revision 1. Escrow becomes a future app. |
| 3 scope | Layer 1 is in scope; escrow, fill program and bonds move to later apps. |
| 4 architecture | Same offchain pipeline. Base side becomes `SolanaLightClient` + `SolanaTx`. |
| 5 repo layout | Updated: see README. No `fill_program`; adds `relayer`, `contracts/examples`, optional `outbox`. |
| 7 phases | Re-cut in `CHECKLIST.md`. |
| 8.1–8.7 light client specs | Still valid, with the corrections in findings.md. |
| 8.8 public values | Replaced by generic values: validatorSetHash, epoch, shredVersion, slot, blockId, finalityPath, tx message hash, plus the tx bytes or a commitment to them. |
| 9–10 fill program, escrow | Deferred to the escrow app. |
| 13–14 risks, dependency strategy | Still valid. |

---

## Version 1 (original, unchanged)

# Build Plan: Near-Native Solana Cross-Chain Settlement

Version 1, 1 November 2026. Owner: Steve (AlphaR2).

This is the working plan for building the demo we discussed: show that Solana can take part in proof-based cross-chain settlement, close to "native", with a small custom build.

How to use it:

- Read sections 1 to 4 once, to get the picture.
- Work through section 7 phase by phase. Each phase has tasks, a "done when" check, and the risks to watch.
- Sections 8 to 11 are the reference specs you come back to while coding.
- Anything marked **Confirm** is a belief from research that we must check against real data before trusting it.

---

## 1. The goal, in plain words

We want to send the HyperFX CEO something he can run and watch, that shows:

1. **Refunds don't need any proof from Solana.** Stop fills on Solana at a cutoff slot. Let the user refund on Base after a later deadline. The refund checks only Base's own clock.
2. **A Base contract can check, by itself, that something really happened on Solana.** Since Alpenglow, Solana finality is one BLS signature check. We add a proof that the solver's fill transaction is inside that final block.

What we are **not** claiming:

- We are not proving the fill transaction *succeeded*. Solana doesn't make that provable yet. A solver bond covers it.
- We are not removing all trust. For now we post each epoch's validator list ourselves, and we say so.

The honest one-line pitch:

> "Solana can't give you EVM-style storage proofs. But with a different design, refunds need no proof at all, and fills can be checked against Solana's own validators instead of a multisig."

---

## 2. The idea again, step by step

Think of it as a courier job between two cities.

1. **The user locks money on Base.** That's the escrow. The order says: "pay this person this much cNGN (or a test token) on Solana, before slot X."
2. **A solver pays the user on Solana** from its own pocket, by calling our small Solana program. The program only accepts the payment if it is before the cutoff slot, and it records the order as FILLED.
3. **The solver goes back to Base to get paid.** It brings a receipt (a ZK proof) that says:
   - "this Solana block is final" (checked against Solana's validators), and
   - "this exact fill transaction is in that block, and it says order X, amount Y, person Z."
4. **Base checks the receipt.** If it's good, the solver posts a small bond and waits a short challenge window, then gets the escrow.
5. **If nobody fills, or the fill failed,** the user waits until the refund time and takes their money back on Base. No proof from Solana needed.

The key rule from our research:

> Only what is written inside the transaction's own bytes can be proven. Not logs, not events, not what the program wrote while running.

So the fill transaction must carry the order details in its own instruction data, and the Solana program must reject the transaction if those details are false. Then "the transaction landed and succeeded" means "the details are true". The "succeeded" part is what the bond covers.

---

## 3. What's in scope and what's not

**In scope for the demo**

- A Rust light client crate for Alpenglow (`alpenglow-lc`): check certificates and check a transaction is inside a block.
- An SP1 program that runs that crate and outputs a small receipt.
- A Solana program (Anchor) for fills, on devnet.
- An escrow contract and verifier on Base Sepolia.
- Scripts that run four scenarios end to end.
- A one-pager and a short video.

**Out of scope for now (backlog)**

- Proving a transaction succeeded (the lattice-hash proof, "Rung 4").
- Trustless validator set updates (needs a SIMD or similar).
- A solver network, pricing, FX quotes, a UI.
- CCTP, real cNGN, mainnet funds.
- Ethereum L1 deployment.
- Attestor fallback.

---

## 4. Architecture

### 4.1 The pieces

```
                         SOLANA (devnet)                                   BASE SEPOLIA
                 ┌─────────────────────────────┐                ┌──────────────────────────────────┐
                 │  fill_program (Anchor)       │                │  Escrow.sol                       │
                 │  - fill(order data)          │                │  - place(order)                   │
 solver ───────► │  - checks slot < cutoff      │                │  - claim(proof)  ──► IVerifier    │
                 │  - pays beneficiary          │                │  - finalizeClaim() after window   │
                 │  - creates FillRecord PDA    │                │  - challenge() during window      │
                 └──────────────┬──────────────┘                │  - refund() after T_refund        │
                                │                                │                                   │
                       block + certificates                     │  AlpenglowSP1Verifier.sol         │
                                │                                │  - stores validator set hash      │
                                ▼                                │    per epoch                      │
                 ┌─────────────────────────────┐                │  - calls pinned SP1 verifier      │
                 │  OFFCHAIN (your machine)     │                │  - checks public values match     │
                 │  fetch: certs, shreds,       │   proof +      │    the escrow order               │
                 │         validator set        │  public values └──────────────────────────────────┘
                 │  alpenglow-lc (Rust crate)   │ ─────────────────────────►
                 │  sp1 program (wraps the      │
                 │         crate) -> proof      │
                 └─────────────────────────────┘
```

### 4.2 Who trusts what in the demo

- **User refund:** trusts only Base. Nothing else.
- **Solver payment:** trusts Solana's consensus (via certificates), the validator list we posted, SP1's correctness, and the bond + challenge window for "did the fill succeed".
- **Damage limit:** per-order cap and a pause switch.

### 4.3 Why each design choice

- **Two time windows** so refunds never need proof (section 9.4).
- **ZK (SP1) instead of hand-written Solidity BLS**, so we reuse Agave's exact verification code instead of rewriting risky cryptography.
- **A wrapper crate with our own interface**, so Agave version changes stay inside one crate.
- **An `IVerifier` interface**, so we can swap verifiers later without touching escrow.
- **Devnet for the full flow, mainnet data for credibility.** Our program lives on devnet, and we also verify a real mainnet certificate to show the verifier works on the real network.

---

## 5. Repo layout

Monorepo. Suggested name: `alpenglow-settle` (change as you like).

```
alpenglow-settle/
├── README.md                    what this is, how to run the demo
├── PLAN.md                      this file
├── docs/
│   ├── 01-how-chains-handle-success-failure-refunds-proofs.md
│   ├── 02-solving-it-on-solana.md
│   ├── findings.md              running log: what we confirmed, what was wrong
│   └── one-pager.md             the thing you send to the CEO
├── crates/
│   ├── alpenglow-lc/            the light client (no SP1, no network code)
│   │   ├── src/lib.rs           public API
│   │   ├── src/validator_set.rs ranks, stake, commitment hash
│   │   ├── src/certificate.rs   finality checks (wraps Agave)
│   │   ├── src/inclusion.rs     shred -> FEC set -> double Merkle -> block_id
│   │   ├── src/tx.rs            parse tx bytes, find our instruction
│   │   └── tests/               fixture-driven tests
│   └── fetch/                   CLI: pulls certs, validator sets, shreds, writes fixtures
├── sp1/
│   ├── program/                 runs inside SP1: calls alpenglow-lc, commits public values
│   └── script/                  host: loads fixture, proves, writes proof + public values
├── solana/
│   └── programs/fill_program/   Anchor program
├── contracts/                   Foundry
│   ├── src/Escrow.sol
│   ├── src/IVerifier.sol
│   ├── src/AlpenglowSP1Verifier.sol
│   ├── test/
│   └── script/
├── fixtures/
│   ├── mainnet/                 genesis cert, real certs, validator sets
│   └── devnet/                  our fill blocks, shreds, certs
└── scripts/
    └── demo/                    the four scenarios
```

Rule: `alpenglow-lc` must not depend on SP1, Solana RPC, or anything networked. It takes bytes in and returns yes/no plus extracted data. That keeps it portable (normal Rust, SP1, RISC Zero, maybe a Solana program later).

---

## 6. Tools and versions

Pin exact versions in `Cargo.toml`, `Anchor.toml` and `foundry.toml` on day one, and write them in `docs/findings.md`.

| Tool | Use | Notes |
|---|---|---|
| Rust stable | everything Rust | SP1 needs its own toolchain too (installed by `sp1up`) |
| `agave-bls-cert-verify` 4.4.x | certificate verification | Same code validators use. Depends on `rayon`, which may not work inside SP1 (see Phase 0) |
| `agave-votor-messages` 4.4.x | vote and certificate types | Match the same 4.4.x line |
| `solana-bls-signatures` | BLS math | Research referenced 4.0.2; use whatever 4.4.x pulls in |
| `solana-signer-store` | likely the ranked validator list | **Confirm** what it holds |
| Agave ledger shred code | shred and Merkle structures | May need to copy small parts instead of depending on the whole crate |
| SP1 (latest stable) | zkVM | Pin the version. Its BLS12-381 precompiles matter for speed |
| Anchor (latest stable) | Solana program | Devnet |
| Foundry | Solidity build/test/deploy | Base Sepolia |
| Solana CLI with Agave v4.3+ | RPC calls like `getAgGenesisCert` | `solana alpenglow-genesis-info` is the CLI version |

Networks:

- **Solana mainnet-beta:** read only. Alpenglow active since slot 459,653,000 (21 Oct 2026).
- **Solana devnet:** our program. Alpenglow migrated on devnet in late September 2026. **Confirm** with `getAgGenesisCert` on devnet.
- **Base Sepolia:** contracts. EIP-2537 BLS precompiles are present on OP Stack chains since the Isthmus upgrade. With the SP1 route we don't call them directly, but they're our fallback.

---

## 7. Phases

Rough timeline: about 3 weeks of focused work. Phases 5 and 6 can run alongside phases 1 to 4.

### Phase 0: Setup and the go/no-go check (days 1 to 2)

**Why:** everything depends on one question. Can we run Agave's certificate check, on a real certificate, natively and then inside SP1?

**Tasks**

1. Create the repo and folders from section 5. Copy the two research docs into `docs/`.
2. Install toolchains: Rust, `sp1up`, Anchor, Solana CLI, Foundry.
3. Fetch the mainnet genesis certificate:
   - `getAgGenesisCert` over mainnet RPC (or `solana -um alpenglow-genesis-info`).
   - Save the raw JSON to `fixtures/mainnet/genesis_cert.json`.
4. Build the validator set for that epoch (epoch 1064):
   - Get vote accounts and their stake for the epoch.
   - Read each vote account's registered BLS public key (added by SIMD-0387). Parse the account data.
   - Apply the ranking rule from section 8.3.
   - Save to `fixtures/mainnet/validator_set_1064.json`.
   - Sanity check: around 654 participating validators at the switch, and the genesis certificate carried 446 of them (from research). Our counts should be close.
5. In a scratch Rust binary, call `agave_bls_cert_verify::cert_verify::verify_certificate` on the genesis certificate. Work out from the crate's code what it needs as input for the validator set.
6. Try compiling the same call inside a minimal SP1 program.

**Done when**

- The genesis certificate verifies natively, and a tampered copy (one bit flipped in the signature) fails.
- We know whether the Agave crate compiles in SP1.

**Go / no-go**

- **Compiles in SP1:** continue with the SP1 route.
- **Fails only because of `rayon` or a few dependencies:** copy the small verify function into `alpenglow-lc` (Apache-2.0 licensed), strip the threading, continue.
- **Fails badly:** fall back to the hand-written Solidity verifier (BLS via EIP-2537). Same plan, but Phase 4 becomes "write Solidity BLS + hash-to-curve". Add about a week.

**Risks**

- The certificate JSON format from RPC may differ from the wire format the crate expects. Expect some conversion code.
- The BLS key field in vote accounts may need a newer `solana-vote-interface` to parse.

---

### Phase 1: The light client crate, finality part (days 3 to 6)

**Why:** this is the core. A small crate with a simple, stable API that we own.

**Tasks**

1. Define types:
   - `ValidatorSet { epoch, shred_version, entries: Vec<(bls_pubkey, stake)>, total_stake }`, already in rank order.
   - `Finality { slot, block_id, path: Fast | Slow }`
2. Implement `ValidatorSet::commitment()` (section 8.4). This 32-byte hash is what Base stores.
3. Implement `verify_finality(set, certs) -> Result<Finality>`:
   - **Fast path:** one fast-finalization certificate (80% of stake notarized `(slot, block_id)`).
   - **Slow path:** a finalization certificate for the slot (60%) **plus** a notarization certificate for `(slot, block_id)` (60%). Both required.
   - Reject any certificate whose `shred_version` doesn't match the set's.
   - Reject mixed certificates (three-state bitmap) for now. Note it in `findings.md`.
4. Write fixture-driven tests: load from `fixtures/`, expect pass or fail.
5. Write the adversarial tests from section 10.2 now, not later.

**Done when**

- The genesis certificate passes through our API.
- At least 3 more real certificates pass (as soon as Phase 2 can fetch them).
- All adversarial tests fail the way they should.

**Risks**

- **The slow-path trap.** A finalization certificate signs only the slot, not the block. If we accept it without the matching notarization certificate, an attacker can pair a real finality with a fake block. Test this explicitly.
- Rank ordering mismatch. If the bitmap doesn't line up with our list, nothing verifies. Debug by comparing against how Agave builds its list in `epoch_stakes.rs`.

---

### Phase 2: Getting the data (days 3 to 8, alongside Phase 1)

**Why:** this is the most practical risk. Certificates and shreds aren't served by normal RPC calls.

**What we need**

| Data | For | Where to get it |
|---|---|---|
| Genesis certificate | Phase 0 | `getAgGenesisCert`, plain RPC |
| Certificates for a chosen slot | Phase 1 and 4 | Block footers (each leader puts the latest finalization certificate it knows in its footer), or a node's certificate pool |
| Validator set per epoch | everything | Vote accounts + epoch stakes over RPC |
| Raw shreds of a chosen block | Phase 3 | A node's blockstore, or the Old Faithful / Triton faithful archive |
| `block_id` of a block | Phase 3 | Agave stores it in the `double_merkle_meta` column (per the Triton faithful archive work) |

**The recommended route: run a devnet node**

A non-voting Agave node on **devnet** is much lighter than a mainnet node and gives us everything for our own fill blocks:
- shreds in its blockstore,
- block ids,
- footers through Geyser (`notify_block_footer`, opt in with `block_footer_notifications_enabled()`),
- possibly the full certificate pool.

For mainnet, we only need a few fixtures (genesis cert plus a handful of footers), so an RPC provider or the archive is enough.

**Tasks**

1. Decide: devnet node on a rented server, or a paid gRPC provider for devnet. Write the decision in `findings.md`.
2. Build `crates/fetch` with commands:
   - `fetch genesis-cert --cluster mainnet|devnet`
   - `fetch validator-set --cluster ... --epoch N`
   - `fetch footer --cluster ... --slot S`
   - `fetch block --cluster ... --slot S` (shreds + block_id)
3. Each command writes a fixture file with a clear format (section 8.6).

**Open questions to settle in this phase**

- Do gRPC footer streams include the certificates? Triton's blog says you can request them (`include_certificates: true`). Solana's Alpenglow page says the Geyser footer message forwards slot, bank id, bank hash and similar, but **not** the certificates. Test it.
- For a given slot S, which certificates can we actually get? A footer carries the latest finalization certificate the *leader* knew about, which may be for an earlier slot. If we can't get one for slot S directly, we use a later certified block and walk back through parent block ids (section 8.7).

**Done when**

- We can produce, on demand, a full fixture for any recent devnet slot: certificates proving it final, its shreds, its block_id, and the epoch's validator set.

---

### Phase 3: The light client crate, inclusion part (days 7 to 11)

**Why:** finality alone says "a block is final". We need "this transaction is in it".

**Tasks**

1. Implement the path from transaction to block_id (section 8.5):
   - data shreds -> FEC set Merkle tree (64 leaves, 6 levels, 20-byte truncated nodes)
   - FEC set roots -> double Merkle tree (last leaf = parent slot, parent block_id, FEC set count)
   - root = block_id
2. Rebuild the entry byte stream from the data shreds of the relevant batch, deserialize entries, find the transaction.
3. Parse the transaction (legacy and v0 message formats).
4. Find **our** instruction: top-level, program id = `fill_program`, decode its data (section 9.2).
5. Implement `verify_inclusion(block_id, shreds, tx_index) -> Result<FillClaim>`.
6. Optional: `walk_back(certified_block_id, headers) -> ancestor block_id` (section 8.7).

**Done when**

- For a real devnet fill transaction, the crate returns the right `FillClaim` and the path checks up to the certified block_id.
- Changing one byte in any shred, or any Merkle sibling, makes it fail.

**Risks**

- A transaction can span shreds, and entry batches must be parsed from a batch boundary. You may need every data shred of that batch, not just one.
- Exact shred layout changes between versions. Pin the Agave version and use their structs where possible.
- **Truncated nodes:** 20-byte Merkle nodes give about 80-bit collision resistance. Fine for a demo. Cap value per proof in production.

---

### Phase 4: SP1 program and Base verifier (days 10 to 14)

**Why:** this turns our Rust checks into a receipt Base can verify cheaply.

**Tasks**

1. `sp1/program`:
   - Read inputs: validator set, certificates, shreds/paths, tx index.
   - Call `alpenglow-lc`: `verify_finality`, then `verify_inclusion`, check the block ids match.
   - Commit public values (section 8.8).
2. `sp1/script`:
   - Load a fixture, run in execute mode first (fast, no proof) to check logic and count cycles.
   - Then produce a Groth16 proof for EVM. Use a local GPU or Succinct's prover network.
   - Write `proof.bin` and `public_values.bin`.
3. `contracts/src/AlpenglowSP1Verifier.sol`:
   - Stores `programVKey` (the SP1 program's verification key).
   - Stores `validatorSetHash[epoch]`, set by an owner (demo) with an event. Production: timelock + overlap check.
   - Calls a **pinned** SP1 verifier contract version, not a router someone else can update. **Confirm** the deployed address on Base Sepolia from SP1's docs.
   - Decodes public values and returns a `FillClaim`.
4. Foundry tests with real proof fixtures. Record gas.

**Done when**

- A real devnet fill produces a proof that `AlpenglowSP1Verifier` accepts on Base Sepolia.
- A proof made with the wrong validator set hash, or for a different order, is rejected.

**Risks**

- Proof time and cost. Measure cycles in execute mode first. BLS pairing and hashing many shreds are the heavy parts.
- SP1 version drift. Pin it, and keep the verifier contract address pinned to the matching version.

---

### Phase 5: Solana fill program (days 4 to 7, in parallel)

Spec in section 9. Small, but the rules matter.

**Tasks**

1. Anchor program `fill_program` with one instruction, `fill`.
2. Create a test SPL token on devnet (stand-in for cNGN). Mint to a solver wallet.
3. Tests: happy path, after cutoff, wrong beneficiary account, double fill, insufficient balance.

**Done when**

- All tests pass on a local validator and on devnet.
- A successful fill creates exactly one `FillRecord` PDA, and a second fill for the same order fails.

---

### Phase 6: Base escrow (days 5 to 9, in parallel)

Spec in section 10.

**Tasks**

1. `IVerifier.sol`, `Escrow.sol`, and a `MockVerifier.sol` for early testing.
2. Test token on Base Sepolia (stand-in for USDC).
3. Foundry tests:
   - place -> claim (mock) -> wait window -> finalize: solver paid, bond returned.
   - place -> no claim -> refund after `T_refund`: user paid.
   - claim after `T_refund`: rejected.
   - refund before `T_refund`: rejected.
   - claim -> challenge upheld -> user refunded, bond to user.
   - double claim, double refund, claim after refund: all rejected.
   - amount above cap: rejected at `place`.

**Done when**

- All tests pass with `MockVerifier`, then with `AlpenglowSP1Verifier` on a real fixture.

---

### Phase 7: End-to-end demo (days 15 to 18)

**The four scenarios**

1. **Filled and claimed.** User places on Base. Solver fills on devnet. Scripts fetch data, prove, claim on Base. Window passes, solver paid.
2. **Nobody fills.** User places. Nothing happens. After `T_refund`, user refunds. Show there was no Solana proof involved.
3. **Fill fails onchain.** Solver sends a fill that fails (for example, not enough tokens). Show the failed transaction on the explorer, show no `FillRecord` exists, user refunds after `T_refund`. Optionally: a dishonest solver tries to claim using that failed transaction, and the challenge wins.
4. **Forged proof.** Submit a proof with a tampered certificate or the wrong validator set. Verifier rejects.

**Tasks**

1. `scripts/demo/` with one command per scenario, clear console output.
2. Record a 3 to 5 minute video.
3. Write `docs/one-pager.md` (section 12).

**Done when**

- Someone else can clone the repo and run scenario 2 and 4 with no special infrastructure, and see scenario 1 and 3 from recorded fixtures.

---

### Phase 8: After the demo (backlog, not now)

- Rung 4: lattice-hash proof of "the fill succeeded" (needs a cryptographer's review first).
- Draft a SIMD for an epoch validator set commitment (fixes trustless set updates).
- Attestor as a second, independent check for big amounts.
- Ethereum L1 (ZK route is the only affordable one there).
- Direction B (escrow on Solana, fill on EVM), which is fully provable today.
- Plug into existing frameworks (Hyperlane security module, LayerZero verifier network) once the verifier is solid.

---

## 8. Light client reference specs

Everything here comes from reading Agave code and SIMDs. **Confirm** each against real data in Phases 0 to 3, and record results in `findings.md`.

### 8.1 Alpenglow in one paragraph

Validators no longer vote with transactions. They send small signed messages to each other. These are combined into certificates: one aggregated BLS signature plus a bitmap of who signed. Finality is about 150 ms. Mainnet switched on 21 October 2026, slot 459,653,000. Safety assumes at most 20% malicious stake plus 20% offline. Slashing is not live yet.

### 8.2 What each vote signs

| Vote | Tag | Signed fields |
|---|---|---|
| Notarize | 1 | slot, block_id, shred_version |
| Finalize | 2 | slot, shred_version |
| Skip | 3 | slot, shred_version |
| Notarize-fallback | 4 | slot, block_id, shred_version |
| Skip-fallback | 5 | slot, shred_version |

- Source: `votor-messages/src/wire.rs`.
- **Confirm:** byte encoding (wincode, believed little-endian).
- Crypto: BLS12-381. Public keys 48 bytes (G1). Signatures 96 bytes compressed (G2).
- Hash-to-curve tag: `BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_`.
- Proof of possession at key registration: message `"ALPENGLOW" + vote account pubkey + BLS pubkey`.

### 8.3 Certificates and the validator list

| Certificate | Needs | Meaning |
|---|---|---|
| Fast-finalization | 80% notarize | Final in one round, names block_id |
| Notarization | 60% notarize | Block accepted, names block_id |
| Finalization | 60% finalize | Slot final. Does **not** name block_id |
| Notar-fallback | 60% notarize or notar-fallback | Recovery path |
| Skip | 60% skip | Slot skipped |

Building the ranked list for an epoch (from `runtime/src/epoch_stakes.rs`):

1. Take the epoch's vote accounts that have a registered BLS key.
2. Drop duplicate keys.
3. Sort by stake, highest first, then by key.
4. Position in that order = rank. The certificate bitmap is indexed by rank.
5. Total stake for thresholds = sum over this list only.
6. Hard cap: 2,000 validators.

### 8.4 Validator set commitment (our definition)

This is ours, not Solana's. Base stores only this hash.

```
commitment = sha256(
    "ALPENGLOW-LC-VSET-V1"           // domain tag, ours
 || epoch            (u64 LE)
 || shred_version    (u16 LE)
 || count            (u32 LE)
 || for each validator in rank order:
        bls_pubkey   (48 bytes, compressed G1)
     || stake        (u64 LE)
)
```

**Confirm:** whether vote accounts store the BLS key compressed (48 bytes) or uncompressed (96 bytes). Always hash the compressed form so the commitment doesn't depend on how it was stored.

The SP1 program recomputes this from its input and commits it as a public value. Base checks it equals `validatorSetHash[epoch]`.

### 8.5 Block structure and the inclusion path

```
transactions -> entries -> byte stream -> data shreds (~1 KB each)

FEC set   = 32 data shreds + 32 coding shreds
            Merkle tree over 64 shreds, 6 levels, child hashes truncated to 20 bytes

double Merkle tree over all FEC set roots
            last leaf = hash(parent slot, parent block_id, number of FEC sets)
            root = block_id

header (in shreds):  parent slot, parent block_id
footer (in shreds):  bank hash, producer time, user agent,
                     up to 3 optional certificates
```

Proof path for our transaction:

```
tx bytes
 -> data shred(s) of its entry batch
 -> FEC set Merkle path -> FEC set root
 -> double Merkle path -> block_id
 == certified block_id
```

Sources: `ledger/src/shred/merkle_tree.rs`, `ledger/src/blockstore.rs`, `entry/src/block_component.rs`, SIMD-0340.

### 8.6 Fixture format

JSON, one file per case, hex for bytes:

```json
{
  "cluster": "devnet",
  "epoch": 0,
  "validator_set": { "shred_version": 0, "entries": [["<bls_pubkey_hex>", 0]] },
  "certificates": [{ "kind": "notarize|finalize|fast_finalize", "slot": 0,
                     "block_id": "<hex or null>", "bitmap": "<hex>", "signature": "<hex>" }],
  "block": { "slot": 0, "block_id": "<hex>", "shreds": ["<hex>"], "tx_index": 0 },
  "expect": { "ok": true, "fill_claim": { "order_id": "<hex>", "amount": 0 } }
}
```

### 8.7 Walking back to an older block

If we can only get certificates for a later block L, we can still prove our block B (an ancestor):

- L's double Merkle tree commits to L's parent block_id.
- Repeat until we reach B.
- Each step needs only that block's last leaf and the path to it, not the whole block.

Finality of L implies its ancestors are final. Use only if direct certificates for B aren't available.

### 8.8 SP1 public values

```solidity
struct PublicValues {
    bytes32 validatorSetHash;   // section 8.4
    uint64  epoch;
    uint16  shredVersion;
    uint64  slot;
    bytes32 blockId;
    uint8   finalityPath;       // 0 = fast, 1 = slow
    bytes32 fillProgramId;      // must equal our program
    bytes32 orderId;
    bytes32 beneficiary;        // Solana pubkey of the user
    bytes32 mint;               // token mint on Solana
    uint64  amount;
    uint64  cutoffSlot;
    bytes32 txSigHash;          // sha256 of the tx's first signature (64 bytes)
}
```

ABI-encode inside SP1 so Solidity can `abi.decode` it directly.

---

## 9. Solana fill program spec

### 9.1 Accounts

```rust
#[account]
pub struct FillRecord {
    pub order_id: [u8; 32],
    pub solver: Pubkey,
    pub beneficiary: Pubkey,
    pub mint: Pubkey,
    pub amount: u64,
    pub slot: u64,
    pub bump: u8,
}
// PDA seeds: [b"fill", order_id]
```

Using `init` on this PDA means a second fill for the same order fails automatically. No separate "create order" step on Solana is needed.

### 9.2 The `fill` instruction

Instruction data (this is what Base reads from the transaction bytes):

```
order_id      [u8; 32]
beneficiary   Pubkey
mint          Pubkey
amount        u64
cutoff_slot   u64
```

Rules, all in one instruction:

1. `Clock::get()?.slot < cutoff_slot`, else fail. Use slots, not `unix_timestamp`: since Alpenglow the leader sets the timestamp, and it can drift.
2. The beneficiary token account must be the associated token account of `beneficiary` for `mint`. Check owner and mint.
3. Transfer `amount` of `mint` from the solver's token account to the beneficiary's. Same instruction.
4. `init` the `FillRecord` PDA with these values and the current slot.
5. Must be called as a **top-level** instruction (reject CPI calls by checking the instructions sysvar), so the data is in the transaction bytes.

Why this works: if the transaction succeeds, every field in its data is true (the transfer happened, before the cutoff, to the right person, once). If it fails, nothing happened.

### 9.3 What the program cannot know (and who checks it)

The Solana program doesn't know about the Base escrow. It can't tell if `order_id`, `amount` or `cutoff_slot` match a real order. **Base checks that** by comparing the proven instruction data to its stored order. That split is deliberate: each chain only checks what it can see.

### 9.4 Token edge cases

- Use a plain SPL test token for the demo.
- Real cNGN on Solana has blacklisting. A fill to a blacklisted user fails, and the user refunds by time, which is correct. **Confirm** how it's enforced before using real cNGN.
- Token-2022 transfer fees or hooks would change the received amount. Reject such mints for now.

---

## 10. Base contracts spec

### 10.1 Interfaces

```solidity
interface IVerifier {
    struct FillClaim {
        bytes32 orderId;
        bytes32 beneficiary;
        bytes32 mint;
        uint64  amount;
        uint64  cutoffSlot;
        uint64  slot;
        bytes32 txSigHash;
    }
    function verifyFill(bytes calldata proof, bytes calldata publicValues)
        external view returns (FillClaim memory);
}
```

### 10.2 Escrow

```solidity
struct Order {
    address user;
    address token;          // test USDC on Base
    uint256 amountIn;
    bytes32 beneficiary;    // Solana pubkey
    bytes32 mint;           // Solana mint
    uint64  amountOut;
    uint64  cutoffSlot;     // Solana slot
    uint64  refundAfter;    // Base timestamp
    Status  status;         // Open, Claimed, Settled, Refunded
}

function place(Order calldata o) external returns (bytes32 orderId);
function claim(bytes32 orderId, bytes calldata proof, bytes calldata pv) external payable; // bond
function challenge(bytes32 orderId, bytes calldata evidence) external;                    // demo: arbiter role
function finalizeClaim(bytes32 orderId) external;
function refund(bytes32 orderId) external;
```

`orderId = keccak256(abi.encode(block.chainid, address(this), user, nonce, order fields))`.

Rules:

- `claim` only before `refundAfter`. Checks the proven claim matches the order exactly (beneficiary, mint, amountOut, cutoffSlot). Requires a bond. Starts the challenge window.
- `finalizeClaim` after the window if not challenged: pay solver, return bond.
- `refund` after `refundAfter` if status is `Open`. No proof.
- One claim per order. One payout per order.
- Per-order cap on `amountIn`. Pause switch on `place` and `claim`. **`refund` is never paused.**
- No minting anywhere.

### 10.3 The challenge, honestly

In the demo, "did the fill succeed" is settled by an **arbiter role** (you), who checks the transaction status over RPC. Label it clearly in the README, the video and the one-pager. It's the open problem that Rung 4 or a SIMD would close. Since the user can always refund by time, the arbiter can only affect the solver's payout, never the user's safety.

### 10.4 Adversarial tests the verifier must fail

- Finalization certificate paired with a block_id it doesn't cover (slow path without notarize).
- Bitmap claims a signer who didn't sign.
- Stake below 60% / 80%.
- Duplicate or unregistered BLS key in the set.
- Certificate from a different epoch's set.
- `shred_version` mismatch.
- One byte changed in a shred, a Merkle sibling, or the tx.
- Instruction from a different program id.
- Our instruction called via CPI (should never be in a valid claim).
- Valid proof for order A used to claim order B.
- Same proof submitted twice.

---

## 11. Time windows and numbers

### 11.1 Time

- Solana slot target is 200 ms (SIMD-0525, live since epoch 1053). Not guaranteed.
- `cutoffSlot`: current slot + about 3 to 5 minutes' worth (900 to 1,500 slots).
- `refundAfter`: well after that. For the demo, 60 minutes after placement is fine, because proof generation can take minutes.
- Challenge window: 10 minutes for the demo.
- Rule: `refundAfter` minus cutoff time must exceed proof time + Base inclusion + challenge window + margin.

### 11.2 Gas and cost (rough)

- SP1 Groth16 verification on EVM: roughly 230k to 300k gas (IBC's Solidity light client reports about 230k for a Groth16 check).
- Fallback hand-written route: pairing about 103k, hash-to-G2 about 60k, plus key aggregation.
- Base Sepolia is free (testnet ETH).
- Proving: measure in Phase 4. Expect minutes per proof.

---

## 12. The demo package for the CEO

### 12.1 One-pager outline

1. **The claim we tested:** "Solana can't support proof-based settlement like EVM."
2. **What's true about it:** Solana has no storage proofs and no receipt proofs. Correct.
3. **What we built instead:**
   - Refunds by time: no proof needed from Solana.
   - Fills checked against Solana's own validators via Alpenglow certificates, plus proof the fill transaction is in the final block.
4. **The four scenarios** (with links to the video).
5. **What we trust, plainly:** Solana consensus, the posted validator list, SP1, and a bond for "did the fill succeed".
6. **What's next:** proving success (lattice-hash route or a SIMD), trustless validator updates.

### 12.2 Video script (about 4 minutes)

1. 30 s: the problem, one sentence each for EVM and Solana.
2. 60 s: scenario 1, filled and claimed.
3. 45 s: scenario 2, refund with no proof.
4. 45 s: scenario 3, failed fill, user refunds.
5. 30 s: scenario 4, forged proof rejected.
6. 30 s: what's trusted and what's next.

### 12.3 Tone

Don't oversell. He'll spot it. "Near native" is fair. "Trustless" is not, yet.

---

## 13. Risks and fallbacks

- **Agave crates don't compile in SP1.** Copy the small functions, strip threading. Worst case: hand-written Solidity verifier.
- **Can't get certificates for our slot.** Walk back from a later certified block (section 8.7), or capture from our own devnet node's certificate pool.
- **Can't get shreds.** Own devnet node is the reliable fix. Archive as backup.
- **Proof too slow or expensive.** Profile cycles. Split work: verify certificates once per epoch, prove only the inclusion per fill.
- **Our reading of Agave is wrong somewhere.** That's why Phase 0 starts with a real certificate. Log every correction in `findings.md`.
- **Agave or SP1 version changes.** Pinned versions, wrapper crate, fixtures that catch breaks.
- **Firedancer differences.** Not running Alpenglow yet. Formats must match byte for byte for consensus anyway. Watch it.

---

## 14. Dependency strategy (keep trust shallow)

1. `alpenglow-lc` knows nothing about SP1. It can move to another zkVM or a hand-written verifier.
2. Pin the SP1 verifier contract version. Don't point at a router someone else can update.
3. Generate proofs yourself when possible. The prover network is a convenience, not a requirement.
4. Escrow talks only to `IVerifier`. Swapping is a config change.
5. Caps and a pause switch limit damage if any piece breaks.
6. Production idea: require two different checks for big amounts (SP1 proof and an attestor).

---

## 15. Decisions to make before day 1

1. **Repo:** new public GitHub repo under AlphaR2, or private first?
2. **Data:** own devnet node (rented server) or paid gRPC provider?
3. **Proving:** local GPU or Succinct's prover network for the demo?
4. **Names:** repo, crate, program.
5. **Who drives:** you write with review, or I scaffold and explain as we go.

---

## 16. Day 1 checklist

- [ ] Create repo and folders.
- [ ] Install Rust, `sp1up`, Anchor, Solana CLI (Agave v4.3+), Foundry.
- [ ] `getAgGenesisCert` on mainnet, save fixture.
- [ ] `getAgGenesisCert` on devnet, confirm Alpenglow is active there.
- [ ] Pull epoch 1064 vote accounts, parse BLS keys, build ranked list, save fixture.
- [ ] Verify genesis certificate natively with `agave-bls-cert-verify`.
- [ ] Flip one bit, confirm it fails.
- [ ] Try the same inside a minimal SP1 program.
- [ ] Write results in `docs/findings.md`. Make the go/no-go call.

---

## 17. Glossary

- **Alpenglow:** Solana's consensus since 21 Oct 2026. Votes aggregate into certificates.
- **Certificate:** one aggregated BLS signature plus a bitmap of signers.
- **block_id:** Merkle root over a block's data. What notarize votes sign.
- **Shred / FEC set:** packet-sized pieces of a block. 32 data + 32 recovery shreds per FEC set.
- **Double Merkle root:** the tree over FEC set roots. Its root is the block_id.
- **Rank:** a validator's position in the stake-sorted list. Indexes the bitmap.
- **shred_version:** changes on emergency restarts. Mismatch means pause.
- **Light client:** a small verifier that follows another chain's consensus.
- **SP1:** a zkVM. Runs Rust and outputs a proof anyone can check.
- **Public values:** the outputs an SP1 proof commits to.
- **Escrow:** money held by a contract until a rule releases it.
- **Solver:** the party that pays the user on the other chain from its own funds.
- **Bond:** money a solver puts at risk when claiming. Lost if the claim is proven false.

---

## 18. References

**Our research**
- `docs/01-how-chains-handle-success-failure-refunds-proofs.md`
- `docs/02-solving-it-on-solana.md`

**Solana / Agave**
- Alpenglow upgrade page: https://solana.com/upgrades/alpenglow
- getAgGenesisCert: https://solana.com/docs/rpc/http/getaggenesiscert
- SIMD-0326 Alpenglow: https://github.com/solana-foundation/solana-improvement-documents/blob/main/proposals/0326-alpenglow.md
- SIMD-0340 block id / double Merkle root: https://github.com/solana-foundation/solana-improvement-documents/blob/main/proposals/0340-validate-chained-block-id.md
- SIMD-0387 BLS keys in vote accounts: https://simd.mixy.one/simd/0387-bls-pubkey-management-in-vote-account/
- agave-bls-cert-verify: https://docs.rs/agave-bls-cert-verify/latest/agave_bls_cert_verify/
- agave-votor-messages: https://docs.rs/agave-votor-messages/latest/agave_votor_messages/
- Vote payloads: https://github.com/anza-xyz/agave/blob/master/votor-messages/src/wire.rs
- Certificates: https://github.com/anza-xyz/agave/blob/master/votor-messages/src/certificate.rs
- Validator ranks: https://github.com/anza-xyz/agave/blob/master/runtime/src/epoch_stakes.rs
- Shred Merkle tree: https://github.com/anza-xyz/agave/blob/master/ledger/src/shred/merkle_tree.rs
- Blockstore / double Merkle root: https://github.com/anza-xyz/agave/blob/master/ledger/src/blockstore.rs
- Block header and footer: https://github.com/anza-xyz/agave/blob/master/entry/src/block_component.rs
- Triton: Alpenglow and footers: https://blog.triton.one/alpenglow-and-your-triton-stack-what-to-update-before-mainnet/
- Triton faithful archive (block ids, markers): https://github.com/rpcpool/radiance-triton-faithful/pull/54
- 200 ms slots live: https://solanacompass.com/news/solanas-200ms-slot-time-is-live-on-mainnet-at-epoch-1053-completing-simd-0525
- Alpenglow mainnet activation (slot 459,653,000): https://airdropalert.com/blogs/alpenglow-mainnet/

**ZK and EVM**
- Succinct SP1 Solana verifier: https://blog.succinct.xyz/learn/solana-sp1/
- SP1 BLS12-381 precompiles: https://blog.succinct.xyz/succinctshipsprecompiles/
- SP1 Helios with storage proofs (Across): https://github.com/across-protocol/sp1-helios
- IBC Solidity light client (Groth16 gas reference): https://github.com/cosmos/solidity-ibc-eureka
- EIP-2537: https://eips.ethereum.org/EIPS/eip-2537

**Context**
- Hyperbridge Intent Gateway, cancelling orders: https://docs.hyperbridge.network/developers/evm/intent-gateway/cancelling-orders/
- Hypernative, Hyperbridge exploit: https://www.hypernative.io/blog/how-three-compounding-failures-let-an-attacker-mint-1-2b-in-bridged-tokens
- Bankai (planned Solana support, Q2 2027): https://solanacompass.com/projects/bankai
- State of Solana light clients (Tinydancer stopped): https://forum.solana.com/t/state-of-tinydancer-light-clients-on-solana/4762
