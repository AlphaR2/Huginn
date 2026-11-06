# Security

**Status: not production-ready. Do not put value behind it.** It has had two outside adversarial reviews, and every finding is fixed, mitigated or documented below. Real Groth16 verification has not yet been exercised on a real chain.

## Threat model

| Who | Can they forge a proof? |
|---|---|
| Anyone with shreds and certificates | No. The certificate pins `block_id`; Huginn rebuilds it. |
| A relayer | No. They can delay or reorder relays, not change what is proven. `SolanaControlled`'s nonce fixes the order. |
| Less than 20% malicious Solana stake | No (Alpenglow's assumption). |
| The light client's owner | **Yes, after the public delay**, by proposing a validator set whose keys they hold. This is the remaining root of trust. |

## Known limits

- **Owner trust.** Mitigated, not removed: sets count only after `setDelay`, are visible (`pendingSet`, `ValidatorSetProposed`), can be cancelled or replaced, and ownership moves in two steps. Real fix: prove the epoch-to-epoch handover in SP1. Until then use a multisig and watch the events.
- **Inclusion is not success.** Failed transactions are in blocks too.
- **Cancel is a race.** A signed command is public once final; anyone can relay it before a cancel's proof exists. Make revocable commands conditional inside the contract.
- **A failing command does not stall the queue.** It logs `CommandFailed` and spends its nonce; sign it again to retry. A gas guard makes a relayer who starves a command revert the whole call instead of spending its nonce.
- **Transaction formats.** Legacy, v0 and v1 are parsed and match Agave on millions of random inputs. A future format placed before a target would hide it until the parser is updated.
- **Untested at scale.** Real certificates so far have one signer; multi-signer sets are tested with synthetic data and one devnet genesis certificate.
- **Rogue-key attack.** Huginn relies on Solana's vote program having checked proof of possession for every BLS key.

## Reviews

| Round | Findings | Outcome |
|---|---|---|
| 1 | 8 (2 high, 2 medium, 4 lower) plus usability | All fixed, mitigated or documented. Nonce-bound commands, delayed validator sets, v1 transactions, key handling, conflict check in `verify`. |
| 2 | 3 (aimed at the round-1 fixes) | Failed commands no longer stall the queue; conflict records are kept per validator set so a bad-set episode can't poison real proofs; the cancel race is documented and pinned by a test. |

Full reports: [`research/adversarial-report.md`](research/adversarial-report.md). The attacks are kept as tests: `crates/alpenglow-core/tests/adversarial.rs`, and `test/Adversarial*.t.sol` in huginn-evm (`test_FIXED_*` was an attack that now fails, `test_HELD_*` always failed, `test_LIMIT_*` is a documented limit, `test_POC_*` is an open weakness).

## Reporting a problem

Open a private security advisory on the GitHub repository, or contact the maintainers directly. Please don't file public issues for exploitable bugs.
