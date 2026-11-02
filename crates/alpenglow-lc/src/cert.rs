//! Certificate verification: a thin layer over Agave's `verify_certificate`.
//!
//! We don't reimplement the check. Agave's function decodes the bitmap, aggregates the
//! signers' keys, verifies the BLS signature, then checks the stake threshold. Our job is
//! to feed it the right inputs: the certificate bytes, and a rank map built from the
//! epoch's `ValidatorSet`.

use {
    crate::validator_set::{RankedValidator, ValidatorSet},
    agave_bls_cert_verify::cert_verify::{self, verify_certificate},
    agave_votor_messages::{
        certificate::{Certificate, CertificateType},
        consensus_message::Block,
        unverified_vote_message::UnverifiedCertificate,
    },
    serde::Deserialize,
    solana_bls_signatures::{
        pubkey::{PopVerified, PubkeyAffine, PubkeyCompressed},
        Signature,
    },
    solana_hash::Hash,
    std::num::NonZero,
};

pub use agave_bls_cert_verify::cert_verify::Error as AgaveError;

/// A BLS public key in the form Agave's verifier takes.
pub type PopKey = PopVerified<PubkeyAffine>;

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("validator set has zero total stake")]
    EmptySet,
    #[error("signature must be 192 bytes, got {0}")]
    SignatureLength(usize),
    #[error(transparent)]
    Cert(#[from] AgaveError),
}

/// The `getAgGenesisCert` RPC result. Byte fields come as JSON arrays of numbers.
#[derive(Clone, Debug, Deserialize)]
pub struct RpcGenesisCert {
    pub block: RpcBlock,
    pub signature: RpcSignature,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RpcBlock {
    pub slot: u64,
    #[serde(rename = "blockId")]
    pub block_id: [u8; 32],
}

#[derive(Clone, Debug, Deserialize)]
pub struct RpcSignature {
    pub signature: Vec<u8>,
    pub bitmap: Vec<u8>,
}

impl RpcGenesisCert {
    /// The certificate as Agave's verifier takes it. `shred_version` isn't in the RPC
    /// response but is part of every signed payload, so the caller supplies it.
    pub fn to_unverified(&self, shred_version: u16) -> Result<UnverifiedCertificate, Error> {
        let sig: [u8; 192] = self
            .signature
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| Error::SignatureLength(self.signature.signature.len()))?;
        Ok(UnverifiedCertificate {
            cert_type: CertificateType::Genesis(Block {
                slot: self.block.slot,
                block_id: Hash::new_from_array(self.block.block_id),
            }),
            signature: Signature(sig),
            bitmap: self.signature.bitmap.clone(),
            shred_version,
        })
    }
}

impl RankedValidator {
    /// The BLS key as Agave's verifier takes it.
    ///
    /// Trust assumption: proof of possession was checked by the vote program when the key
    /// was registered, so we skip it here. Agave does the same in `epoch_stakes.rs`.
    pub fn pop_verified_key(&self) -> Option<PopKey> {
        let affine = PubkeyAffine::try_from(PubkeyCompressed(self.bls_pubkey)).ok()?;
        Some(unsafe { PopVerified::new_unchecked(affine) })
    }
}

/// Verifies `cert` against the validator set of the epoch it was produced in.
pub fn verify(cert: UnverifiedCertificate, set: &ValidatorSet) -> Result<Certificate, Error> {
    let total_stake = NonZero::new(set.total_stake).ok_or(Error::EmptySet)?;
    verify_with(cert, set.entries.len(), total_stake, |rank| {
        let e = set.entries.get(rank)?;
        Some((NonZero::new(e.stake)?, e.pop_verified_key()?))
    })
}

/// Same as [`verify`], with the rank map given directly. Useful when the set is
/// reconstructed rather than read from one epoch (see the devnet genesis test).
pub fn verify_with(
    cert: UnverifiedCertificate,
    max_validators: usize,
    total_stake: NonZero<u64>,
    rank_map: impl FnMut(usize) -> Option<(NonZero<u64>, PopKey)>,
) -> Result<Certificate, Error> {
    // `None`: no thread pool, so the sequential path. Same result, no rayon at runtime.
    Ok(verify_certificate(cert, max_validators, total_stake, rank_map, None)?)
}

/// Re-exported so callers can match on signature failures without naming Agave's crate.
pub use cert_verify::Error::{MissingRank, NotEnoughStake, VerifySig};

#[cfg(test)]
mod tests {
    use {
        super::*,
        agave_votor_messages::wire::get_vote_payload_to_sign,
        solana_bls_signatures::{keypair::Keypair, BlsError, SignatureProjective},
    };

    const SHRED: u16 = 7016;

    /// signer-store base2: version 0 | bit count u16 LE | bits, LSB first.
    fn base2(ranks: &[usize], bits: usize) -> Vec<u8> {
        let mut out = vec![0u8, bits as u8, (bits >> 8) as u8];
        out.resize(3 + bits.div_ceil(8), 0);
        for &r in ranks {
            out[3 + r / 8] |= 1 << (r % 8);
        }
        out
    }

    /// 10 validators with stakes 100, 90, ..., 10 (already in rank order). Total 550.
    fn setup() -> (Vec<Keypair>, ValidatorSet) {
        let kps: Vec<Keypair> = (0..10).map(|i| Keypair::derive(&[i + 1; 32]).unwrap()).collect();
        let entries = kps
            .iter()
            .enumerate()
            .map(|(i, kp)| RankedValidator {
                bls_pubkey: PubkeyCompressed::from(&*kp.public).0,
                stake: 100 - 10 * i as u64,
                vote_pubkey: [i as u8; 32],
                node_pubkey: [i as u8; 32],
            })
            .collect();
        (kps, ValidatorSet { epoch: 1, total_stake: 550, entries })
    }

    fn genesis(slot: u64) -> CertificateType {
        CertificateType::Genesis(Block { slot, block_id: Hash::new_from_array([7; 32]) })
    }

    /// A real certificate: each signer signs Agave's payload, signatures are summed.
    fn signed(kps: &[Keypair], cert_type: CertificateType, signers: &[usize]) -> UnverifiedCertificate {
        let mut cert = UnverifiedCertificate {
            cert_type,
            signature: Signature([0; 192]),
            bitmap: base2(signers, kps.len()),
            shred_version: SHRED,
        };
        let (payload, _) = cert.get_vote_payload();
        let sigs: Vec<_> = signers.iter().map(|&i| kps[i].sign(&payload)).collect();
        cert.signature = Signature::from(&SignatureProjective::aggregate(sigs.iter()).unwrap());
        cert
    }

    fn bad_sig() -> Error {
        Error::Cert(VerifySig(BlsError::VerificationFailed))
    }

    // Ranks 0..=8 hold 540 of 550 stake (98%), above the 82% genesis threshold.
    const SIGNERS: [usize; 9] = [0, 1, 2, 3, 4, 5, 6, 7, 8];

    #[test]
    fn valid_certificate_verifies() {
        let (kps, set) = setup();
        let cert = signed(&kps, genesis(5), &SIGNERS);
        assert_eq!(verify(cert, &set).unwrap().cert_type, genesis(5));
    }

    #[test]
    fn payload_matches_agave_encoder() {
        let (kps, _) = setup();
        let cert = signed(&kps, genesis(5), &SIGNERS);
        let (payload, fallback) = cert.get_vote_payload();
        assert_eq!(fallback, None);
        let block = Block { slot: 5, block_id: Hash::new_from_array([7; 32]) };
        let vote = agave_votor_messages::vote::Vote::new_genesis_vote(block);
        assert_eq!(payload, get_vote_payload_to_sign(vote, SHRED));
    }

    #[test]
    fn adding_a_non_signer_to_the_bitmap_fails() {
        let (kps, set) = setup();
        let mut cert = signed(&kps, genesis(5), &SIGNERS);
        cert.bitmap = base2(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9], 10);
        assert_eq!(verify(cert, &set).unwrap_err(), bad_sig());
    }

    #[test]
    fn removing_a_signer_from_the_bitmap_fails() {
        let (kps, set) = setup();
        let mut cert = signed(&kps, genesis(5), &SIGNERS);
        cert.bitmap = base2(&[0, 1, 2, 3, 4, 5, 6, 7], 10);
        assert_eq!(verify(cert, &set).unwrap_err(), bad_sig());
    }

    #[test]
    fn flipping_one_signature_bit_fails() {
        let (kps, set) = setup();
        let mut cert = signed(&kps, genesis(5), &SIGNERS);
        cert.signature.0[100] ^= 1;
        // A flipped bit usually leaves the bytes off the curve, so it fails at decoding
        // rather than at the pairing check. Either way it must not verify.
        assert!(matches!(verify(cert, &set).unwrap_err(), Error::Cert(VerifySig(_))));
    }

    #[test]
    fn different_slot_shred_version_or_type_fails() {
        let (kps, set) = setup();
        let good = signed(&kps, genesis(5), &SIGNERS);

        let mut c = good.clone();
        c.cert_type = genesis(6);
        assert_eq!(verify(c, &set).unwrap_err(), bad_sig());

        let mut c = good.clone();
        c.shred_version = SHRED + 1;
        assert_eq!(verify(c, &set).unwrap_err(), bad_sig());

        // Same block, but claimed as a Notarize cert: different tag byte, different payload.
        let mut c = good;
        c.cert_type = CertificateType::Notarize(Block { slot: 5, block_id: Hash::new_from_array([7; 32]) });
        assert_eq!(verify(c, &set).unwrap_err(), bad_sig());
    }

    #[test]
    fn genuine_signatures_without_enough_stake_fail() {
        let (kps, set) = setup();
        // Ranks 0..=6 hold 490 of 550 (89%): enough for genesis (82%).
        assert!(verify(signed(&kps, genesis(5), &[0, 1, 2, 3, 4, 5, 6]), &set).is_ok());
        // Ranks 0..=5 hold 450 of 550 (81.8%): every signature is real, still rejected.
        let err = verify(signed(&kps, genesis(5), &[0, 1, 2, 3, 4, 5]), &set).unwrap_err();
        assert!(matches!(err, Error::Cert(NotEnoughStake { aggregate_stake: 450, .. })));
    }

    #[test]
    fn wrong_validator_set_fails() {
        let (kps, mut set) = setup();
        let cert = signed(&kps, genesis(5), &SIGNERS);
        // Same keys, ranks shifted by one: the bitmap now points at the wrong people.
        set.entries.rotate_left(1);
        assert_eq!(verify(cert, &set).unwrap_err(), bad_sig());
    }

    #[test]
    fn bitmap_longer_than_set_is_rejected() {
        let (kps, mut set) = setup();
        let cert = signed(&kps, genesis(5), &SIGNERS);
        set.entries.truncate(8);
        assert!(matches!(verify(cert, &set).unwrap_err(), Error::Cert(AgaveError::Decode(_))));
    }
}
