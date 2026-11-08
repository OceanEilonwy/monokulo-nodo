#!/bin/sh
# Puts what tests/acme-e2e.sh needs into the running dev VM: Pebble and pebble-challtestsrv at a
# pinned release, checked against their published SHA-256, the fake DNS provider, curl and python3.
# Runs on the host. Pebble's release binaries are x86_64, so this is for the x86-kvm board.
#   tests/acme-prepare.sh && scripts/dev-vm.sh ssh 'cat > /tmp/acme-e2e.sh' < tests/acme-e2e.sh
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
PEBBLE_VERSION=v2.10.1
PEBBLE_SHA256=4f2fcb5bca8c85c9cf73ad140fccfc0d2be40bd81ab99879c79b7b8a0b4f70ed
CHALLTESTSRV_SHA256=e93a5aa25ecdf3af2f9fbb2de32b0173e64a2eae81002a4ccfe35fa6f4f60b92
cache=$root/build/pebble-$PEBBLE_VERSION
mkdir -p "$cache"

fetch() {
	name=$1; sum=$2
	file=$cache/$name-linux-amd64.tar.gz
	if ! printf '%s  %s\n' "$sum" "$file" | sha256sum -c - >/dev/null 2>&1; then
		curl -fsSL -o "$file.part" "https://github.com/letsencrypt/pebble/releases/download/$PEBBLE_VERSION/$name-linux-amd64.tar.gz"
		printf '%s  %s\n' "$sum" "$file.part" | sha256sum -c - >/dev/null || { echo "checksum mismatch: $name" >&2; rm -f "$file.part"; exit 1; }
		mv "$file.part" "$file"
	fi
	tar -xzf "$file" -C "$cache" --strip-components=3 "$name-linux-amd64/linux/amd64/$name"
}
fetch pebble "$PEBBLE_SHA256"
fetch pebble-challtestsrv "$CHALLTESTSRV_SHA256"

vm() { "$root/scripts/dev-vm.sh" "$@"; }
vm mirror curl python3
vm ssh 'mkdir -p /tmp/acme'
for f in pebble pebble-challtestsrv; do
	vm ssh "cat > /tmp/acme/$f && chmod 755 /tmp/acme/$f" < "$cache/$f"
done
vm ssh 'cat > /tmp/acme/fake-dns.py' < "$root/tests/acme/fake-dns.py"
vm ssh "/tmp/acme/pebble -version 2>&1 | head -1"
