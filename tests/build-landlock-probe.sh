#!/bin/sh
# Builds nodo-confine and its integration tests as static aarch64 binaries, for
# tests/kernel-checks.sh to run inside the emulated machine. The tests start the real
# nodo-confine and try to reach an abstract Unix socket from inside its sandbox.
# Needs the Rust target: rustup target add aarch64-unknown-linux-musl
# Usage: tests/build-landlock-probe.sh
set -eu
. "$(dirname "$0")/../scripts/lib.sh"
out=$root/build/landlock-probe
# The test binary has nodo-confine's absolute path compiled in, so the guest gets the same path.
export CARGO_TARGET_DIR=$out/target
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
cd "$root"
test_bin=$(cargo test -q -p nodo-confine --release --target aarch64-unknown-linux-musl --test confine --no-run \
	--message-format=json | python3 -I -c '
import json, sys
for line in sys.stdin:
    m = json.loads(line)
    t = m.get("target", {})
    if m.get("executable") and t.get("name") == "confine" and "test" in t.get("kind", []):
        print(m["executable"])')
[ -x "$test_bin" ] || { echo "the nodo-confine test binary was not built" >&2; exit 1; }
confine=$CARGO_TARGET_DIR/aarch64-unknown-linux-musl/release/nodo-confine
[ -x "$confine" ] || { echo "nodo-confine was not built" >&2; exit 1; }
cp "$test_bin" "$out/confine-test"
cp "$confine" "$out/nodo-confine"
echo "$confine" > "$out/confine-path.txt"
echo "built $out/confine-test and $out/nodo-confine"
