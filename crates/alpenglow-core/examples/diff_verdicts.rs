//! Reference verdicts for the Solidity differential test (observer work order W9).
//!
//! Reads certificate cases in the EIP-2537 encoding used on EVM chains (G1 = 128 bytes,
//! G2 = 256 bytes), checks each one with this crate's `verify_cert`, and writes the cases back
//! with `"rust_accepts": true|false` and the error. The Solidity verifier must agree on every
//! case.
//!
//! cargo run --offline -p alpenglow-core --features fixture --example diff_verdicts -- IN OUT

use {
    alpenglow_core::finality::{verify_cert, CertType},
    bls12_381::{G1Affine, G2Affine},
    serde_json::Value,
};

fn hex_bytes(v: &Value) -> Vec<u8> {
    hex::decode(v.as_str().unwrap().trim_start_matches("0x")).unwrap()
}

/// EIP-2537 G1 (16 zero bytes | x 48 | 16 zero bytes | y 48) -> bls12_381 uncompressed (x | y).
fn g1(eip: &[u8]) -> Option<G1Affine> {
    if eip.len() != 128 || eip[..16].iter().chain(&eip[64..80]).any(|&b| b != 0) {
        return None;
    }
    let mut raw = [0u8; 96];
    raw[..48].copy_from_slice(&eip[16..64]);
    raw[48..].copy_from_slice(&eip[80..128]);
    if raw.iter().all(|&b| b == 0) {
        raw[0] = 0x40; // point at infinity
    }
    Option::from(G1Affine::from_uncompressed(&raw))
}

/// EIP-2537 G2 (x.c0 | x.c1 | y.c0 | y.c1, 64 each) -> bls12_381 uncompressed (x.c1 | x.c0 | y.c1 | y.c0).
fn g2(eip: &[u8]) -> Option<G2Affine> {
    if eip.len() != 256 {
        return None;
    }
    let fp = |i: usize| -> Option<&[u8]> {
        let w = &eip[i * 64..(i + 1) * 64];
        w[..16].iter().all(|&b| b == 0).then(|| &w[16..])
    };
    let (x0, x1, y0, y1) = (fp(0)?, fp(1)?, fp(2)?, fp(3)?);
    let mut raw = [0u8; 192];
    raw[..48].copy_from_slice(x1);
    raw[48..96].copy_from_slice(x0);
    raw[96..144].copy_from_slice(y1);
    raw[144..].copy_from_slice(y0);
    if raw.iter().all(|&b| b == 0) {
        raw[0] = 0x40;
    }
    Option::from(G2Affine::from_uncompressed(&raw))
}

fn verdict(c: &Value) -> Result<u64, String> {
    let slot = c["slot"].as_u64().unwrap();
    let block_id: [u8; 32] = hex_bytes(&c["block_id"]).try_into().unwrap();
    let cert = match c["cert_type"].as_str().unwrap() {
        "notarize" => CertType::Notarize { slot, block_id },
        "finalize_fast" => CertType::FinalizeFast { slot, block_id },
        "finalize" => CertType::Finalize { slot },
        t => return Err(format!("unknown cert type {t}")),
    };
    let pks = hex_bytes(&c["pubkeys"]);
    let stakes: Vec<u64> = c["stakes"].as_array().unwrap().iter().map(|s| s.as_u64().unwrap()).collect();
    if pks.len() != stakes.len() * 128 {
        return Err("set size".into());
    }
    let keys: Vec<G1Affine> = pks
        .chunks(128)
        .map(|k| g1(k).ok_or("bad public key"))
        .collect::<Result<_, _>>()?;
    let sig = g2(&hex_bytes(&c["signature"])).ok_or("bad signature encoding")?;
    verify_cert(
        &cert,
        c["shred_version"].as_u64().unwrap() as u16,
        &sig,
        &hex_bytes(&c["bitmap"]),
        stakes.len(),
        c["total_stake"].as_u64().unwrap(),
        |r| Some((*stakes.get(r)?, *keys.get(r)?)),
    )
    .map_err(|e| format!("{e:?}"))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut cases: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(&args[1]).unwrap()).unwrap();
    for c in &mut cases {
        let v = verdict(c);
        println!("{:40} {}", c["name"].as_str().unwrap(), match &v {
            Ok(s) => format!("ACCEPT (signed stake {s})"),
            Err(e) => format!("reject: {e}"),
        });
        c["rust_accepts"] = v.is_ok().into();
        c["rust_error"] = v.err().into();
    }
    std::fs::write(&args[2], serde_json::to_string_pretty(&cases).unwrap() + "\n").unwrap();
}
