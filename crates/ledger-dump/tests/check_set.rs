//! `check-set` on real devnet epoch-1206 data (5 blocks' footers, `fixtures/devnet-1206`).
//! The 20-validator set verifies every certificate; the wrong 23-validator set verifies none.
//! (Over all 13 blocks fetched, the same sets give 13/13 and 3/13; run
//! `ledger-dump --shreds <dir> --sets <set> check-set` on a full directory to see it.)

use {
    ledger_dump::{check_set, ShredFiles, ShredSource, Sets},
    std::{fs, path::PathBuf},
};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/devnet-1206")
}

fn run(set_dir: &str, min: usize) -> serde_json::Value {
    let src = ShredFiles::new(&root().join("shreds"));
    let (a, b) = src.range().unwrap();
    let slots: Vec<u64> = (a..=b).collect();
    let sets = Sets { dir: root().join(set_dir), slots_per_epoch: 432_000 };
    check_set(&src, &slots, &sets, min).unwrap()
}

#[test]
fn correct_set_passes_and_is_not_modified() {
    let path = root().join("set-flagged/validator_set_1206.json");
    let before = fs::read(&path).unwrap();
    let r = run("set-flagged", 5);
    assert_eq!(r["pass"], true, "{r}");
    assert_eq!(r["validators"], 20);
    assert_eq!(r["certificates"]["verified"], 5);
    assert_eq!(r["set_was_modified"], false);
    assert_eq!(fs::read(&path).unwrap(), before, "check-set must never rewrite the set");
}

#[test]
fn wrong_set_fails() {
    let r = run("set-orig", 1);
    assert_eq!(r["pass"], false, "{r}");
    assert_eq!(r["validators"], 23);
    assert_eq!(r["certificates"]["failed"], 5);
}

#[test]
fn too_few_certificates_fails_closed() {
    let r = run("set-flagged", 6);
    assert_eq!(r["pass"], false, "needs at least 6 certificates but only 5 exist");
}
