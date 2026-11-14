#!/usr/bin/env bash
# Reproducibility check for the SP1 program (observer finding F11).
# The program key that destination contracts pin is a function of the program's ELF. This builds
# the ELF here and again from a copy of the sources at a different path, then compares them
# byte for byte. Same bytes = same program key, independent of where the code is checked out.
#
#   zk/check-program-key.sh
#
# Needs the SP1 toolchain (sp1up --version v6.10.0). Takes a few minutes: the copy builds in
# its own target folder.
set -euo pipefail
here="$(cd "$(dirname "$0")/.." && pwd)"
elf=zk/program/target/elf-compilation/riscv64im-succinct-zkvm-elf/release/huginn-program
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
copy="$work/some/other/place/huginn"
mkdir -p "$copy"

echo "1/3 building the program here ($here)"
(cd "$here/zk/prover" && cargo build --release --quiet)

echo "2/3 building it again from a copy at $copy"
rsync -a --exclude target --exclude .git "$here/crates" "$here/zk" "$here/Cargo.toml" "$here/Cargo.lock" "$copy/"
(cd "$copy/zk/prover" && CARGO_TARGET_DIR="$work/target" cargo build --release --quiet)

echo "3/3 comparing"
a="$(shasum -a 256 "$here/$elf" | cut -d' ' -f1)"
# sp1-build writes the program into the cargo target folder, which is CARGO_TARGET_DIR for the copy.
b="$(shasum -a 256 "$work/target/${elf#zk/program/target/}" | cut -d' ' -f1)"
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
