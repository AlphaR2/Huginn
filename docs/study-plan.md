# Study plan: own Huginn end to end

A path through both repos, in an order where each step uses what the last one taught. Every phase has three parts:
- **Read:** the files, and the questions you should be able to answer afterwards.
- **Do:** commands to run, and what you should see.
- **Journal:** what to write down in your own words.

Roughly 8 sessions of 2–3 hours each. Don't skip the "break it" parts: changing code and watching a test fail is the fastest way to see what each line protects.

Paths are relative to the folder holding both repos (`/Users/mac/works/hobbs`), unless a step says `cd`.

---

## Phase 0: get your machine ready (one session)

The cleanup removed SP1 and the Solana CLI. Foundry is back. Follow [`run.md`](run.md) part 1, "Tools", for each missing one:

```bash
curl -L https://sp1up.succinct.xyz | bash
```

```bash
sp1up --version v6.10.0
```

```bash
sh -c "$(curl -sSfL https://release.anza.xyz/v4.4.0/install)"
```

Open a new terminal, then check:

```bash
cargo prove --version && solana --version && forge --version && protoc --version
```

You want: SP1 6.10.0, solana-cli 4.4.x, forge 1.9.x, any protoc.

Then make sure both repos build and every test passes before you read anything, so any failure later is yours:

```bash
cd huginn && cargo test --release
```

Expect about 99 passed, 3 ignored.

```bash
cd huginn-evm && forge test
```

Expect 49 passed, 5 skipped (the Groth16 tests, until a real proof exists).

**Journal:** write down the version of every tool. When something breaks in a month, this is the first thing you'll want.

---

## Phase 1: the big picture, no code (one session)

**Read**, in this order:
1. `huginn/README.md`: what Huginn is, what's trusted, what it doesn't prove.
2. `huginn/docs/codebase.md` sections 1 and 2: the one-picture diagram and the reading order.
3. `huginn-evm/README.md`: the EVM side, the TUI, the "Trust" and "Using it" sections.
4. `huginn/docs/adversarial-report.md`: how an outsider tried to break it, and the status table at the end.
5. `huginn/docs/findings.md`, only these sections: "Certificates from shreds", "v1 transactions are live", "Trust assumptions".

**You should be able to answer, without looking:**
- What two things does a proof show about a Solana transaction? (Finality and inclusion.)
- Where does the certificate for block S come from? (Block S+1's footer, inside its shreds.)
- Why does the EVM contract need the validator set hash, and who posts it today?
- What does Huginn *not* prove? (That the transaction succeeded, or any program state.)
- Why is a practice run not a proof?

**Journal:** draw the pipeline by hand: Solana validator → shreds → ledger-dump → SP1 program → proof → SolanaLightClient → SolanaGreeter. Under each box, write one sentence: what goes in, what comes out, what's checked.

---

## Phase 2: the verifier core, `alpenglow-core` (two sessions)

This is the heart of Huginn: everything the proof guarantees is checked here. It's plain Rust, about 1,500 lines, and the same code runs inside SP1.

### Session 2a: finality and the block_id

**Read:**
1. `crates/alpenglow-core/src/finality.rs` (167 lines): checking one certificate.
2. `crates/alpenglow-core/src/shred.rs` (342 lines): shreds, their Merkle proofs, and rebuilding the block_id.

**Questions:**
- What exactly do the validators sign? Find the vote payload bytes.
- What are the fast path (80%) and slow path (60%), and why does the slow path need two certificates?
- How does the bitmap say who signed? What is a rank?
- How does a data shred prove it belongs to an FEC set? How do the FEC set roots become the block_id?

**Do:** run the tests for these parts and read their names. Each one is an attack that's refused:

```bash
cd huginn && cargo test --release -p alpenglow-core --test proof -- --list
```

**Break it** (undo each change with `git checkout <file>` afterwards):
- In `finality.rs`, `threshold()` (line ~80): change the fast path's `(80, 100)` to `(70, 100)`, then run `cargo test --release -p alpenglow-core`. Which test fails, and what would a 70% threshold let through?
- In `proof.rs`, `TxInclusion::verify`: delete the `return Err(Error::NotAtBoundary)` for a missing boundary shred, then run the tests again. Which attack does `component_boundary_is_enforced` describe?

### Session 2b: transactions and the statements we prove

**Read:**
1. `crates/alpenglow-core/src/block.rs` (316 lines): components, entries, the footer, `find_in_component`.
2. `crates/alpenglow-core/src/tx.rs` (326 lines): legacy, v0 and v1 transactions.
3. `crates/alpenglow-core/src/proof.rs` (391 lines): `FinalityInput`, `TxInclusion`, `TxInput`, `TxOutput::encode`.
4. `zk/program/src/main.rs` (26 lines): all of the above, run in SP1.

**Questions:**
- Why does inclusion need the "boundary shred" before the run? (Without it, the transaction could be read "out of frame".)
- Why is it safe to stop parsing at the target transaction? (Read the comment on `find_in_component`.)
- In a v1 transaction, where are the signatures, and which bytes do they sign?
- What are the public values, byte by byte? This layout is what the Solidity side decodes.

**Do:**
- Read the reviewer's tests and their comments: `crates/alpenglow-core/tests/adversarial.rs`.
- Run the Agave comparison: Huginn's parser against Agave's on 300,000 random transactions.

```bash
cargo test --release -p alpenglow-lc --test tx_matches_agave
```

**Break it:** in `tx.rs`, delete the "duplicate addresses" check in `sanity_check_v1`, then run the test above. The comparison catches the disagreement and prints the exact transaction. This is how you'll catch it if Solana changes the format again.

**Journal:** write out `TxOutput::encode`'s layout as a table from memory, then check it against `huginn-evm/src/HuginnOutput.sol`. They must match byte for byte.

---

## Phase 3: run the Solana side (one session)

### 3a. Offline first: no validator needed

`fixtures/local/shreds/` holds the saved shreds of two blocks from a real local run. Slot 133389 contains the memo "huginn:31337:greeter:offline from shred files". The memo's format is older than today's; that doesn't matter here.

```bash
cd huginn && cargo build --release -p ledger-dump
```

```bash
./target/release/ledger-dump --shreds fixtures/local/shreds --slot 133389 --out /tmp/huginn-study
```

**Look for:** the certificate for 133389 is read from block 133390's footer and checks out against the validator set, and the rebuilt block_id equals the certified one.

Now run the SP1 program on it without proving (seconds). `SP1_PROVER=mock` executes every check and counts cycles but makes no proof:

```bash
cd zk/prover && SP1_PROVER=mock cargo run --release -- --block /tmp/huginn-study/block_133389.json --signature 4BxoTDeV7tEiuiYp5AUbP8WJ2FAw7xd3bqSbWELKLsHPNcpw6nUnJuJ3jpA2fTnT41pCUxogGRVf6GupJNELy63Q
```

The first build compiles the SP1 program and takes a while.

**Look for:** the cycle counts (finality vs inclusion), and that the zkVM's output equals native Rust's.

Then see how the cost grows with the validator set:

```bash
SP1_PROVER=mock cargo run --release -- --synthetic 100,1000 --slow 1000
```

### 3b. A real STARK proof (~5 minutes, laptop fans)

```bash
cargo run --release -- --prove compressed
```

This proves the default fixture (slot 46718) for real, and writes a summary to `fixtures/proofs/`. Note the program key it prints. It will differ from older notes, because the code changed (v1 parsing).

### 3c. Live: your own validator

Follow [`run.md`](run.md) part 1, steps 2 and 3. Then send a memo yourself:

```bash
solana transfer 7eEm8iDQomGBvWzz159ATKbvG3aZfaBfTz6LvnDERmRR 0.01 --with-memo "hello from my study" --allow-unfunded-recipient --url http://127.0.0.1:8899
```

Wait about 15 seconds for finality, then dump its block. Use the slot from `solana confirm -v <signature>`:

```bash
cd huginn && ./target/release/ledger-dump --slot <slot> --out /tmp/huginn-study
```

**Journal:** for your own transaction, write down the slot, its parent, how many shreds and FEC sets the block had, and how many shreds the inclusion proof needed.

---

## Phase 4: the EVM contracts (one session)

**Read**, in this order (the same order as `huginn-evm/docs/codebase.md` section 2):
1. `src/HuginnOutput.sol` (60 lines): decoding the public values.
2. `src/SolanaLightClient.sol` (209 lines): `verify`, `submit`, delayed validator sets, two-step ownership.
3. `src/SolanaTx.sol` (255 lines): the Solidity twin of `tx.rs`, v1 included.
4. `src/SolanaControlled.sol` (144 lines) and `src/examples/SolanaGreeter.sol` (20 lines): a contract a Solana key controls.

**Questions:**
- In `verify`, list every check in order. What is each one protecting against?
- What stops a memo for one greeter from running on another greeter? On another chain? Running twice? Running out of order?
- How does a controller cancel a command they signed but nobody relayed?
- What can the owner still do that nobody else can, and how is it slowed down?
- Why does a *failed* Solana transaction still run its command? When does that matter?

**Do:**

```bash
cd huginn-evm && forge test -vv
```

Then read `test/Adversarial.t.sol`. `test_FIXED_*` are attacks that now fail; `test_POC_*` is the one still open.

**Break it:**
- In `SolanaControlled.sol`, change `if (n != nonce + 1)` to `if (n <= nonce)`, then run `forge test`. Which test catches the reordering attack?
- In `SolanaLightClient.sol`, remove the `ConflictingBlock` check from `verify`. Which test fails?

**Journal:** in a few sentences, explain the trust model as if to a developer who wants to build on it: what's proven, what's trusted, and what they must check themselves.

---

## Phase 5: run both sides together (one session)

Follow [`run.md`](run.md) part 1, steps 4 to 6: the validator running, then `doctor`, then the TUI.

```bash
cd huginn-evm && ./app/target/release/huginn-evm run doctor
```

```bash
./app/target/release/huginn-evm
```

1. **A practice run first** (about 10 seconds). Watch the five stages. Then press `d` and go through all 12 steps: each shows the exact command it ran. `docs/demo.md` says what each step should show.
2. **A real STARK run:** press `m` for "real (STARK)", then `enter`. About 5 minutes. Notice the result card says the proof was checked on your machine, not by the contract.
3. **Step by step, without the TUI.** Each step saves its state to `proofs/state.json`, so you can run them one at a time and look at what changed in between:

```bash
./app/target/release/huginn-evm run doctor anvil deploy greeter
```

```bash
./app/target/release/huginn-evm run send finalize fetch
```

```bash
./app/target/release/huginn-evm run prove submit check command forged
```

4. **Ask the contracts yourself** with `cast`. Take the addresses from `proofs/state.json`:

```bash
cast call <greeter> "greeting()(string)" --rpc-url http://127.0.0.1:8545
```

```bash
cast call <greeter> "nonce()(uint64)" --rpc-url http://127.0.0.1:8545
```

```bash
cast call <client> "validatorSetHash(uint64)(bytes32)" 0 --rpc-url http://127.0.0.1:8545
```

**Break it, live:**
- Run `send` again, but skip `prove` and run `command` with the old proof. The greeter refuses with `WrongNonce`, because that proof's memo used the previous nonce.
- Deploy a second greeter (`run greeter`), then run `command` with a proof made for the first one. It's refused with `NoCommand`.

**Journal:** for one full run, record the time each stage took, the cycles, the submit gas, and the program key. That's your baseline.

---

## Phase 6: explain it (one session, for the video)

You understand it when you can explain each item without notes. Use this as the video outline:

1. **The problem:** another chain wants to know something happened on Solana, without trusting a bridge.
2. **Finality:** Alpenglow certificates, BLS aggregate signatures, 80% / 60% thresholds, the validator set.
3. **Inclusion:** shreds, FEC sets, Merkle roots, the block_id. Why shreds alone are enough (the footer trick).
4. **SP1:** why prove it at all (one small proof instead of re-checking everything on the EVM), and STARK vs Groth16.
5. **The EVM side:** `verify`, `SolanaTx`, and a Solana wallet controlling a contract (memo, address, nonce).
6. **The demo:** the TUI run, then the forged memo being refused.
7. **Honest limits:** the owner posts validator sets (delayed and public, but still trusted), inclusion isn't success, Groth16 needs ≥32 GB of RAM.

If a point is hard to explain, go back to its phase.

---

## Phase 7: what's next (when you're ready)

These are the open items, roughly in order:
1. **A Groth16 proof on a VPS:** [`run.md`](run.md) part 2. Then `forge test --match-contract Groth16` checks it with SP1's real verifier and prints the real gas cost.
2. **Base Sepolia:** deploy against Succinct's verifier there, propose validator sets with `script/ProposeValidatorSet.s.sol`, submit the Groth16 proof.
3. **Multi-validator sets:** real certificates so far have one signer.
4. **Proving the validator set handover in SP1:** this removes the owner from the trust model (review finding 2's root cause).
5. **Proving success, not just inclusion:** Layer 2 in `PLAN.md` (review finding 3).

---

## Quick reference

| I want to… | Command (from) |
|---|---|
| run all huginn tests | `cargo test --release` (huginn) |
| run all contract tests | `forge test -vv` (huginn-evm) |
| check my setup | `./app/target/release/huginn-evm run doctor` (huginn-evm) |
| open the TUI | `./app/target/release/huginn-evm` (huginn-evm) |
| dump a block | `./target/release/ledger-dump --slot <slot>` (huginn) |
| run the SP1 program without proving | `SP1_PROVER=mock cargo run --release` (huginn/zk/prover) |
| make a real STARK proof | `cargo run --release -- --prove compressed` (huginn/zk/prover) |
| see the program key | `./app/target/release/huginn-evm vkey` (huginn-evm) |
| stop everything | `pkill -f solana-test-validator`, `pkill anvil` |
