//! Adversarial tests written by an outside reviewer (not part of the original build).
//! Each test names the attack it tries. Tests marked DEMONSTRATES prove a *trust
//! assumption*: they succeed in breaking the verifier when the assumption is violated, so
//! the assumption has to hold elsewhere (see docs/adversarial-report.md).

use {
    alpenglow_core::{
        block::{self, Component},
        finality::{self, CertType},
        fixture,
        proof::{Aggregate, Error, FinalityInput, Path, Validator},
        shred::{self, Hash},
    },
    bls12_381::{G1Affine, G1Projective, G2Affine, Scalar},
    serde_json::Value,
};

fn json(file: &str) -> Value {
    let p = format!("{}/../../fixtures/local/{file}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn base() -> FinalityInput {
    fixture::block_input(&json("block_46718.json")).finality
}

fn not_enough(e: Error) -> bool {
    matches!(e, Error::Finality(finality::Error::NotEnoughStake { .. }))
}

fn equal_stake_set(n: usize) -> (Vec<Scalar>, Vec<Validator>) {
    let (sks, mut v) = fixture::synthetic_set(n);
    for x in &mut v {
        x.stake = 1;
    }
    (sks, v)
}

// ---------------------------------------------------------------------------------------
// Finality: key and signature games
// ---------------------------------------------------------------------------------------

/// DEMONSTRATES: the verifier sums public keys with no proof of possession, so ONE rogue
/// key in the posted validator set lets its owner forge finality alone, claiming the stake
/// of every honest validator. The only defence is that Solana's vote program checked proof
/// of possession when each key was registered and that the set owner posts only those keys.
#[test]
fn rogue_key_forges_finality_if_a_key_without_proof_of_possession_is_in_the_set() {
    let b = base();
    let (sks, mut validators) = fixture::synthetic_set(3);
    let honest_sum: Scalar = sks.iter().copied().sum();
    let x = Scalar::from(0xdead_beef_u64); // the attacker's only secret
    let evil_pk = G1Projective::generator() * (x - honest_sum); // attacker does not need its dlog
    validators.push(Validator {
        bls_pubkey: G1Affine::from(evil_pk).to_compressed().to_vec(),
        stake: 1,
    });
    let cert = CertType::FinalizeFast {
        slot: b.slot,
        block_id: b.block_id,
    };
    // sign() adds the listed signers' secrets: honest entries 0, the attacker's entry x.
    let fake = [
        Scalar::from(0u64),
        Scalar::from(0u64),
        Scalar::from(0u64),
        x,
    ];
    let agg = fixture::sign(&fake, &[0, 1, 2, 3], cert, b.shred_version);
    let forged = FinalityInput {
        validators,
        final_aggregate: agg,
        notar_aggregate: None,
        ..b
    };
    let f = forged.verify().expect("rogue key forged a certificate");
    assert_eq!(f.path, Path::Fast);
    assert_eq!(
        f.signed_stake, f.total_stake,
        "attacker claimed 100% of stake with one secret"
    );
}

/// Two non-identity keys that cancel (P and -P) plus the identity signature would verify
/// trivially if the aggregate key were allowed to be the identity.
#[test]
fn cancelling_keys_with_identity_signature_are_rejected() {
    let b = base();
    let p = G1Projective::generator() * Scalar::from(5u64);
    let v = |pt: G1Projective| Validator {
        bls_pubkey: G1Affine::from(pt).to_compressed().to_vec(),
        stake: 50,
    };
    let forged = FinalityInput {
        validators: vec![v(p), v(-p)],
        final_aggregate: Aggregate {
            signature: G2Affine::identity().to_compressed().to_vec(),
            bitmap: vec![0, 2, 0, 0b11],
        },
        notar_aggregate: None,
        ..b
    };
    assert_eq!(
        forged.verify().unwrap_err(),
        Error::Finality(finality::Error::VerifySig)
    );
}

/// An identity public key in the set can never count as a signer.
#[test]
fn identity_public_key_never_signs() {
    let b = base();
    let forged = FinalityInput {
        validators: vec![Validator {
            bls_pubkey: G1Affine::identity().to_compressed().to_vec(),
            stake: 100,
        }],
        final_aggregate: Aggregate {
            signature: G2Affine::identity().to_compressed().to_vec(),
            bitmap: vec![0, 1, 0, 1],
        },
        notar_aggregate: None,
        ..b
    };
    assert!(forged.verify().is_err());
}

/// A signature over the right message by the wrong cluster (another shred version) fails.
#[test]
fn signature_does_not_transfer_between_shred_versions() {
    let b = base();
    let good = fixture::synthetic(&b, 20);
    assert!(good.verify().is_ok());
    let mut other = good;
    other.shred_version ^= 1;
    assert_eq!(
        other.verify().unwrap_err(),
        Error::Finality(finality::Error::VerifySig)
    );
}

/// A FinalizeFast (tag 1) signature is the same payload as Notarize (tag 1), by design.
/// It must NOT also pass as Finalize(slot) (tag 2): swapping the two certs in the slow
/// path must fail.
#[test]
fn slow_path_certs_cannot_be_swapped() {
    let b = base();
    let s = fixture::synthetic_slow(&b, 20);
    let mut swapped = s.clone();
    let notar = s.notar_aggregate.clone().unwrap();
    swapped.notar_aggregate = Some(s.final_aggregate.clone());
    swapped.final_aggregate = notar;
    assert!(swapped.verify().is_err());
}

// ---------------------------------------------------------------------------------------
// Finality: stake thresholds, exactly at the edge
// ---------------------------------------------------------------------------------------

#[test]
fn fast_path_threshold_is_exactly_80_percent() {
    let b = base();
    let (sks, validators) = equal_stake_set(100);
    let cert = CertType::FinalizeFast {
        slot: b.slot,
        block_id: b.block_id,
    };
    let attempt = |k: usize| {
        let signers: Vec<usize> = (0..k).collect();
        FinalityInput {
            validators: validators.clone(),
            final_aggregate: fixture::sign(&sks, &signers, cert, b.shred_version),
            notar_aggregate: None,
            ..b.clone()
        }
        .verify()
    };
    assert!(attempt(80).is_ok());
    assert!(not_enough(attempt(79).unwrap_err()));
}

#[test]
fn slow_path_threshold_is_exactly_60_percent_on_both_certs() {
    let b = base();
    let (sks, validators) = equal_stake_set(100);
    let attempt = |k_notar: usize, k_final: usize| {
        let n: Vec<usize> = (0..k_notar).collect();
        let f: Vec<usize> = (0..k_final).collect();
        FinalityInput {
            validators: validators.clone(),
            final_aggregate: fixture::sign(
                &sks,
                &f,
                CertType::Finalize { slot: b.slot },
                b.shred_version,
            ),
            notar_aggregate: Some(fixture::sign(
                &sks,
                &n,
                CertType::Notarize {
                    slot: b.slot,
                    block_id: b.block_id,
                },
                b.shred_version,
            )),
            ..b.clone()
        }
        .verify()
    };
    assert!(attempt(60, 60).is_ok());
    assert!(
        not_enough(attempt(59, 60).unwrap_err()),
        "notarize at 59% must fail"
    );
    assert!(
        not_enough(attempt(60, 59).unwrap_err()),
        "finalize at 59% must fail"
    );
}

/// Stakes that overflow u64 when summed are refused rather than wrapped.
#[test]
fn stake_overflow_is_refused() {
    let b = base();
    let (_, mut v) = fixture::synthetic_set(2);
    v[0].stake = u64::MAX;
    v[1].stake = 1;
    let f = FinalityInput { validators: v, ..b };
    assert_eq!(f.verify().unwrap_err(), Error::EmptySet);
}

/// Huge but legal stakes: the cross-multiplication must not overflow (u128).
#[test]
fn threshold_math_does_not_overflow_with_huge_stakes() {
    let b = base();
    let (sks, mut v) = fixture::synthetic_set(2);
    v[0].stake = u64::MAX / 2;
    v[1].stake = u64::MAX / 2;
    let cert = CertType::FinalizeFast {
        slot: b.slot,
        block_id: b.block_id,
    };
    let both = FinalityInput {
        validators: v.clone(),
        final_aggregate: fixture::sign(&sks, &[0, 1], cert, b.shred_version),
        notar_aggregate: None,
        ..b.clone()
    };
    assert!(both.verify().is_ok());
    let half = FinalityInput {
        final_aggregate: fixture::sign(&sks, &[0], cert, b.shred_version),
        validators: v,
        ..b
    };
    assert!(not_enough(half.verify().unwrap_err()));
}

// ---------------------------------------------------------------------------------------
// Signer bitmap
// ---------------------------------------------------------------------------------------

/// Spare bits past `bits` in the last byte are ignored, so one certificate has many valid
/// encodings. Harmless to safety (nothing hashes the bitmap) but anything that keys on
/// certificate bytes would see different values for the same certificate.
#[test]
fn bitmap_spare_bits_are_malleable_but_harmless() {
    let b = base();
    let good = fixture::synthetic(&b, 10);
    let mut m = good.clone();
    let last = m.final_aggregate.bitmap.len() - 1;
    m.final_aggregate.bitmap[last] |= 0b1111_1100; // bits 10..16 do not exist
    assert_ne!(m.final_aggregate.bitmap, good.final_aggregate.bitmap);
    let (a, c) = (good.verify().unwrap(), m.verify().unwrap());
    assert_eq!(
        a, c,
        "same statement proven from two different certificate encodings"
    );
}

#[test]
fn bitmap_framing_attacks_are_rejected() {
    let b = base();
    let good = fixture::synthetic(&b, 10);
    let mutate = |f: &dyn Fn(&mut Vec<u8>)| {
        let mut m = good.clone();
        f(&mut m.final_aggregate.bitmap);
        m.verify().unwrap_err()
    };
    // base3 marker, claimed length longer/shorter than the set, payload too long/short.
    assert_eq!(
        mutate(&|bm| bm[0] = 1),
        Error::Finality(finality::Error::WrongEncoding)
    );
    assert!(matches!(
        mutate(&|bm| bm[0] = 7),
        Error::Finality(finality::Error::Decode)
    ));
    assert!(matches!(
        mutate(&|bm| bm[1] = 11),
        Error::Finality(finality::Error::Decode)
    ));
    assert!(matches!(
        mutate(&|bm| bm.push(0)),
        Error::Finality(finality::Error::Decode)
    ));
    assert!(matches!(
        mutate(&|bm| {
            bm.pop();
        }),
        Error::Finality(finality::Error::Decode)
    ));
    assert!(matches!(
        mutate(&|bm| bm.truncate(2)),
        Error::Finality(finality::Error::Decode)
    ));
    // A bitmap longer than the validator set names ranks that do not exist.
    assert!(matches!(
        mutate(&|bm| {
            bm[1] = 0xff;
            bm[2] = 0xff;
        }),
        Error::Finality(finality::Error::Decode)
    ));
}

/// Signing with the right keys but claiming MORE signers than actually signed.
#[test]
fn claiming_extra_signers_breaks_the_signature() {
    let b = base();
    let (sks, validators) = equal_stake_set(100);
    let cert = CertType::FinalizeFast {
        slot: b.slot,
        block_id: b.block_id,
    };
    let mut agg = fixture::sign(&sks, &(0..79).collect::<Vec<_>>(), cert, b.shred_version);
    agg.bitmap = fixture::sign(&sks, &(0..80).collect::<Vec<_>>(), cert, b.shred_version).bitmap;
    let f = FinalityInput {
        validators,
        final_aggregate: agg,
        notar_aggregate: None,
        ..b
    };
    assert_eq!(
        f.verify().unwrap_err(),
        Error::Finality(finality::Error::VerifySig)
    );
}

// ---------------------------------------------------------------------------------------
// Merkle tree: the odd-leaf duplication trick (CVE-2012-2459 style)
// ---------------------------------------------------------------------------------------

/// The raw Merkle primitive pairs an odd last node with itself, so appending a copy of the
/// last leaf does not change the root. block_id defends by binding the FEC-set count into
/// the last leaf. This test proves both halves.
#[test]
fn odd_leaf_duplication_is_blocked_by_the_count_leaf() {
    let parent: Hash = [9; 32];
    for n in 1..=9u8 {
        let roots: Vec<Hash> = (1..=n).map(|i| [i; 32]).collect();
        let id = shred::block_id(&roots, 7, &parent).unwrap();

        // Raw primitive: [.., x] and [.., x, x] collide when the leaf count is odd.
        let mut leaves = roots.clone();
        leaves.push(shred::parent_leaf(7, &parent, n as u32));
        if leaves.len() % 2 == 1 {
            let mut dup = leaves.clone();
            dup.push(*leaves.last().unwrap());
            assert_eq!(
                shred::merkle_root(&leaves).unwrap(),
                shred::merkle_root(&dup).unwrap(),
                "primitive is malleable"
            );
        }

        // block_id: every way of padding or trimming the FEC roots gives a different id.
        let mut dup_root = roots.clone();
        dup_root.push(*roots.last().unwrap());
        assert_ne!(
            id,
            shred::block_id(&dup_root, 7, &parent).unwrap(),
            "n={n} duplicate root"
        );
        if n > 1 {
            assert_ne!(
                id,
                shred::block_id(&roots[..roots.len() - 1], 7, &parent).unwrap(),
                "n={n} trimmed"
            );
        }
        // Presenting the count leaf itself as an extra FEC root.
        let mut fake = roots.clone();
        fake.push(shred::parent_leaf(7, &parent, n as u32));
        assert_ne!(
            id,
            shred::block_id(&fake, 7, &parent).unwrap(),
            "n={n} count leaf as root"
        );
        // Wrong parent slot / id.
        assert_ne!(id, shred::block_id(&roots, 8, &parent).unwrap());
        assert_ne!(id, shred::block_id(&roots, 7, &[8; 32]).unwrap());
    }
}

// ---------------------------------------------------------------------------------------
// Transaction parser: what the contract will read must be what Solana executed
// ---------------------------------------------------------------------------------------

fn cu16(n: usize) -> Vec<u8> {
    match n {
        0..=0x7f => vec![n as u8],
        0x80..=0x3fff => vec![(n & 0x7f) as u8 | 0x80, (n >> 7) as u8],
        _ => vec![
            (n & 0x7f) as u8 | 0x80,
            ((n >> 7) & 0x7f) as u8 | 0x80,
            (n >> 14) as u8,
        ],
    }
}

const MEMO: [u8; 32] = [
    0x05, 0x4a, 0x53, 0x52, 0x28, 0x54, 0xcd, 0xd1, 0x03, 0xcc, 0x2e, 0x46, 0x31, 0xa3, 0xf5, 0x17,
    0x92, 0xac, 0x72, 0x7c, 0xf1, 0xe5, 0x0a, 0xbc, 0x97, 0x82, 0x8a, 0xc5, 0x99, 0x4f, 0x3c, 0xc2,
];

struct Tx {
    sigs: usize,
    header: [u8; 3],
    keys: Vec<[u8; 32]>,
    ixs: Vec<(u8, Vec<u8>, Vec<u8>)>,
    v0: Option<u8>,
    lookups: Vec<([u8; 32], Vec<u8>, Vec<u8>)>,
}

impl Tx {
    fn valid() -> Tx {
        Tx {
            sigs: 1,
            header: [1, 0, 1],
            keys: vec![[7; 32], MEMO],
            ixs: vec![(1, vec![0], b"huginn:1:greeter:hi".to_vec())],
            v0: None,
            lookups: vec![],
        }
    }
    fn bytes(&self) -> Vec<u8> {
        let mut o = cu16(self.sigs);
        for i in 0..self.sigs {
            o.extend_from_slice(&[i as u8 + 1; 64]);
        }
        if let Some(v) = self.v0 {
            o.push(0x80 | v);
        }
        o.extend_from_slice(&self.header);
        o.extend(cu16(self.keys.len()));
        for k in &self.keys {
            o.extend_from_slice(k);
        }
        o.extend_from_slice(&[3; 32]);
        o.extend(cu16(self.ixs.len()));
        for (p, a, d) in &self.ixs {
            o.push(*p);
            o.extend(cu16(a.len()));
            o.extend_from_slice(a);
            o.extend(cu16(d.len()));
            o.extend_from_slice(d);
        }
        if self.v0.is_some() {
            o.extend(cu16(self.lookups.len()));
            for (t, w, r) in &self.lookups {
                o.extend_from_slice(t);
                o.extend(cu16(w.len()));
                o.extend_from_slice(w);
                o.extend(cu16(r.len()));
                o.extend_from_slice(r);
            }
        }
        o
    }
}

fn component(tx: &[u8]) -> Vec<u8> {
    let mut c = 1u64.to_le_bytes().to_vec();
    c.extend_from_slice(&1u64.to_le_bytes()); // num_hashes
    c.extend_from_slice(&[0; 32]); // entry hash
    c.extend_from_slice(&1u64.to_le_bytes()); // tx count
    c.extend_from_slice(tx);
    c
}

fn parses(tx: &Tx) -> bool {
    block::parse_component(&component(&tx.bytes())).is_ok()
}

#[test]
fn parser_accepts_the_valid_baseline_and_reads_what_was_written() {
    let bytes = component(&Tx::valid().bytes());
    let Component::Entries(e) = block::parse_component(&bytes).unwrap() else {
        panic!()
    };
    let t = &e[0].transactions[0];
    assert_eq!(t.signers(), &[[7u8; 32]]);
    assert_eq!(t.program_id(&t.instructions[0]), Some(&MEMO));
    assert_eq!(t.instructions[0].data, b"huginn:1:greeter:hi");
}

#[test]
fn parser_rejects_structurally_invalid_transactions() {
    let mut t = Tx::valid();
    t.sigs = 2;
    assert!(!parses(&t), "2 signatures, 1 required");

    let mut t = Tx::valid();
    t.sigs = 0;
    t.header[0] = 0;
    assert!(!parses(&t), "no signers");

    let mut t = Tx::valid();
    t.ixs[0].0 = 0;
    assert!(!parses(&t), "program id is the fee payer");

    let mut t = Tx::valid();
    t.ixs[0].0 = 2;
    assert!(!parses(&t), "program id index past static keys");

    let mut t = Tx::valid();
    t.ixs[0].1 = vec![9];
    assert!(!parses(&t), "instruction account index out of range");

    let mut t = Tx::valid();
    t.header = [1, 1, 1];
    assert!(!parses(&t), "fee payer would be readonly");

    let mut t = Tx::valid();
    t.header = [1, 0, 2];
    assert!(!parses(&t), "more readonly-unsigned than non-signers");

    let mut t = Tx::valid();
    t.v0 = Some(1);
    assert!(!parses(&t), "unknown message version");

    let mut t = Tx::valid();
    t.v0 = Some(0);
    t.lookups = vec![([5; 32], vec![0; 200], vec![0; 100])];
    assert!(!parses(&t), "v0: more than 256 accounts through lookups");

    let mut bytes = component(&Tx::valid().bytes());
    bytes.push(0);
    assert!(block::parse_component(&bytes).is_err(), "trailing byte");
}

/// A length written non-minimally (0x80 0x00 for 0) is a second spelling of the same
/// transaction. Solana rejects it; accepting it would let one transaction hash two ways.
#[test]
fn non_canonical_compact_u16_is_rejected() {
    let t = Tx::valid();
    let mut b = t.bytes();
    // signatures count 01 -> 81 00
    b.splice(0..1, [0x81, 0x00]);
    assert!(block::parse_component(&component(&b)).is_err());
    // three-byte spelling of a small number
    let mut b = t.bytes();
    b.splice(0..1, [0x81, 0x80, 0x00]);
    assert!(block::parse_component(&component(&b)).is_err());
    // value over u16::MAX
    let mut b = t.bytes();
    b.splice(0..1, [0xff, 0xff, 0x7f]);
    assert!(block::parse_component(&component(&b)).is_err());
}

/// Forged huge counts must fail fast and not allocate or loop for ages.
#[test]
fn huge_counts_fail_without_blowing_up() {
    let start = std::time::Instant::now();
    for forged in [vec![0xff, 0xff, 0x03], vec![0xe8, 0x07]] {
        // as signature count, as key count, as instruction count
        let mut b = Tx::valid().bytes();
        b.splice(0..1, forged.clone());
        assert!(block::parse_component(&component(&b)).is_err());
    }
    // entry batch claiming u64::MAX entries / transactions
    let mut c = u64::MAX.to_le_bytes().to_vec();
    c.extend_from_slice(&[0; 100]);
    assert!(block::parse_component(&c).is_err());
    let mut c = component(&Tx::valid().bytes());
    c[48..56].copy_from_slice(&u64::MAX.to_le_bytes()); // tx_count
    assert!(block::parse_component(&c).is_err());
    assert!(start.elapsed().as_secs() < 2);
}

/// DOCUMENTS: duplicate account keys parse. Agave refuses such a transaction before it can
/// execute, so it cannot reach a finalized block, but the Solidity `SolanaTx` accepts it
/// too, so a contract reading unproven bytes sees it.
#[test]
fn duplicate_account_keys_are_accepted_by_the_parser() {
    let mut t = Tx::valid();
    t.header = [2, 0, 1];
    t.sigs = 2;
    t.keys = vec![[7; 32], [7; 32], MEMO];
    t.ixs[0].0 = 2;
    assert!(parses(&t));
}

/// Mutation fuzz: no input may panic, and any accepted transaction must satisfy the
/// invariants contracts rely on.
#[test]
fn mutation_fuzz_never_panics_and_accepted_transactions_are_sane() {
    let seeds: Vec<Vec<u8>> = {
        let mut v0 = Tx::valid();
        v0.v0 = Some(0);
        v0.lookups = vec![([5; 32], vec![0, 1], vec![2])];
        let mut multi = Tx::valid();
        multi.sigs = 2;
        multi.header = [2, 0, 1];
        multi.keys = vec![[7; 32], [8; 32], MEMO];
        multi.ixs = vec![(2, vec![0, 1], vec![b'x'; 200]), (2, vec![], vec![])];
        vec![
            component(&Tx::valid().bytes()),
            component(&v0.bytes()),
            component(&multi.bytes()),
        ]
    };
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut accepted = 0u32;
    for i in 0..400_000u32 {
        let mut b = seeds[(i % 3) as usize].clone();
        for _ in 0..1 + next() % 4 {
            let pos = (next() as usize) % b.len().max(1);
            match next() % 5 {
                0 => b[pos] ^= 1 << (next() % 8),
                1 => b[pos] = next() as u8,
                2 => {
                    b.remove(pos);
                }
                3 => b.insert(pos, next() as u8),
                _ => b.truncate(pos),
            }
            if b.is_empty() {
                break;
            }
        }
        if let Ok(Component::Entries(entries)) = block::parse_component(&b) {
            for e in &entries {
                for t in &e.transactions {
                    accepted += 1;
                    assert_eq!(t.signatures.len(), t.num_required_signatures as usize);
                    assert!(t.num_required_signatures as usize <= t.account_keys.len());
                    assert!(t.num_readonly_signed < t.num_required_signatures);
                    for ix in &t.instructions {
                        assert!(
                            ix.program_id_index != 0
                                && (ix.program_id_index as usize) < t.account_keys.len()
                        );
                    }
                    assert!(t.raw.len() >= 64 * t.signatures.len());
                }
            }
        }
    }
    assert!(
        accepted > 1000,
        "fuzzer should still reach accepted inputs, got {accepted}"
    );
}

/// A minimal valid v1 transaction (prefix 0x81, signatures after the message): one signer,
/// the Memo program, one Memo instruction, a compute unit limit in the config.
fn v1_tx(memo: &[u8]) -> Vec<u8> {
    let mut o = vec![0x81, 1, 0, 1];
    o.extend_from_slice(&0b100u32.to_le_bytes()); // config: compute unit limit
    o.extend_from_slice(&[3; 32]); // blockhash
    o.extend_from_slice(&[1, 2]); // 1 instruction, 2 addresses
    o.extend_from_slice(&[8; 32]);
    o.extend_from_slice(&MEMO);
    o.extend_from_slice(&200_000u32.to_le_bytes());
    o.extend_from_slice(&[1, 1]); // program 1, 1 account
    o.extend_from_slice(&(memo.len() as u16).to_le_bytes());
    o.push(0);
    o.extend_from_slice(memo);
    o.extend_from_slice(&[0x55; 64]); // the signature, at the end
    o
}

fn entry_of(txs: &[&[u8]]) -> Vec<u8> {
    let mut c = 1u64.to_le_bytes().to_vec(); // one entry
    c.extend_from_slice(&1u64.to_le_bytes()); // num_hashes
    c.extend_from_slice(&[0; 32]);
    c.extend_from_slice(&(txs.len() as u64).to_le_bytes());
    for t in txs {
        c.extend_from_slice(t);
    }
    c
}

/// FIXED (finding 4). Before: one transaction Huginn couldn't parse made its whole
/// component unparseable, so its neighbours were unprovable. v1 transactions (SIMD-0385)
/// are live on mainnet since epoch 1035, so this was a real censorship vector.
/// Now v1 parses, and a component is parsed only up to the target transaction.
#[test]
fn fixed_a_neighbouring_transaction_no_longer_blocks_the_target() {
    let good = Tx::valid().bytes();
    let sig = [1u8; 64]; // Tx::valid()'s first signature
    let v1 = v1_tx(b"a v1 neighbour");
    let mut junk = vec![0x82u8, 1, 0, 1]; // no Solana format starts with 0x82
    junk.extend_from_slice(&[0u8; 200]);

    // A real v1 transaction before or after the target: everything parses.
    for c in [entry_of(&[&good, &v1]), entry_of(&[&v1, &good])] {
        assert!(block::parse_component(&c).is_ok());
        assert!(block::find_in_component(&c, &sig).unwrap().is_some());
    }
    // Bytes no parser knows, after the target: never read, the target is found.
    let c = entry_of(&[&good, &junk]);
    assert!(
        block::parse_component(&c).is_err(),
        "the whole component still doesn't parse"
    );
    let (e, t, tx) = block::find_in_component(&c, &sig).unwrap().unwrap();
    assert_eq!((e, t, tx.raw), (0, 0, &good[..]));
}

/// What's left: bytes no parser knows BEFORE the target still hide it, because the target's
/// start can't be found without reading past them. Such bytes can't be in a finalized
/// block (Agave couldn't have replayed it), so this only bites if Solana adds a new
/// transaction format that Huginn doesn't know yet. The v1 test above is that case for v1.
#[test]
fn an_unknown_format_before_the_target_still_hides_it() {
    let good = Tx::valid().bytes();
    let mut junk = vec![0x82u8, 1, 0, 1];
    junk.extend_from_slice(&[0u8; 200]);
    assert!(block::find_in_component(&entry_of(&[&junk, &good]), &[1u8; 64]).is_err());
}

#[test]
fn v1_transaction_fields_and_signed_message() {
    let b = v1_tx(b"huginn:1:0xabc:1:hi");
    let c = entry_of(&[&b]);
    let Component::Entries(entries) = block::parse_component(&c).unwrap() else {
        panic!()
    };
    let t = &entries[0].transactions[0];
    assert_eq!(t.version, Some(1));
    assert_eq!(t.signatures, vec![[0x55; 64]]);
    assert_eq!(t.account_keys, vec![[8; 32], MEMO]);
    assert_eq!(t.recent_blockhash, [3; 32]);
    assert_eq!(t.instructions.len(), 1);
    assert_eq!(
        (
            t.instructions[0].program_id_index,
            t.instructions[0].accounts,
            t.instructions[0].data
        ),
        (1, &[0u8][..], &b"huginn:1:0xabc:1:hi"[..])
    );
    // The signature signs everything before it, the 0x81 byte included.
    assert_eq!(t.message, &b[..b.len() - 64]);
    assert_eq!(t.raw, &b[..]);
}

#[test]
fn v1_rules_match_agave() {
    let base = v1_tx(b"x");
    let bad = |f: &dyn Fn(&mut Vec<u8>)| {
        let mut b = base.clone();
        f(&mut b);
        block::parse_component(&entry_of(&[&b])).is_err()
    };
    assert!(!bad(&|_| {}));
    assert!(bad(&|b| b[4] = 0b100000), "unknown config bit");
    assert!(bad(&|b| b[4] = 0b01), "half a priority fee");
    assert!(
        bad(&|b| b[42..74].copy_from_slice(&MEMO)),
        "duplicate addresses"
    );
    assert!(bad(&|b| b[2] = 1), "readonly fee payer");
    assert!(
        bad(&|b| {
            b.pop();
        }),
        "signature cut short"
    );
    assert!(bad(&|b| b.push(0)), "trailing byte");
    assert!(bad(&|b| b[110] = 0), "program id is the fee payer");
    assert!(bad(&|b| b[112] = 0xff), "data length past the end");
}

/// A v1 transaction serialized by Agave's own SDK (`fixtures/tx_v1_agave.json`): two
/// signers, priority fee + compute limit + heap size, a 300-byte instruction.
#[test]
fn v1_transaction_written_by_agave() {
    let p = format!(
        "{}/../../fixtures/tx_v1_agave.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let j: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    let raw = hex::decode(j["raw"].as_str().unwrap()).unwrap();
    let message = hex::decode(j["message"].as_str().unwrap()).unwrap();
    let c = entry_of(&[&raw]);
    let Component::Entries(entries) = block::parse_component(&c).unwrap() else {
        panic!()
    };
    let t = &entries[0].transactions[0];
    assert_eq!(t.version, Some(1));
    assert_eq!(t.message, &message[..]);
    assert_eq!(t.signatures, vec![[0x11; 64], [0x22; 64]]);
    assert_eq!(
        (
            t.num_required_signatures,
            t.num_readonly_signed,
            t.num_readonly_unsigned
        ),
        (2, 1, 1)
    );
    assert_eq!(
        t.account_keys,
        vec![[0xa1; 32], [0xb2; 32], [0xc3; 32], MEMO]
    );
    assert_eq!(t.recent_blockhash, [0x77; 32]);
    assert_eq!(
        t.instructions[0].data,
        b"huginn: hello from a v1 transaction"
    );
    assert_eq!(t.instructions[0].accounts, &[0, 1]);
    assert_eq!(t.instructions[1].data, &[b'a'; 300][..]);
    // Found by its first signature, which sits at the end of a v1 transaction.
    assert!(block::find_in_component(&c, &[0x11; 64]).unwrap().is_some());
}
