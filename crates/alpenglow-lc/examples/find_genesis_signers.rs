//! Phase 0 research tool: check a real genesis certificate's BLS signature when we can't
//! rebuild the exact historical validator ranking.
//!
//! The cert's bitmap says *how many* validators signed (k of n). Ranks have shifted since,
//! but the signers' keys are probably still registered. So we try every k-subset of today's
//! ranked keys and look for one whose aggregate key verifies the signature.
//! A hit proves our payload encoding, hash-to-curve, and signature parsing match Solana's.
//!
//! Usage:
//!   cargo run --release --example find_genesis_signers -- --self-test
//!   cargo run --release --example find_genesis_signers -- \
//!       fixtures/devnet/genesis_cert.json fixtures/devnet/validator_set_1198.json <shred_version>

use {
    agave_votor_messages::{consensus_message::Block, vote::Vote, wire::get_vote_payload_to_sign},
    alpenglow_lc::validator_set::ValidatorSet,
    anyhow::{bail, Context, Result},
    rayon::prelude::*,
    serde_json::Value,
    solana_bls_signatures::{
        keypair::Keypair,
        pubkey::{PopVerified, PubkeyAffine, PubkeyCompressed},
        PreparedHashedMessage, PubkeyProjective, Signature, SignatureProjective, VerifySignature,
    },
    solana_hash::Hash,
    std::time::Instant,
};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--self-test") {
        return self_test();
    }
    let [cert_path, set_path, shred_version] = args.as_slice() else {
        bail!("usage: <genesis_cert.json> <validator_set.json> <shred_version> | --self-test");
    };
    let shred_version: u16 = shred_version.parse()?;

    let cert: Value = serde_json::from_str(&std::fs::read_to_string(cert_path)?)?;
    let r = &cert["result"];
    let slot = r["block"]["slot"].as_u64().context("slot")?;
    let block_id: [u8; 32] = bytes(&r["block"]["blockId"])?
        .try_into()
        .ok()
        .context("blockId")?;
    let sig: [u8; 192] = bytes(&r["signature"]["signature"])?
        .try_into()
        .ok()
        .context("sig")?;
    let bitmap = bytes(&r["signature"]["bitmap"])?;

    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(set_path)?)?;
    let set: ValidatorSet = serde_json::from_value(fixture["validator_set"].clone())?;

    let (bits, ranks) = decode_base2(&bitmap)?;
    println!(
        "cert: slot {slot}, bitmap {bits} bits, {} signers",
        ranks.len()
    );
    println!(
        "today's set: epoch {}, {} ranked validators",
        set.epoch,
        set.entries.len()
    );

    let payload = genesis_payload(slot, block_id, shred_version);
    println!(
        "payload ({} bytes): {}",
        payload.len(),
        hex::encode(&payload)
    );

    let keys: Vec<_> = set
        .entries
        .iter()
        .map(|e| pop_verified(&e.bls_pubkey))
        .collect::<Result<_>>()?;
    let prepared = PreparedHashedMessage::new(&payload);
    let signature = Signature(sig);

    // Cheap first try: maybe the ranking hasn't changed at all.
    if bits == keys.len() && verifies(&keys, &ranks, &signature, &prepared) {
        println!("MATCH using today's ranks directly: the set is unchanged.");
        return Ok(());
    }

    match search(&keys, ranks.len(), &signature, &prepared) {
        Some(found) => {
            println!("MATCH: these {} validators signed the genesis cert:", found.len());
            for i in found {
                let e = &set.entries[i];
                println!(
                    "  today-rank {i:>3}  stake {:>20}  vote {}",
                    e.stake,
                    hex::encode(e.vote_pubkey)
                );
            }
        }
        None => println!(
            "NO MATCH. Either a signer's key changed or left, or shred_version {shred_version} is wrong."
        ),
    }
    Ok(())
}

/// The exact bytes validators signed: Agave's own encoder, not ours.
fn genesis_payload(slot: u64, block_id: [u8; 32], shred_version: u16) -> Vec<u8> {
    let block = Block {
        slot,
        block_id: Hash::new_from_array(block_id),
    };
    get_vote_payload_to_sign(Vote::new_genesis_vote(block), shred_version)
}

fn pop_verified(key: &[u8; 48]) -> Result<PopVerified<PubkeyAffine>> {
    let affine = PubkeyAffine::try_from(PubkeyCompressed(*key))
        .ok()
        .context("bad BLS key")?;
    // SAFETY (trust assumption): keys come from vote accounts, whose PoP the vote program
    // checked at registration. Same assumption Agave makes in epoch_stakes.rs.
    Ok(unsafe { PopVerified::new_unchecked(affine) })
}

fn verifies(
    keys: &[PopVerified<PubkeyAffine>],
    pick: &[usize],
    sig: &Signature,
    msg: &PreparedHashedMessage,
) -> bool {
    PubkeyProjective::aggregate(pick.iter().map(|&i| &keys[i]))
        .is_ok_and(|agg| agg.verify_signature_prepared(sig, msg).is_ok())
}

/// Tries every k-subset of `keys`, in parallel. Splits work by the subset's first index.
fn search(
    keys: &[PopVerified<PubkeyAffine>],
    k: usize,
    sig: &Signature,
    msg: &PreparedHashedMessage,
) -> Option<Vec<usize>> {
    let n = keys.len();
    if k == 0 || k > n {
        return None;
    }
    println!("searching C({n}, {k}) = {} subsets...", choose(n, k));
    let start = Instant::now();
    let found = (0..=n - k).into_par_iter().find_map_any(|first| {
        // Enumerate subsets {first} ∪ (k-1 indices from first+1..n) in lexicographic order.
        let mut idx: Vec<usize> = std::iter::once(first).chain(first + 1..first + k).collect();
        loop {
            if verifies(keys, &idx, sig, msg) {
                return Some(idx);
            }
            // Advance the tail (positions 1..k) to the next combination.
            let mut p = k;
            loop {
                if p == 1 {
                    return None;
                }
                p -= 1;
                if idx[p] < n - (k - p) {
                    idx[p] += 1;
                    for q in p + 1..k {
                        idx[q] = idx[q - 1] + 1;
                    }
                    break;
                }
            }
        }
    });
    println!("search took {:.1?}", start.elapsed());
    found
}

fn self_test() -> Result<()> {
    // 22 known keys; 11 of them sign a genesis payload. The search must find exactly those.
    let n = 22;
    let signers = [0usize, 2, 3, 5, 8, 9, 13, 14, 17, 19, 21];
    let kps: Vec<Keypair> = (0..n)
        .map(|i| Keypair::derive(&[i as u8 + 1; 32]).unwrap())
        .collect();
    let payload = genesis_payload(504_148_999, [7; 32], 12345);
    let sigs: Vec<SignatureProjective> = signers.iter().map(|&i| kps[i].sign(&payload)).collect();
    let agg = SignatureProjective::aggregate(sigs.iter())?;
    let signature = Signature::from(&agg);
    let keys: Vec<_> = kps
        .iter()
        .map(|kp| pop_verified(&PubkeyCompressed::from(&*kp.public).0))
        .collect::<Result<_>>()?;
    let msg = PreparedHashedMessage::new(&payload);

    let found = search(&keys, signers.len(), &signature, &msg).context("self-test: no match")?;
    if found != signers {
        bail!("self-test: found {found:?}, expected {signers:?}");
    }
    // And a wrong shred version must not verify.
    let wrong = PreparedHashedMessage::new(&genesis_payload(504_148_999, [7; 32], 12346));
    if verifies(&keys, &signers, &signature, &wrong) {
        bail!("self-test: wrong shred version verified");
    }
    println!("self-test OK: found {found:?}");
    Ok(())
}

fn bytes(v: &Value) -> Result<Vec<u8>> {
    v.as_array()
        .context("expected byte array")?
        .iter()
        .map(|b| {
            b.as_u64()
                .and_then(|b| u8::try_from(b).ok())
                .context("not a byte")
        })
        .collect()
}

/// signer-store base2: version 0 | bit count u16 LE | bits, LSB first.
fn decode_base2(bitmap: &[u8]) -> Result<(usize, Vec<usize>)> {
    if bitmap.len() < 3 || bitmap[0] != 0 {
        bail!("not a base2 bitmap");
    }
    let bits = u16::from_le_bytes([bitmap[1], bitmap[2]]) as usize;
    let body = &bitmap[3..];
    let ranks = (0..bits)
        .filter(|i| body.get(i / 8).is_some_and(|b| b >> (i % 8) & 1 == 1))
        .collect();
    Ok((bits, ranks))
}

fn choose(n: usize, k: usize) -> u128 {
    (0..k).fold(1u128, |acc, i| acc * (n - i) as u128 / (i + 1) as u128)
}
