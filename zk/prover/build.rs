//! Builds the SP1 program (`zk/program`) so that its ELF, and therefore the program key every
//! destination contract pins, is the same on every machine:
//!   - `--locked`: dependency versions come from the committed `zk/program/Cargo.lock`;
//!   - `--remap-path-prefix`: the compiler embeds source paths in panic messages, which would
//!     put this machine's home folder and checkout path into the ELF. They are rewritten to
//!     fixed names (`/cargo/registry`, `/cargo/git`, `/huginn`).
//! What still has to match: the SP1 toolchain (`sp1up --version v6.10.0`). `zk/check-program-key.sh`
//! builds a second copy at another path and compares the ELFs.

use {
    sp1_build::{build_program_with_args, BuildArgs},
    std::path::{Path, PathBuf},
};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("../..").canonicalize().expect("huginn checkout root");
    let cargo_home = std::env::var("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").expect("HOME")).join(".cargo"));

    let mut remaps: Vec<(PathBuf, String)> = Vec::new();
    // Each registry index has its own folder (index.crates.io-<hash>); map them all.
    if let Ok(dirs) = std::fs::read_dir(cargo_home.join("registry/src")) {
        for d in dirs.flatten() {
            remaps.push((d.path(), "/cargo/registry".into()));
        }
    }
    remaps.push((cargo_home.join("git/checkouts"), "/cargo/git".into()));
    remaps.push((root.clone(), "/huginn".into()));

    let rustflags = remaps
        .iter()
        .map(|(from, to)| format!("--remap-path-prefix={}={to}", canonical(from).display()))
        .collect();
    build_program_with_args(
        "../program",
        BuildArgs { rustflags, locked: true, ..Default::default() },
    );
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}
