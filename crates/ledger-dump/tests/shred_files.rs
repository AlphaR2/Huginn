//! Items 6.1/6.3: a fixture built from shred files alone (no ledger, no plugin, no RPC),
//! and the transaction in it proven final and included end to end (natively).

use {
    alpenglow_core::fixture,
    ledger_dump::{build_fixture, newest_certified, Certs, ShredFiles, ShredSource, Sets},
    std::path::PathBuf,
};

const SLOT: u64 = 133389;
const SIG: &str = "4BxoTDeV7tEiuiYp5AUbP8WJ2FAw7xd3bqSbWELKLsHPNcpw6nUnJuJ3jpA2fTnT41pCUxogGRVf6GupJNELy63Q";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn files() -> ShredFiles {
    ShredFiles::new(&root().join("fixtures/local/shreds"))
}

fn sets() -> Sets {
    Sets { dir: root().join("fixtures/local"), slots_per_epoch: 432_000 }
}

fn b58(s: &str) -> [u8; 64] {
    // Base58 decode without a dependency: big-number in base 58.
    const A: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let mut out = vec![0u8; 64];
    for c in s.bytes() {
        let mut carry = A.iter().position(|&a| a == c).unwrap() as u32;
        for b in out.iter_mut().rev() {
            carry += *b as u32 * 58;
            *b = carry as u8;
            carry >>= 8;
        }
    }
    out.try_into().unwrap()
}

#[test]
fn files_cover_the_block_and_its_child() {
    assert_eq!(files().range().unwrap(), (SLOT, SLOT + 1));
    // The newest slot a certificate exists for: SLOT (its cert is in SLOT + 1's footer).
    assert_eq!(newest_certified(&files(), &Certs::Shreds).unwrap(), SLOT);
}

#[test]
fn fixture_from_shred_files_proves_the_memo_final_and_included() {
    let built = build_fixture(&files(), SLOT, &sets(), &Certs::Shreds).unwrap();
    assert_eq!(built.fixture["cert_source"], "shreds");
    assert_eq!(built.fixture["parent_slot"], SLOT - 1);

    // The whole statement the SP1 program proves, run natively on this fixture.
    let input = fixture::tx_input(&built.fixture, b58(SIG));
    let out = input.verify().unwrap();
    assert_eq!(out.finality.slot, SLOT);
    assert_eq!(out.finality.signed_stake, out.finality.total_stake, "one validator, all stake");
    let memo = b"huginn:31337:greeter:offline from shred files";
    assert!(out.tx.raw.windows(memo.len()).any(|w| w == memo), "the memo is in the proven transaction");
}

#[test]
fn the_parent_alone_is_not_enough() {
    // SLOT + 1 has no child in the files, so its certificate can't be found.
    let err = build_fixture(&files(), SLOT + 1, &sets(), &Certs::Shreds).err().unwrap();
    assert!(format!("{err:#}").contains("no child block"), "{err:#}");
}
