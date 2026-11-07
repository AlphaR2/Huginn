Raw data shreds of two consecutive **real devnet** slots, fetched on 2026-11-07 from a public
validator with `shred-repair` (Agave's repair protocol), no ledger and no paid feed:

- `shreds_519635459.json`: 192 data shreds, 6 FEC sets. Its certificate (fast path, 17 of the
  21 ranked validators signing) is in the footer of the next block.
- `shreds_519635460.json`: its child. Its footer carries the certificate for 519635459.

`../validator_set_1202.json` is devnet epoch 1202's ranked BLS set, fetched the same day. The
parent's own certificate is *not* in 519635459's footer (it carries the one for 519635457), so
the parent's block id comes from the block's header.

`ledger-dump --shreds fixtures/devnet/shreds --sets fixtures/devnet --slot 519635459`
rebuilds and checks the block (the test `devnet_block.rs` does the same).
