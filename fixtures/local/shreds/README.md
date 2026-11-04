Raw shreds of two consecutive local slots, written by
`ledger-dump --export-shreds 133389..133390 --out fixtures/local/shreds`.

- `shreds_133389.json`: a block holding the memo transfer
  `4BxoTDeV7tEiuiYp5AUbP8WJ2FAw7xd3bqSbWELKLsHPNcpw6nUnJuJ3jpA2fTnT41pCUxogGRVf6GupJNELy63Q`
  ("huginn:31337:greeter:offline from shred files"); its footer also carries the
  certificate for its parent, 133388.
- `shreds_133390.json`: its child, whose footer carries the certificate for 133389.

These two files alone prove the transaction final and included: no ledger, no plugin, no RPC.
`ledger-dump --shreds fixtures/local/shreds --slot 133389` rebuilds the block fixture from them.
