#!/bin/sh
# Proves disk-full protection in the KVM dev VM with a real cuprated.
# Usage: tests/health/disk-full.sh   (after scripts/dev-vm.sh up)
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
cargo build -q --release --target x86_64-unknown-linux-musl -p nodo-health
vm() { "$root/scripts/dev-vm.sh" ssh "$@"; }
vm 'cat > /usr/local/bin/nodo-healthd && chmod 755 /usr/local/bin/nodo-healthd' < target/x86_64-unknown-linux-musl/release/nodo-healthd
vm 'cat > /tmp/disk-full.sh' < tests/health/_disk-full.sh
vm 'sh /tmp/disk-full.sh </dev/null; rc=$?; rm -f /tmp/disk-full.sh /usr/local/bin/nodo-healthd; exit $rc'
