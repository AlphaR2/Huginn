//! A repair client: one throwaway identity, one UDP socket, one peer at a time.

use {
    crate::wire::{self, Body, Hash, Pubkey, Reply},
    anyhow::{bail, Context, Result},
    solana_keypair::Keypair,
    solana_signer::Signer,
    std::{
        fs::File,
        io::Read,
        net::{SocketAddr, UdpSocket},
        time::{Duration, SystemTime, UNIX_EPOCH},
    },
};

/// How long we wait for one answer before asking again.
const WAIT: Duration = Duration::from_millis(1500);
/// Asks per request. A first ask to a new peer is answered with a ping, so two are normal.
const TRIES: usize = 6;

pub struct Client {
    socket: UdpSocket,
    keypair: Keypair,
    me: Pubkey,
    peer: SocketAddr,
    peer_id: Pubkey,
    nonce: u32,
}

impl Client {
    /// `peer` is the validator's `serveRepair` address and `peer_id` its identity, both from
    /// `getClusterNodes`. The identity key is random and used only for this run.
    pub fn new(peer: SocketAddr, peer_id: Pubkey) -> Result<Client> {
        let mut seed = [0u8; 32];
        File::open("/dev/urandom")
            .context("open /dev/urandom")?
            .read_exact(&mut seed)?;
        let keypair = Keypair::new_from_array(seed);
        let me = keypair.pubkey().to_bytes();
        let bind = if peer.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        };
        let socket = UdpSocket::bind(bind).context("bind UDP socket")?;
        socket.set_read_timeout(Some(WAIT))?;
        Ok(Client {
            socket,
            keypair,
            me,
            peer,
            peer_id,
            nonce: 1,
        })
    }

    pub fn identity(&self) -> Pubkey {
        self.me
    }

    fn sign(&self, msg: &[u8]) -> [u8; 64] {
        let sig = self.keypair.sign_message(msg);
        <[u8; 64]>::try_from(sig.as_ref()).expect("64-byte signature")
    }

    /// Sends one request and returns its answer, doing the ping/pong handshake if asked.
    fn ask(&mut self, body: &Body) -> Result<Reply> {
        self.nonce = self.nonce.wrapping_add(1);
        let nonce = self.nonce;
        let mut buf = vec![0u8; 2048];
        for _ in 0..TRIES {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
            let mut req = wire::request(body, &self.me, &self.peer_id, now, nonce);
            let sig = self.sign(&wire::signed_part(&req));
            wire::put_signature(&mut req, &sig);
            self.socket.send_to(&req, self.peer)?;
            // Read until the timeout: late answers to earlier asks are skipped by nonce.
            loop {
                let (n, from) = match self.socket.recv_from(&mut buf) {
                    Ok(v) => v,
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        break
                    }
                    Err(e) => return Err(e.into()),
                };
                if from != self.peer {
                    continue;
                }
                match wire::parse_reply(body, &buf[..n], nonce) {
                    Some(Reply::Ping { token }) => {
                        let hash = wire::pong_hash(&token);
                        let pong = wire::pong(&self.me, &hash, &self.sign(&hash));
                        self.socket.send_to(&pong, self.peer)?;
                        break; // ask again now that we are known
                    }
                    Some(reply) => return Ok(reply),
                    None => continue,
                }
            }
        }
        bail!("no answer from {} after {TRIES} tries (is the slot still held, and the block id right?)", self.peer)
    }

    fn shred(&mut self, body: Body) -> Result<Vec<u8>> {
        match self.ask(&body)? {
            Reply::Shred(s) => Ok(s),
            other => bail!("expected a shred, got {other:?}"),
        }
    }

    /// Data shred `index` of `slot`, by slot (no block id needed).
    pub fn window(&mut self, slot: u64, index: u64) -> Result<Vec<u8>> {
        self.shred(Body::WindowIndex {
            slot,
            shred_index: index,
        })
    }

    /// The highest data shred the peer holds for `slot` at or above `index`.
    pub fn highest(&mut self, slot: u64, index: u64) -> Result<Vec<u8>> {
        self.shred(Body::HighestWindowIndex {
            slot,
            shred_index: index,
        })
    }

    /// Data shred `index` of the block with this id.
    pub fn window_for_block(&mut self, slot: u64, index: u32, block_id: Hash) -> Result<Vec<u8>> {
        self.shred(Body::WindowIndexForBlockId {
            slot,
            shred_index: index,
            block_id,
        })
    }

    /// `(fec_set_count, parent_slot, parent_block_id, proof)` for a block.
    pub fn parent(&mut self, slot: u64, block_id: Hash) -> Result<(u32, u64, Hash, Vec<u8>)> {
        match self.ask(&Body::ParentAndFecSetCount { slot, block_id })? {
            Reply::Parent {
                fec_set_count,
                parent_slot,
                parent_block_id,
                proof,
            } => Ok((fec_set_count, parent_slot, parent_block_id, proof)),
            other => bail!("expected parent info, got {other:?}"),
        }
    }

    /// One FEC set's root (20-byte entry) and its proof against the block id.
    pub fn fec_root(
        &mut self,
        slot: u64,
        block_id: Hash,
        fec_set_index: u32,
    ) -> Result<([u8; 20], Vec<u8>)> {
        match self.ask(&Body::FecSetRoot {
            slot,
            block_id,
            fec_set_index,
        })? {
            Reply::FecRoot { root, proof } => Ok((root, proof)),
            other => bail!("expected a set root, got {other:?}"),
        }
    }
}
