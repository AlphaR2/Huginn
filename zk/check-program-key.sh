#!/usr/bin/env bash
# Reproducibility check for the SP1 program (observer finding F11).
# The program key that destination contracts pin is a function of the program's ELF. This builds
# the ELF here and again from a copy of the sources at a different path, then compares them
# byte for byte. Same bytes = same program key, independent of where the code is checked out.
#
#   zk/check-program-key.sh
#
# Builds in SP1's pinned Docker image (see zk/prover/build.rs), so Docker must be running.
# The same ELF hash must come out on every machine: compare the printed hash across machines.
# Takes a while: the copy builds the prover in its own target folder.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
elf=zk/program/target/elf-compilation/docker/riscv64im-succinct-zkvm-elf/release/huginn-program
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
copy="$work/some/other/place/huginn"
mkdir -p "$copy"

echo "1/3 building the program here ($here)"
(cd "$here/zk/prover" && cargo build --release --quiet)

echo "2/3 building it again from a copy at $copy"
rsync -a --exclude target --exclude .git "$here/crates" "$here/zk" "$here/Cargo.toml" "$here/Cargo.lock" "$copy/"
# No CARGO_TARGET_DIR: the Docker build needs the target folder inside the checkout it mounts.
(cd "$copy/zk/prover" && cargo build --release --quiet)

echo "3/3 comparing"
a="$(shasum -a 256 "$here/$elf" | cut -d' ' -f1)"
b="$(shasum -a 256 "$copy/$elf" | cut -d' ' -f1)"
echo "  here : $a"
echo "  copy : $b"
if grep -q "$HOME" "$here/$elf"; then
  echo "FAIL: the ELF still contains this machine's home path"; exit 1
fi
if [ "$a" = "$b" ]; then
  echo "PASS: identical ELF, so the same program key wherever the code is checked out"
else
  echo "FAIL: the ELFs differ"; exit 1
fi
