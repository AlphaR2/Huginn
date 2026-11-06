# How Huginn works

Huginn lets a contract on another chain (Base first, any EVM chain the same way) check that a Solana transaction is in a block Solana's validators finalized. No bridge committee and no multisig vouching for it: the proof is checked against Solana's own validator signatures.

## The idea in four steps

1. **Finality.** Under Alpenglow, validators vote on each block. Their BLS signatures are combined into one *certificate* (60% of stake to notarize, 80% for the fast path). A block's certificate is stored in the *next* block's footer.
2. **Inclusion.** A block is sent as *shreds* (small packets). Every shred carries a Merkle path to its set's root, and the roots combine into the block's `block_id`. The certificate signs that `block_id`. So if we rebuild `block_id` from shreds and it matches, the shreds, and any transaction in them, are the certified block's.
3. **Proof.** Steps 1 and 2 run inside an SP1 zkVM program (`zk/program`, using `alpenglow-core`). It outputs a small proof plus fixed-layout public values: slot, `block_id`, the validator set it checked against, the shred version, and the transaction bytes.
4. **Verify.** A contract checks the proof (SP1's Groth16 verifier, about 270k gas) and the public values: the right program, the right Solana cluster, the validator set stored for that slot's epoch.

```
Solana node / saved shreds
        │ shreds of block N and N+1
        ▼
  ledger-dump / fetch ──► block + certificate + validator set
        │
        ▼
  SP1 program (alpenglow-core): check BLS certificate, rebuild block_id, find the transaction
        │ proof + public values
        ▼
  SolanaLightClient (EVM): verify proof, check program key, cluster, validator set
        │
        ▼
  your contract (e.g. SolanaControlled): act on the verified transaction
```

## Where shreds come from

Anywhere. The source is untrusted: the certificate pins `block_id`, and Huginn recomputes it, so a wrong or missing shred fails rather than fooling anyone. Options: a node's local ledger, exported shred files, or a raw-shred feed. A paid gRPC feed is not enough on its own (it gives footers but not shreds).

## What is trusted

- Solana consensus (at most 20% malicious stake, under Alpenglow's assumptions).
- **The validator set for each epoch**, posted by the light client's owner. Posted sets wait a public delay before they count, but the owner is still trusted until set handover is proven in SP1. Use a multisig owner.
- SP1's proof system and the pinned verifier contract.
- Solana's vote program, which checks BLS proof-of-possession when keys are registered. Huginn relies on that.

## What it does not prove

- **Success.** A failed Solana transaction is still in a block. The proof shows inclusion, not that it worked.
- **Program state** (for example "this account holds X"). Solana does not sign state. See Layer 2 in `research/plan.md`.

## The pieces

| Piece | Repo | Role |
|---|---|---|
| `alpenglow-core` | huginn | The verifier: certificate, block_id, shreds, transactions. Pure Rust, runs inside SP1. |
| `alpenglow-lc` | huginn | Cross-checks against Agave's own crates; validator set ranking. |
| `zk/program`, `zk/prover` | huginn | The SP1 program and the `huginn-prover` crate / CLI. |
| `fetch`, `ledger-dump`, `plugins/footer-dump` | huginn | Get validator sets, shreds, blocks, certificates. |
| `SolanaLightClient`, `SolanaTx` | huginn-evm | Check proofs; parse the proven transaction. |
| `SolanaControlled` | huginn-evm | Base class for a contract a Solana key controls through Memo transactions. |
| `app` (`huginn-evm` CLI/TUI) | huginn-evm | Runs the whole flow. |

Per-file detail: [`codebase.md`](codebase.md). Setup: [`run.md`](run.md).
