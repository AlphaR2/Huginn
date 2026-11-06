//! `alpenglow-core`'s transaction parser must accept exactly what Agave accepts
//! (solana-transaction 5.1.0: wincode `deserialize_exact`, the format block entries use, then
//! `sanitize`), and read the same fields and the same signed message. Legacy, v0 and v1.
//!
//! Why it matters: a transaction Agave accepts but Huginn refuses can sit in a finalized
//! block and make the transactions around it unprovable (review finding 4). One Huginn
//! accepts but Agave refuses can't be in a finalized block, but is checked anyway.
use alpenglow_core::block::{self, Component};
use solana_transaction::versioned::VersionedTransaction;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, k: u64) -> bool {
        self.below(k) == 0
    }
}

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

fn gen_v1(r: &mut Rng) -> Vec<u8> {
    let req = if r.chance(30) { 13 } else { r.below(4) as u8 };
    let ro_signed = if r.below(10) < 8 { 0 } else { r.below(3) as u8 };
    let ro_unsigned = r.below(4) as u8;
    let naddr = if r.chance(40) { 65 } else { r.below(8) as u8 };
    let nix = if r.chance(40) { 65 } else { r.below(4) as u8 };
    let mask: u32 = match r.below(10) {
        0 => 1 << r.below(32),
        1 => 0b01,
        2 => 0b10,
        _ => r.below(32) as u32,
    };
    let mut o = vec![0x81, req, ro_signed, ro_unsigned];
    o.extend_from_slice(&mask.to_le_bytes());
    o.extend_from_slice(&[9; 32]);
    o.push(nix);
    o.push(naddr);
    for k in 0..naddr {
        let b = if r.chance(10) { 0 } else { k + 1 };
        o.extend_from_slice(&[b; 32]);
    }
    if mask & 0b11 == 0b11 {
        o.extend_from_slice(&r.next().to_le_bytes());
    }
    for bit in [0b100u32, 0b1000] {
        if mask & bit != 0 {
            o.extend_from_slice(&(r.next() as u32).to_le_bytes());
        }
    }
    if mask & 0b10000 != 0 {
        let heap: u32 = match r.below(4) {
            0 => 32 * 1024,
            1 => 256 * 1024,
            2 => 1024 * r.below(300) as u32,
            _ => r.next() as u32,
        };
        o.extend_from_slice(&heap.to_le_bytes());
    }
    let mut payloads = Vec::new();
    for _ in 0..nix {
        let prog = r.below(naddr as u64 + 2) as u8;
        let na = r.below(4) as u8;
        let nd = if r.chance(8) { 300 } else { r.below(6) as u16 };
        o.extend_from_slice(&[prog, na]);
        o.extend_from_slice(&nd.to_le_bytes());
        for _ in 0..na {
            payloads.push(r.below(naddr as u64 + 2) as u8);
        }
        payloads.extend((0..nd).map(|_| r.next() as u8));
    }
    o.extend(payloads);
    let sigs = if r.below(10) < 9 {
        req as usize
    } else {
        r.below(4) as usize
    };
    for i in 0..sigs {
        o.extend_from_slice(&[i as u8 + 1; 64]);
    }
    o
}

fn gen_old(r: &mut Rng) -> Vec<u8> {
    let nkeys = r.below(7) as usize;
    let req = r.below(4) as usize;
    let sigs = if r.below(10) < 8 {
        req
    } else {
        r.below(4) as usize
    };
    let mut o = cu16(sigs);
    for i in 0..sigs {
        o.extend_from_slice(&[i as u8 + 1; 64]);
    }
    let v0 = r.below(2) == 0;
    if v0 {
        o.push(0x80);
    }
    o.extend_from_slice(&[
        req as u8,
        if r.below(10) < 8 { 0 } else { r.below(3) as u8 },
        r.below(4) as u8,
    ]);
    o.extend(cu16(nkeys));
    for k in 0..nkeys {
        o.extend_from_slice(&[k as u8 + 1; 32]);
    }
    o.extend_from_slice(&[9; 32]);
    let nix = r.below(4) as usize;
    o.extend(cu16(nix));
    for _ in 0..nix {
        o.push(r.below(nkeys as u64 + 2) as u8);
        let na = r.below(4) as usize;
        o.extend(cu16(na));
        for _ in 0..na {
            o.push(r.below(nkeys as u64 + 3) as u8);
        }
        let nd = r.below(5) as usize;
        o.extend(cu16(nd));
        o.extend((0..nd).map(|_| r.next() as u8));
    }
    if v0 {
        let nl = r.below(2) as usize;
        o.extend(cu16(nl));
        for _ in 0..nl {
            o.extend_from_slice(&[5; 32]);
            o.extend(cu16(1));
            o.push(0);
            o.extend(cu16(0));
        }
    }
    o
}

fn mutate(r: &mut Rng, o: &mut Vec<u8>) {
    if o.is_empty() {
        return;
    }
    let pos = r.below(o.len() as u64) as usize;
    match r.below(4) {
        0 => o[pos] ^= 1 << r.below(8),
        1 => o[pos] = r.next() as u8,
        2 => {
            o.remove(pos);
        }
        _ => o.insert(pos, r.next() as u8),
    }
}

type Fields = (
    Option<u8>,
    Vec<u8>,
    Vec<Vec<u8>>,
    Vec<Vec<u8>>,
    [u8; 3],
    Vec<u8>,
    Vec<(u8, Vec<u8>, Vec<u8>)>,
);

fn agave(b: &[u8]) -> Result<Fields, String> {
    let tx: VersionedTransaction =
        wincode::deserialize_exact(b).map_err(|e| format!("deser: {e}"))?;
    tx.sanitize().map_err(|e| format!("sanitize: {e:?}"))?;
    let m = &tx.message;
    let h = m.header();
    let version = match m {
        solana_message::VersionedMessage::Legacy(_) => None,
        solana_message::VersionedMessage::V0(_) => Some(0),
        _ => Some(1),
    };
    Ok((
        version,
        m.serialize(),
        tx.signatures.iter().map(|s| s.as_ref().to_vec()).collect(),
        m.static_account_keys()
            .iter()
            .map(|k| k.as_ref().to_vec())
            .collect(),
        [
            h.num_required_signatures,
            h.num_readonly_signed_accounts,
            h.num_readonly_unsigned_accounts,
        ],
        m.recent_blockhash().as_ref().to_vec(),
        m.instructions()
            .iter()
            .map(|i| (i.program_id_index, i.accounts.clone(), i.data.clone()))
            .collect(),
    ))
}

fn huginn(b: &[u8]) -> Result<Fields, String> {
    let mut c = 1u64.to_le_bytes().to_vec();
    c.extend_from_slice(&1u64.to_le_bytes());
    c.extend_from_slice(&[0; 32]);
    c.extend_from_slice(&1u64.to_le_bytes());
    c.extend_from_slice(b);
    match block::parse_component(&c).map_err(|e| format!("{e}"))? {
        Component::Entries(e) => {
            let t = &e[0].transactions[0];
            Ok((
                t.version,
                t.message.to_vec(),
                t.signatures.iter().map(|s| s.to_vec()).collect(),
                t.account_keys.iter().map(|k| k.to_vec()).collect(),
                [
                    t.num_required_signatures,
                    t.num_readonly_signed,
                    t.num_readonly_unsigned,
                ],
                t.recent_blockhash.to_vec(),
                t.instructions
                    .iter()
                    .map(|i| (i.program_id_index, i.accounts.to_vec(), i.data.to_vec()))
                    .collect(),
            ))
        }
        _ => Err("not entries".into()),
    }
}

#[test]
fn parser_agrees_with_agave_on_random_and_mutated_transactions() {
    let n = 300_000u32;
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut ok, mut v1_ok) = (0u32, 0u32);
    for i in 0..n {
        let mut b = if i % 3 == 0 {
            gen_old(&mut r)
        } else {
            gen_v1(&mut r)
        };
        if r.chance(4) {
            mutate(&mut r, &mut b);
        }
        match (agave(&b), huginn(&b)) {
            (Ok(a), Ok(h)) => {
                ok += 1;
                v1_ok += (a.0 == Some(1)) as u32;
                assert_eq!(a, h, "fields differ for {}", hex(&b));
            }
            (Err(_), Err(_)) => {}
            (Err(a), Ok(_)) => panic!("Huginn accepts what Agave refuses ({a}): {}", hex(&b)),
            (Ok(_), Err(h)) => panic!("Agave accepts what Huginn refuses ({h}): {}", hex(&b)),
        }
    }
    // The generator must keep reaching valid transactions of every kind.
    assert!(ok > 10_000 && v1_ok > 5_000, "accepted {ok}, v1 {v1_ok}");
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
