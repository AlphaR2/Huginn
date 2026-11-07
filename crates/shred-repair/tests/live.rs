//! Needs a running local validator (RPC on 127.0.0.1:8899, repair on its `serveRepair`
//! address). Run with: cargo test --release -p shred-repair -- --ignored --nocapture

use shred_repair::{client::Client, fetch_slot};

fn rpc(method: &str, params: serde_json::Value) -> serde_json::Value {
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
    ureq::post("http://127.0.0.1:8899")
        .send_json(body)
        .unwrap()
        .into_json::<serde_json::Value>()
        .unwrap()["result"]
        .clone()
}

#[test]
#[ignore = "needs a running local validator"]
fn a_recent_block_comes_back_complete_and_well_formed() {
    let node = rpc("getClusterNodes", serde_json::json!([]))[0].clone();
    let addr = node["serveRepair"].as_str().unwrap().parse().unwrap();
    let id: [u8; 32] = bs58::decode(node["pubkey"].as_str().unwrap())
        .into_vec()
        .unwrap()
        .try_into()
        .unwrap();
    let slot = rpc("getSlot", serde_json::json!([{"commitment":"finalized"}]))
        .as_u64()
        .unwrap()
        - 20;
    let mut c = Client::new(addr, id).unwrap();
    // fetch_slot already checks the shreds form a complete block with chained set roots.
    let shreds = fetch_slot(&mut c, slot).unwrap();
    assert!(
        shreds.len() >= 32 && shreds.len().is_multiple_of(32),
        "whole FEC sets of 32 data shreds"
    );
}
