//! Item 3.3 on real data: block 46718's data shreds -> components -> entries -> the memo
//! transfer we sent, byte-identical to what RPC returns for that transaction.

use {
    alpenglow_core::{
        block::{self, Component, Marker},
        shred::Shred,
    },
    serde_json::Value,
};

fn json(file: &str) -> Value {
    let p = format!("{}/../../fixtures/local/{file}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn hex(v: &Value) -> Vec<u8> {
    (0..v.as_str().unwrap().len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&v.as_str().unwrap()[i..i + 2], 16).unwrap())
        .collect()
}

fn block_shreds() -> (Value, Vec<Vec<u8>>) {
    let b = json("block_46718.json");
    let shreds = b["data_shreds"].as_array().unwrap().iter().map(hex).collect();
    (b, shreds)
}

#[test]
fn block_parses_into_header_entries_footer() {
    let (b, raw) = block_shreds();
    let shreds: Vec<Shred> = raw.iter().map(|s| Shred::parse(s).unwrap()).collect();
    let bytes = block::deshred(&shreds).unwrap();
    let comps: Vec<Component> = bytes.iter().map(|c| block::parse_component(c).unwrap()).collect();

    // First component: the header, naming the parent that the block_id's last leaf binds.
    let parent_id: [u8; 32] = hex(&b["parent_block_final_cert"]["block_id"]).try_into().unwrap();
    assert_eq!(comps[0], Component::Marker(Marker::Header { parent_slot: 46717, parent_block_id: parent_id }));
    // Observed layout: header, entry batches (our tx), footer, then one last tick entry.
    assert_eq!(comps.len(), 4);
    assert!(matches!(&comps[1], Component::Entries(e) if e.iter().map(|x| x.transactions.len()).sum::<usize>() == 1));
    assert!(matches!(comps[2], Component::Marker(Marker::Footer(_))));
    assert!(matches!(&comps[3], Component::Entries(e) if e.iter().all(|x| x.transactions.is_empty())));
    // Each component is padded to whole FEC sets: they end on shreds 31, 63, 95, 127.
    let ends: Vec<u32> = shreds.iter().filter(|s| s.data_complete()).map(|s| s.index()).collect();
    assert_eq!(ends, [31, 63, 95, 127]);
}

#[test]
fn finds_the_memo_transfer_byte_for_byte() {
    let (_, raw) = block_shreds();
    let shreds: Vec<Shred> = raw.iter().map(|s| Shred::parse(s).unwrap()).collect();
    let bytes = block::deshred(&shreds).unwrap();
    let comps: Vec<Component> = bytes.iter().map(|c| block::parse_component(c).unwrap()).collect();

    let expected = json("tx_46718.json");
    let sig: [u8; 64] = hex(&expected["signature"]).try_into().unwrap();
    let (pos, tx) = block::find_transaction(&comps, &sig).expect("tx in block");
    assert_eq!(tx.raw, hex(&expected["raw"]).as_slice(), "found at {pos:?}");

    // What a destination chain cares about: who signed, which programs, what data.
    assert_eq!(tx.version, None); // legacy
    assert_eq!(tx.signers().len(), 1);
    let programs: Vec<[u8; 32]> = tx.instructions.iter().map(|ix| *tx.program_id(ix).unwrap()).collect();
    let system = [0u8; 32];
    assert_eq!(programs[0], system);
    let memo_ix = tx.instructions.iter().find(|ix| tx.program_id(ix) != Some(&system)).unwrap();
    assert_eq!(memo_ix.data, b"huginn: hello from solana");
}

#[test]
fn every_component_of_every_fixture_block_parses() {
    for f in ["block_44000.json", "block_46718.json"] {
        let b = json(f);
        let raw: Vec<Vec<u8>> = b["data_shreds"].as_array().unwrap().iter().map(hex).collect();
        let shreds: Vec<Shred> = raw.iter().map(|s| Shred::parse(s).unwrap()).collect();
        for c in block::deshred(&shreds).unwrap() {
            block::parse_component(&c).unwrap_or_else(|e| panic!("{f}: {e}"));
        }
    }
}

#[test]
#[ignore]
fn print_component_layout() {
    for f in ["block_44000.json", "block_46718.json"] {
        let (_, raw) = (0, json(f)["data_shreds"].as_array().unwrap().iter().map(hex).collect::<Vec<_>>());
        let shreds: Vec<Shred> = raw.iter().map(|s| Shred::parse(s).unwrap()).collect();
        let summary: Vec<String> = block::deshred(&shreds)
            .unwrap()
            .iter()
            .map(|c| match block::parse_component(c).unwrap() {
                Component::Marker(m) => format!("{:?}", m).chars().take(14).collect(),
                Component::Entries(e) => format!("E{}/{}tx", e.len(), e.iter().map(|x| x.transactions.len()).sum::<usize>()),
            })
            .collect();
        let ends: Vec<u32> = shreds.iter().filter(|s| s.data_complete()).map(|s| s.index()).collect();
        println!("{f}: {} components {:?}\n  ends at shreds {:?}", summary.len(), summary, ends);
    }
}
