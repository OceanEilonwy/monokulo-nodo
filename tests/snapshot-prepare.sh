#!/bin/sh
# Builds test snapshots with scripts/publish-snapshot.sh and throwaway keys, and puts them into the
# running dev VM for tests/snapshot-update.sh. Runs on the host, after nodo-tools and the web
# packages were pushed, because the snapshots carry those same packages.
#
#   test-a  serial 5: nodo-snaptest 1.0 and this project's packages, over Void's index
#   test-b  serial 6: nodo-snaptest 1.1, the update
#   test-c  serial 6 again, with different contents: a conflicting manifest
#   test-e  serial 7: test-a's packages again, withdrawing nothing, which would move a package back
#   test-d  serial 8: test-a's packages again, withdrawing test-b
#   attacker/  test-b's manifest signed by a key the device does not trust
#
# Void's index is the one the VM was built from, read back from its xbps cache, so nothing is
# downloaded and no Void package changes. No real key is used: every key is made here and kept
# under build/snapshot-test.
# scripts/test.sh vm runs it before the VM checks, which then run tests/snapshot-update.sh.
# By hand:
#   tests/snapshot-prepare.sh && scripts/dev-vm.sh push nodo-tools nodo-caddy nodo-localnet nodo-ddns
#   scripts/dev-vm.sh ssh 'cat > /tmp/snapshot-update.sh' < tests/snapshot-update.sh
#   scripts/dev-vm.sh ssh 'sh /tmp/snapshot-update.sh </dev/null'
set -eu
. "$(dirname "$0")/../scripts/lib.sh"
arch=x86_64
work=$root/build/snapshot-test
rm -rf "$work"
mkdir -p "$work/keys" "$work/local" "$work/packages"
export SNAPSHOT_ROOT="$work/server"
publish() { "$root/scripts/publish-snapshot.sh" "$@"; }
snapshot() { cargo run -q --release --manifest-path "$root/Cargo.toml" -p nodo-update --bin nodo-snapshot -- "$@"; }
vm() { "$root/scripts/dev-vm.sh" "$@"; }

snapshot keygen "$work/keys/snapshot.key" > "$work/keys/snapshot.pub"
snapshot keygen "$work/keys/attacker.key" > "$work/keys/attacker.pub"

ensure_builder
# The xbps key, and a system package with a service, at two versions.
docker run --rm --name nodo-snapshot-prepare -e "HOST_UID=$(id -u)" -e "HOST_GID=$(id -g)" -v "$work:/w" "$BUILDER" sh -euc '
	trap "chown -R $HOST_UID:$HOST_GID /w" EXIT
	openssl genrsa -out /w/keys/xbps.pem 2048 2>/dev/null
	for ver in 1.0 1.1; do
		d=/tmp/snaptest-$ver
		mkdir -p "$d/etc/sv/nodo-snaptest"
		printf "#!/bin/sh\nexec 2>&1\nexec </dev/null\necho %s > /run/nodo-snaptest.version\nexec sleep 1000000\n" "$ver" > "$d/etc/sv/nodo-snaptest/run"
		chmod 755 "$d/etc/sv/nodo-snaptest/run"
		(cd /w/packages && xbps-create -A x86_64 -n "nodo-snaptest-${ver}_1" -s "snapshot test service" "$d" >/dev/null)
	done'

void_dir=$(printf %s "$VOID_MIRROR/current" | tr './:' '___')
vm ssh "cat /var/db/xbps/$void_dir/$arch-repodata" > "$work/void-repodata"
[ -s "$work/void-repodata" ] || { echo "the VM has no cached Void index at $void_dir" >&2; exit 1; }

# This project's packages as pushed to the VM, so they are already at the snapshot's versions.
mkdir -p "$work/a" "$work/b"
for p in nodo-tools nodo-caddy nodo-ddns nodo-localnet; do
	cp "$root/out/feed/$arch/$p"-[0-9]*."$arch".xbps "$work/a/"
	cp "$root/out/feed/$arch/$p"-[0-9]*."$arch".xbps "$work/b/"
done
cp "$work/packages/nodo-snaptest-1.0_1.$arch.xbps" "$work/a/"
cp "$work/packages/nodo-snaptest-1.1_1.$arch.xbps" "$work/b/"
cp "$work/packages/nodo-snaptest-1.0_1.$arch.xbps" "$work/local/"

for s in a b c d e; do publish import "test-$s" "$arch" "$work/void-repodata"; done
publish nodo test-a "$arch" "$work/keys/xbps.pem" "$work/a"
publish nodo test-b "$arch" "$work/keys/xbps.pem" "$work/b"
publish nodo test-c "$arch" "$work/keys/xbps.pem" "$work/b"
publish nodo test-d "$arch" "$work/keys/xbps.pem" "$work/a"
publish nodo test-e "$arch" "$work/keys/xbps.pem" "$work/a"
publish sign test-a test 5 "$work/keys/snapshot.key" no --untested
publish sign test-b test 7 "$work/keys/attacker.key" no --untested
mkdir -p "$work/server/attacker"
mv "$work/server/snapshots/test-b/manifest.toml" "$work/server/snapshots/test-b/manifest.toml.sig" "$work/server/attacker/"
publish sign test-b test 6 "$work/keys/snapshot.key" no --untested
publish sign test-c test 6 "$work/keys/snapshot.key" yes --untested
publish sign test-e test 7 "$work/keys/snapshot.key" no --untested
# The publisher pulls test-b: test-a's packages again, under a newer serial, withdrawing it.
publish sign test-d test 8 "$work/keys/snapshot.key" no --untested --withdraws test-b
publish publish test-a test

printf 'channel = "test"\nkeys = ["%s"]\nthreshold = 1\nmin_serial = 4\n' "$(cat "$work/keys/snapshot.pub")" > "$work/pins.toml"
tar -C "$work" -cf - server local pins.toml | vm ssh 'rm -rf /srv/snapshot-test && mkdir -p /srv/snapshot-test && tar -C /srv/snapshot-test -xf -'
echo "snapshots test-a to test-e are in the VM at /srv/snapshot-test"
