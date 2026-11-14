//! A real devnet block, fetched from a public validator with the repair protocol: its
//! multi-validator fast-path certificate verifies and the shreds rebuild the certified
//! block_id. Unlike the local chain, the parent's own certificate is not in the footer.

use {
    ledger_dump::{build_fixture, Certs, Sets, ShredFiles},
    std::path::PathBuf,
};

const SLOT: u64 = 519_635_459;
const BLOCK_ID: &str = "2c20fcd9cc8a3e1c0889215cb41784ddf89848b91ac59f6b4e33dd52cd529cca";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn sets() -> Sets {
    Sets {
        dir: root().join("fixtures/devnet"),
        slots_per_epoch: 432_000,
    }
}

fn files(dir: &str) -> ShredFiles {
    ShredFiles::new(&root().join(dir))
}

#[test]
fn real_devnet_block_rebuilds_its_certified_block_id() {
    let built = build_fixture(
        &files("fixtures/devnet/shreds"),
        SLOT,
        &sets(),
        &Certs::Shreds,
    )
    .expect("devnet block checks out");
    assert_eq!(built.fixture["block_final_cert"]["block_id"], BLOCK_ID);
    assert_eq!(built.fixture["shred_version"], 7016);
    let bh = built.fixture["footer_bank_hash"].as_str().expect("footer bank hash recorded");
    assert_eq!(bh.len(), 64);
    assert_eq!(built.fixture["cluster"], "devnet");
    assert_eq!(built.fixture["epoch"], 1202);
    assert_eq!(built.fixture["data_shreds"].as_array().unwrap().len(), 192);
    // No parent certificate was available, so none is stored, but the id is.
    assert!(built.fixture["parent_block_final_cert"].is_null());
    assert_eq!(
        built.fixture["parent_block_id"],
        "fde16ca77e20fc0354be72cbfecda7e7914fb8a68a45ca806ba52c880abdb046"
    );
}

#[test]
fn a_flipped_byte_in_any_devnet_shred_breaks_it() {
    let dir = std::env::temp_dir().join(format!("huginn-devnet-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for slot in [SLOT, SLOT + 1] {
        std::fs::copy(
            root().join(format!("fixtures/devnet/shreds/shreds_{slot}.json")),
            dir.join(format!("shreds_{slot}.json")),
        )
        .unwrap();
    }
    let path = dir.join(format!("shreds_{SLOT}.json"));
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    // Corrupt the payload of the 100th data shred (first hex digit after the headers).
    let hexs = v["data_shreds"][100].as_str().unwrap().to_string();
    let at = 2 * 200;
    let flipped = if &hexs[at..at + 1] == "0" { "1" } else { "0" };
    v["data_shreds"][100] =
        serde_json::Value::String(format!("{}{}{}", &hexs[..at], flipped, &hexs[at + 1..]));
    std::fs::write(&path, serde_json::to_string(&v).unwrap()).unwrap();
    let err = build_fixture(&ShredFiles::new(&dir), SLOT, &sets(), &Certs::Shreds);
    assert!(err.is_err(), "a tampered shred must not produce a fixture");
    std::fs::remove_dir_all(&dir).ok();
}
