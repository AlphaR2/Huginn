//! Builds the SP1 program (`zk/program`) so that its ELF, and therefore the program key every
//! destination contract pins, is the same on every machine.
//!
//! **Default: SP1's official Docker image, pinned by digest.** A native build is not enough:
//! cargo mixes the host's compiler description (`rustc -vV`, which names the host, e.g.
//! `aarch64-apple-darwin`) into every crate's hash. Those hashes decide the order in which
//! functions and constants are laid out, so a Mac and a Linux server produce the same code in a
//! different order: same size, different bytes, different program key (seen 14 Nov 2026).
//! Inside one pinned linux/amd64 image, every machine runs the same compiler on the same host.
//!
//! Also, in both modes:
//!   - `--locked`: dependency versions come from the committed `zk/program/Cargo.lock`;
//!   - `--remap-path-prefix`: source paths in panic messages are rewritten to fixed names.
//!
//! `HUGINN_NATIVE_PROGRAM=1` builds with the local toolchain instead: faster, no Docker, but the
//! key then depends on the machine. Fine for development; never deploy with it.
//! `zk/check-program-key.sh` builds a second copy at another path and compares the ELFs.

use {
    sp1_build::{build_program_with_args, BuildArgs},
    std::path::{Path, PathBuf},
};

/// SP1 v6.10.0's build image. The digest pins the exact bytes, so a re-pushed tag can't change
/// the program key. Change both together when upgrading SP1.
const SP1_IMAGE_TAG: &str = "v6.10.0";
const SP1_IMAGE: &str =
    "ghcr.io/succinctlabs/sp1@sha256:9363a70004271de441e863612c7aefbbc5c36f371f111cd8abb1672fa5ea99f1";

fn main() {
    println!("cargo:rerun-if-env-changed=HUGINN_NATIVE_PROGRAM");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("../..").canonicalize().expect("huginn checkout root");
    let native = std::env::var("HUGINN_NATIVE_PROGRAM").is_ok_and(|v| v == "1");

    let args = if native {
        println!("cargo:warning=HUGINN_NATIVE_PROGRAM=1: the program key depends on this machine; don't deploy with it");
        BuildArgs { rustflags: remap_flags(&native_remaps(&root)), locked: true, ..Default::default() }
    } else {
        // sp1-build reads the image from SP1_DOCKER_IMAGE; this build script is single-threaded.
        std::env::set_var("SP1_DOCKER_IMAGE", SP1_IMAGE);
        BuildArgs {
            docker: true,
            tag: SP1_IMAGE_TAG.into(),
            // The program uses `crates/alpenglow-core`, outside its own workspace, so the
            // container gets the whole checkout (mounted at /root/program).
            workspace_directory: Some(root.display().to_string()),
            rustflags: remap_flags(&[
                (PathBuf::from("/root/program"), "/huginn".into()),
                (PathBuf::from("/root/.cargo"), "/cargo".into()),
            ]),
            locked: true,
            ..Default::default()
        }
    };
    build_program_with_args("../program", args);
}

/// This machine's paths that end up in panic messages, mapped to fixed names.
fn native_remaps(root: &Path) -> Vec<(PathBuf, String)> {
    let cargo_home = std::env::var("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").expect("HOME")).join(".cargo"));
    let mut remaps: Vec<(PathBuf, String)> = Vec::new();
    // Each registry index has its own folder (index.crates.io-<hash>); map them all.
    if let Ok(dirs) = std::fs::read_dir(cargo_home.join("registry/src")) {
        for d in dirs.flatten() {
            remaps.push((canonical(&d.path()), "/cargo/registry".into()));
        }
    }
    remaps.push((canonical(&cargo_home.join("git/checkouts")), "/cargo/git".into()));
    remaps.push((root.to_path_buf(), "/huginn".into()));
    remaps
}

fn remap_flags(remaps: &[(PathBuf, String)]) -> Vec<String> {
    remaps.iter().map(|(from, to)| format!("--remap-path-prefix={}={to}", from.display())).collect()
}

fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}
