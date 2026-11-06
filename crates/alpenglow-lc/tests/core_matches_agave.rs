//! `alpenglow-core` (pure Rust, runs in SP1) must agree with Agave's own verifier on every
//! certificate: real ones from devnet and the local validator, and thousands of random
//! valid and tampered synthetic ones. Agreement means both accept, or both reject.

use {
    agave_votor_messages::{
        certificate::CertificateType, consensus_message::Block,
        unverified_vote_message::UnverifiedCertificate,
    },
    alpenglow_core::finality::{self as core, CertType},
    alpenglow_lc::{
        cert::{self, FooterCert, RpcGenesisCert},
        validator_set::{RankedValidator, ValidatorSet},
    },
    serde_json::Value,
    solana_bls_signatures::{
        keypair::Keypair, pubkey::PubkeyCompressed, signature::SignatureAffine, Signature,
        SignatureCompressed, SignatureProjective,
    },
    solana_hash::Hash,
    std::num::NonZero,
};

fn fixture(path: &str) -> Value {
    let p = format!("{}/../../fixtures/{path}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn to_agave(t: CertType) -> CertificateType {
    let b = |slot, id| Block {
        slot,
        block_id: Hash::new_from_array(id),
    };
    match t {
        CertType::Notarize { slot, block_id } => CertificateType::Notarize(b(slot, block_id)),
        CertType::FinalizeFast { slot, block_id } => {
            CertificateType::FinalizeFast(b(slot, block_id))
        }
        CertType::Finalize { slot } => CertificateType::Finalize(slot),
        CertType::Genesis { slot, block_id } => CertificateType::Genesis(b(slot, block_id)),
    }
}

/// Core's verdict for a cert against a whole ranked set.
fn core_verify(
    t: CertType,
    shred: u16,
    sig: &[u8; 96],
    bitmap: &[u8],
    set: &ValidatorSet,
) -> Result<u64, core::Error> {
    let sig = core::signature_from_compressed(sig)?;
    core::verify_cert(
        &t,
        shred,
        &sig,
        bitmap,
        set.entries.len(),
        set.total_stake,
        |r| {
            let e = set.entries.get(r)?;
            Some((e.stake, core::pubkey_from_compressed(&e.bls_pubkey).ok()?))
        },
    )
}

/// Agave's verdict for the same inputs (decompressing the signature the way footers do).
fn agave_verify(
    t: CertType,
    shred: u16,
    sig: &[u8; 96],
    bitmap: &[u8],
    set: &ValidatorSet,
) -> bool {
    let Ok(affine) = SignatureAffine::try_from(SignatureCompressed(*sig)) else {
        return false;
    };
    let c = UnverifiedCertificate {
        cert_type: to_agave(t),
        signature: Signature::from(affine),
        bitmap: bitmap.to_vec(),
        shred_version: shred,
    };
    cert::verify(c, set).is_ok()
}

#[test]
fn payloads_match_agave() {
    let id = [9u8; 32];
    for t in [
        CertType::Notarize {
            slot: 77,
            block_id: id,
        },
        CertType::FinalizeFast {
            slot: 77,
            block_id: id,
        },
        CertType::Finalize { slot: 77 },
        CertType::Genesis {
            slot: 77,
            block_id: id,
        },
    ] {
        let c = UnverifiedCertificate {
            cert_type: to_agave(t),
            signature: Signature([0; 192]),
            bitmap: vec![],
            shred_version: 28090,
        };
        assert_eq!(c.get_vote_payload(), (t.payload(28090), None), "{t:?}");
    }
}

fn footer_parts(c: &FooterCert) -> Vec<(CertType, [u8; 96], Vec<u8>)> {
    let sig = |a: &cert::Aggregate| <[u8; 96]>::try_from(a.signature.as_slice()).unwrap();
    match &c.notar_aggregate {
        None => vec![(
            CertType::FinalizeFast {
                slot: c.slot,
                block_id: c.block_id,
            },
            sig(&c.final_aggregate),
            c.final_aggregate.bitmap.clone(),
        )],
        Some(n) => vec![
            (
                CertType::Notarize {
                    slot: c.slot,
                    block_id: c.block_id,
                },
                sig(n),
                n.bitmap.clone(),
            ),
            (
                CertType::Finalize { slot: c.slot },
                sig(&c.final_aggregate),
                c.final_aggregate.bitmap.clone(),
            ),
        ],
    }
}

#[test]
fn real_local_footer_certs_verify_in_core() {
    let set: ValidatorSet =
        serde_json::from_value(fixture("local/validator_set_0.json")["validator_set"].clone())
            .unwrap();
    let block = fixture("local/block_44000.json");
    let mut certs = vec![
        block["block_final_cert"].clone(),
        block["parent_block_final_cert"].clone(),
    ];
    let sample = std::fs::read_to_string(format!(
        "{}/../../fixtures/local/footers_sample.jsonl",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    certs.extend(
        sample
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["block_final_cert"].clone())
            .filter(|v| !v.is_null()),
    );
    assert_eq!(certs.len(), 8);
    for v in certs {
        let c: FooterCert = serde_json::from_value(v).unwrap();
        for (t, sig, bitmap) in footer_parts(&c) {
            assert_eq!(
                core_verify(t, 28090, &sig, &bitmap, &set),
                Ok(set.total_stake),
                "{t:?}"
            );
            assert!(agave_verify(t, 28090, &sig, &bitmap, &set));
        }
    }
}

#[test]
fn real_devnet_genesis_cert_verifies_in_core() {
    // Same reconstruction as tests/devnet_genesis.rs: genesis rank -> today's entry.
    const GENESIS_RANKS: [usize; 11] = [0, 1, 2, 3, 4, 7, 9, 10, 14, 15, 16];
    const TODAY_RANKS: [usize; 11] = [0, 1, 2, 3, 4, 7, 10, 11, 17, 18, 19];
    let c: RpcGenesisCert =
        serde_json::from_value(fixture("devnet/genesis_cert.json")["result"].clone()).unwrap();
    let set: ValidatorSet =
        serde_json::from_value(fixture("devnet/validator_set_1198.json")["validator_set"].clone())
            .unwrap();
    let sig =
        core::signature_from_uncompressed(c.signature.signature.as_slice().try_into().unwrap())
            .unwrap();
    let t = CertType::Genesis {
        slot: c.block.slot,
        block_id: c.block.block_id,
    };
    let map = |r| {
        let i = GENESIS_RANKS.iter().position(|&g| g == r)?;
        let e = &set.entries[TODAY_RANKS[i]];
        Some((e.stake, core::pubkey_from_compressed(&e.bls_pubkey).ok()?))
    };
    core::verify_cert(
        &t,
        7016,
        &sig,
        &c.signature.bitmap,
        17,
        set.total_stake,
        map,
    )
    .unwrap();
    assert_eq!(
        core::verify_cert(
            &t,
            7017,
            &sig,
            &c.signature.bitmap,
            17,
            set.total_stake,
            map
        ),
        Err(core::Error::VerifySig)
    );
    // Agave agrees on the same reconstruction.
    let agave = cert::verify_with(
        c.to_unverified(7016).unwrap(),
        17,
        NonZero::new(set.total_stake).unwrap(),
        |r| {
            let i = GENESIS_RANKS.iter().position(|&g| g == r)?;
            let e = &set.entries[TODAY_RANKS[i]];
            Some((NonZero::new(e.stake)?, e.pop_verified_key()?))
        },
    );
    assert!(agave.is_ok());
}

/// Tiny deterministic PRNG (xorshift64*), so failures reproduce.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn base2(ranks: &[usize], bits: usize) -> Vec<u8> {
    let mut out = vec![0u8, bits as u8, (bits >> 8) as u8];
    out.resize(3 + bits.div_ceil(8), 0);
    for &r in ranks {
        out[3 + r / 8] |= 1 << (r % 8);
    }
    out
}

#[test]
fn core_and_agave_agree_on_random_certs() {
    let n = 24;
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    let kps: Vec<Keypair> = (0..n)
        .map(|i| Keypair::derive(&[i as u8 + 1; 32]).unwrap())
        .collect();
    // Random stakes, then rank like Agave: stake descending, key ascending.
    let mut entries: Vec<(RankedValidator, usize)> = kps
        .iter()
        .enumerate()
        .map(|(i, kp)| {
            let v = RankedValidator {
                bls_pubkey: PubkeyCompressed::from(&*kp.public).0,
                stake: 1 + rng.below(1_000),
                vote_pubkey: [i as u8; 32],
                node_pubkey: [i as u8; 32],
            };
            (v, i)
        })
        .collect();
    entries.sort_by(|a, b| {
        b.0.stake
            .cmp(&a.0.stake)
            .then(a.0.bls_pubkey.cmp(&b.0.bls_pubkey))
    });
    let total = entries.iter().map(|e| e.0.stake).sum();
    let keypair_of: Vec<usize> = entries.iter().map(|e| e.1).collect();
    let set = ValidatorSet {
        epoch: 0,
        total_stake: total,
        entries: entries.into_iter().map(|e| e.0).collect(),
    };

    let (mut accepted, mut rejected) = (0, 0);
    for round in 0..400 {
        let id: [u8; 32] = std::array::from_fn(|_| rng.below(256) as u8);
        let slot = rng.below(1_000_000);
        let t = match rng.below(4) {
            0 => CertType::Notarize { slot, block_id: id },
            1 => CertType::FinalizeFast { slot, block_id: id },
            2 => CertType::Finalize { slot },
            _ => CertType::Genesis { slot, block_id: id },
        };
        // Signer sets of every size, so stake lands on both sides of each threshold.
        let k = 1 + rng.below(n as u64) as usize;
        let mut ranks: Vec<usize> = (0..n).collect();
        for i in 0..n {
            ranks.swap(i, i + rng.below((n - i) as u64) as usize);
        }
        ranks.truncate(k);
        let shred = 28090;
        let payload = t.payload(shred);
        let sigs: Vec<_> = ranks
            .iter()
            .map(|&r| kps[keypair_of[r]].sign(&payload))
            .collect();
        let agg = SignatureProjective::aggregate(sigs.iter()).unwrap();
        let mut sig = SignatureCompressed::from(SignatureAffine::from(agg)).0;
        let mut bitmap = base2(&ranks, n);
        let mut check_shred = shred;
        // Half the rounds: one tamper.
        match round % 8 {
            0 => sig[rng.below(96) as usize] ^= 1 << rng.below(8),
            1 => bitmap[3 + rng.below(3) as usize] ^= 1 << rng.below(8),
            2 => check_shred ^= 1,
            3 => bitmap = base2(&ranks, n + 8), // bitmap longer than the set
            _ => {}
        }
        let agave = agave_verify(t, check_shred, &sig, &bitmap, &set);
        let core = core_verify(t, check_shred, &sig, &bitmap, &set);
        assert_eq!(
            agave,
            core.is_ok(),
            "round {round}: {t:?} k={k} agave={agave} core={core:?}"
        );
        if agave {
            accepted += 1
        } else {
            rejected += 1
        }
    }
    // Make sure both outcomes were exercised plenty.
    assert!(
        accepted > 50 && rejected > 150,
        "accepted {accepted}, rejected {rejected}"
    );
}

/// Every footer captured so far, through core. Run with `-- --ignored`.
#[test]
#[ignore]
fn every_captured_footer_cert_verifies_in_core() {
    let set: ValidatorSet =
        serde_json::from_value(fixture("local/validator_set_0.json")["validator_set"].clone())
            .unwrap();
    let text = std::fs::read_to_string(format!(
        "{}/../../fixtures/local/footers.jsonl",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    let complete = &text[..text.rfind('\n').map_or(0, |i| i + 1)];
    let mut n = 0;
    for l in complete.lines() {
        let v: Value = serde_json::from_str(l).unwrap();
        let Some(c) =
            serde_json::from_value::<Option<FooterCert>>(v["block_final_cert"].clone()).unwrap()
        else {
            continue;
        };
        for (t, sig, bitmap) in footer_parts(&c) {
            core_verify(t, 28090, &sig, &bitmap, &set)
                .unwrap_or_else(|e| panic!("footer {}: {e}", v["slot"]));
        }
        n += 1;
    }
    println!("{n} certs verified in core");
}
