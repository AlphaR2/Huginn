# Running Huginn: locally, on a VPS for Groth16, and cleaning up after

Three parts:
1. **Local**: the whole flow on a laptop, with practice or real STARK proofs.
2. **VPS**: the one thing a laptop can't do, a Groth16 proof (needs ≥32 GB RAM).
3. **Cleanup**: everything the setup puts on your machine, and how to remove it.

---

## 1. Local

This is the one setup guide for both repos. The READMEs and `huginn-evm/docs/demo.md` point here.

### Tools

Install each once. The versions are the ones Huginn is tested with.

| Tool | Why | Install |
|---|---|---|
| Rust | everything | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| SP1 6.10.0 | proves (`cargo prove --version` must say 6.10.0) | `curl -L https://sp1up.succinct.xyz \| bash`, then `sp1up --version v6.10.0` |
| protoc | SP1 builds | `brew install protobuf` (Ubuntu: `apt install protobuf-compiler`) |
| Solana CLI, Agave 4.4.0 | the local Alpenglow validator, sending the memo | `sh -c "$(curl -sSfL https://release.anza.xyz/v4.4.0/install)"` |
| Foundry | contracts, anvil | `curl -L https://foundry.paradigm.xyz \| bash`, then `foundryup` |
| Docker | Groth16 only, and only with ≥32 GB RAM (part 2) | Docker Desktop |

Open a new terminal afterwards, so the new `PATH` entries apply.

### Steps

**1. Clone both repos next to each other.** The app finds huginn at `../huginn`:

```bash
git clone https://github.com/AlphaR2/huginn
git clone https://github.com/AlphaR2/huginn-evm
```

**2. Start the local Alpenglow validator** in `huginn`, and leave it running in its own terminal. Its ledger grows by a few GB a day:

```bash
cd huginn
solana-test-validator --alpenglow --ledger .local-ledger
```

The footer-dump plugin is **not** needed: certificates are read from the shreds. Only add `--geyser-plugin-config plugins/footer-dump/config.json` if you want to compare against the plugin (`s` in the TUI).

**3. Build and test huginn**, in another terminal:

```bash
cd huginn
cargo build --release -p ledger-dump
cargo test --release
```

**4. Build and test huginn-evm:**

```bash
cd huginn-evm
forge install foundry-rs/forge-std --no-git
forge test
cd app && cargo build --release && cd ..
```

`.env` is optional on the local chain: the app reads `.env.example`, and with no `PRIVATE_KEY` it uses anvil's public test key, only on chain 31337.

**5. Check the setup**, from `huginn-evm`. It lists every tool it needs and prints the fix for anything missing:

```bash
./app/target/release/huginn-evm run doctor
```

**6. Run the TUI:**

```bash
./app/target/release/huginn-evm
```

`e` writes a message, `enter` runs it, `d` shows details, `?` shows help, and `m` switches between practice and real (STARK) proofs. What you should see at each step: `huginn-evm/docs/demo.md`.

### A fresh validator is a new chain

The `fixtures/local` files in huginn (validator set, shred version) belong to **this repo's** local chain. A fresh `.local-ledger` makes a new chain with its own keys, so refresh them once it's running:

```bash
cargo run --release -p fetch -- cluster-info --cluster local
cargo run --release -p fetch -- validator-set --cluster local
```

Then copy the new `SHRED_VERSION` and `VALIDATOR_SET_HASH` into `huginn-evm/.env`.

---

## 2. A Groth16 proof on a VPS (one hour, then delete the VPS)

**Why:** gnark loads a 5.9 GB proving key plus a 2.4 GB circuit, and needs working space on top. Succinct recommends 32 GB of RAM; on an 18 GB laptop it is killed (see `docs/findings.md`). A VPS with 32 GB does it, and you delete it after.

**Machine:** Ubuntu 24.04, **≥32 GB RAM, ≥8 vCPUs, ≥100 GB disk** (circuits 14 GB + builds ~12 GB + Docker). Any provider works. Examples:
- Hetzner Cloud CCX33: 8 dedicated vCPU, 32 GB;
- AWS r7i.xlarge or bigger;
- DigitalOcean memory-optimized 32 GB.

Billing is hourly; check the current price. This needs about an hour.

### On the VPS

**1. System packages and Docker:**

```bash
sudo apt update && sudo apt install -y build-essential pkg-config libssl-dev protobuf-compiler clang git curl docker.io
sudo usermod -aG docker $USER && newgrp docker
```

**2. Rust:**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env
```

**3. SP1.** Huginn pins v6.10.0; `cargo prove --version` must say 6.10.0:

```bash
curl -L https://sp1up.succinct.xyz | bash
source ~/.bashrc
sp1up --version v6.10.0
cargo prove --version
```

**4. The code.** Either clone (needs GitHub access if the repo is private):

```bash
git clone https://github.com/AlphaR2/huginn
```

Or copy it from your laptop. Run this on the laptop, from the folder holding `huginn`:

```bash
rsync -az --exclude target --exclude .local-ledger --exclude fixtures/local/footers.jsonl huginn/ root@<vps-ip>:huginn/
```

**5. Make the proof.** On first use SP1 downloads the circuit files (~6 GB) and the gnark Docker image by itself. Expect about 10 minutes for the STARK, then several minutes in gnark:

```bash
cd huginn/zk/prover
cargo run --release -- --prove groth16
```

Success ends with a line like:

```
groth16 proof: setup …s, prove …s, verify …s -> ../../fixtures/proofs/real-1-validator-fast.groth16.json
```

### Back on your laptop

**1. Fetch the proof summary.** It holds the program key, the public values and the ~260-byte proof:

```bash
scp root@<vps-ip>:huginn/fixtures/proofs/real-1-validator-fast.groth16.json huginn/fixtures/proofs/
cp huginn/fixtures/proofs/real-1-validator-fast.groth16.json huginn-evm/test/fixtures/
```

**2. Check it with SP1's real Solidity verifier.** This prints the real gas cost:

```bash
cd huginn-evm
forge test -vv --match-contract Groth16
```

**3. Delete the VPS** in your provider's console. Nothing on it is needed afterwards.

The same proof file is what step 7.3 (Base Sepolia) submits: deploy `SolanaLightClient` against Succinct's verifier there with this proof's `program_vkey`.

---

## 3. Cleanup: what the setup put on your machine

Do this only after both repos are pushed and you're done testing. Sizes are what this project's Mac had on 2026-11-04 (about 40 GB in all).

### Stop what's running

The validator: Ctrl-C in its terminal, or:

```bash
pkill -f solana-test-validator
```

anvil, which the TUI may have left running:

```bash
pkill anvil
```

### Inside the repos

Build outputs (huginn ~9.4 GB, huginn-evm ~2.4 GB):

```bash
cd huginn && cargo clean && (cd zk/prover && cargo clean) && (cd zk/program && cargo clean)
cd ../huginn-evm && (cd app && cargo clean) && forge clean && rm -rf lib broadcast proofs/*.json
```

The validator's ledger (5.4 GB) and the plugin's footer log (80 MB):

```bash
rm -rf huginn/.local-ledger huginn/fixtures/local/footers.jsonl
```

Or remove the clones entirely, once they're pushed:

```bash
rm -rf huginn huginn-evm
```

### Tools in your home folder

| What | Size | Remove with |
|---|---:|---|
| SP1 Groth16 circuit files + archive | 14 GB | `rm -rf ~/.sp1/circuits` |
| SP1 entirely (toolchain + circuits) | 15 GB | `rm -rf ~/.sp1`, then delete the sp1 `PATH` line in `~/.zshenv` |
| Rust's download caches (re-downloaded when needed) | 2.9 GB | `rm -rf ~/.cargo/registry ~/.cargo/git` |
| Rust toolchains | 2.7 GB | `rustup self uninstall` (removes Rust completely) |
| Foundry | 0.3 GB | `rm -rf ~/.foundry`, then delete its `PATH` line in `~/.zshenv` |
| Solana CLI | 0.6 GB | `rm -rf ~/.local/share/solana ~/.config/solana` (the keypair there only holds local test SOL) |

### Docker

The gnark image (~0.8 GB):

```bash
docker rmi ghcr.io/succinctlabs/sp1-gnark:v6.1.0
```

Everything Docker holds that isn't in use: stopped containers, unused images, **all** unused volumes (~3 GB here). Only do this if nothing else you care about lives in Docker:

```bash
docker system prune -a --volumes
```

Then in Docker Desktop, set Settings → Resources → Memory back to what it was.
