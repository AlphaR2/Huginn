# A real devnet transaction, proven (2026-11-07)

The first proof of a transaction on a live network, using no local ledger, no paid feed and no node.

| | |
|---|---|
| Cluster | Solana devnet (Agave 4.4.0, shred version 7016) |
| Transaction | `3ikzDj21Hy2EoqeFxFCb4bSkcqaSBGhGZ7LQZFr6dgHzuPMAQLphzBX9oYJpbPAkeQ6RhJ9Qez8hXEAdCR9BBJE1` (a **v1** transaction, succeeded on devnet) |
| Slot / block | 519,635,459 (epoch 1202), `block_id` `2c20fcd9cc8a3e1c0889215cb41784ddf89848b91ac59f6b4e33dd52cd529cca` |
| Where the shreds came from | A public devnet validator, via `shred-repair` (Agave's repair protocol) |
| Certificate | Fast path, 17 of the 21 ranked validators signing (from the next block's footer) |
| Validator set | Epoch 1202, 22 ranked, commitment `2a67bb51366433e04d3ee35b5804bd5e45e67979df16b1b8c5c7a0f03da787b2` |
| SP1 execution | 5,887,484 cycles: 4,578,506 for the certificate, 643,135 for inclusion |
| STARK proof (compressed) | setup 1.5 s, prove 311.5 s, verify 0.04 s, 1.27 MB; checked on the proving machine only |
| Program key | `0x00d62acf116c0769693a77ed9e2ad7a53d49aca726c36a5b2652e928ea9025e0` |

## What the proof's public values contain (checked by decoding them)
- the validator set commitment above (equals what `fetch validator-set --cluster devnet` produced from public RPC),
- the shred version 7016, the slot, the `block_id`,
- the transaction's bytes, including its signature.

So anyone can recompute the set commitment from devnet RPC and compare, and look the signature up on a devnet explorer. A wrong signature is refused (`TxNotFound`).

## Reproduce
```bash
cargo run --release -p shred-repair -- --rpc https://api.devnet.solana.com fetch --slot 519635459 --out DIR
cargo run --release -p fetch -- --out FX validator-set --cluster devnet     # during epoch 1202
cargo run --release -p ledger-dump -- --shreds DIR --slot 519635459 --sets FX/devnet --out OUT
cd zk/prover && cargo run --release -- --block OUT/block_519635459.json \
  --signature 3ikzDj21Hy2EoqeFxFCb4bSkcqaSBGhGZ7LQZFr6dgHzuPMAQLphzBX9oYJpbPAkeQ6RhJ9Qez8hXEAdCR9BBJE1 --prove compressed
```
The block and set used are kept in `fixtures/devnet/` (`shreds/`, `validator_set_1202.json`).

## Not shown
- A Groth16 proof or an on-chain check (needs a 32 GB machine and a destination chain).
- That a contract accepted it: the destination chain must hold this epoch's set hash and use the program key above.

## A note on the program key
The key is a hash of the exact compiled program, so even reformatting the Rust source changes it (panic messages embed line numbers). This run's key differs from the one in `huginn-evm/.env` for that reason. Before a real deployment, build the program reproducibly (SP1's Docker build) and pin that key.
