#!/bin/sh
# Freezes Void's repository for devices, in three explicit steps. Nothing is signed until the
# snapshot has been built and tested.
#
#   scripts/publish-snapshot.sh fetch <name> <arch>...   copy repodata and the packages the image needs
#   scripts/publish-snapshot.sh test  <name> <board>     build an image from only the snapshot
#   scripts/publish-snapshot.sh sign  <name> <channel> <serial> <secret-key> [needs-restart]
#
# The snapshot lands in out/snapshots/<name>/current/<arch>, laid out like a Void mirror, so the
# image builder can use it as VOID_MIRROR. Upload out/snapshots/<name> and manifest.toml{,.sig}
# to the update server afterwards. The secret key should live offline; sign on that machine.
set -eu
. "$(dirname "$0")/lib.sh"
cmd=${1:?usage: publish-snapshot.sh fetch|test|sign <name> ...}
name=${2:?snapshot name, for example 2026.12}
case $name in *[!A-Za-z0-9._-]* | .* | -*) echo "bad snapshot name: $name" >&2; exit 2 ;; esac
[ $# -ge 2 ] && shift 2
snap=$root/out/snapshots/$name
stamp=$snap/tested

repodir() { case $1 in aarch64) echo "$snap/current/aarch64" ;; *) echo "$snap/current" ;; esac; }
upstream() { case $1 in aarch64) echo "$VOID_MIRROR/current/aarch64" ;; *) echo "$VOID_MIRROR/current" ;; esac; }

case $cmd in
fetch)
	[ $# -gt 0 ] || { echo "name at least one architecture" >&2; exit 2; }
	ensure_builder
	for arch in "$@"; do
		dir=$(repodir "$arch")
		mkdir -p "$dir"
		# The closure of everything an image installs. Boards may add packages; list them here too.
		pkgs="monokulo-base cuprated-service busybox-static linux linux6.18 linux-firmware-network dracut zstd"
		docker run --rm --name "nodo-snapshot-fetch-$name-$arch" \
			-e "ARCH=$arch" -e "UP=$(upstream "$arch")" -e "PKGS=$pkgs" -e "HOST_UID=$(id -u)" -e "HOST_GID=$(id -g)" \
			-v "$root:/src" -v "$dir:/snap" "$BUILDER" sh -euc '
				trap "chown -R $HOST_UID:$HOST_GID /snap" EXIT
				cd /snap
				curl -fsSO "$UP/$ARCH-repodata"
				mkdir -p /tmp/r/var/db/xbps/keys /tmp/cache
				cp /var/db/xbps/keys/* /tmp/r/var/db/xbps/keys/
				# Download only: xbps checks every package against the copied, Void-signed index.
				XBPS_TARGET_ARCH=$ARCH xbps-install -S -D -y -c /tmp/cache -r /tmp/r \
					-R /src/out/feed/$ARCH -R /snap $PKGS >/dev/null || true
				XBPS_TARGET_ARCH=$ARCH xbps-install -S -D -y -c /tmp/cache -r /tmp/r -R "$UP" $PKGS >/dev/null
				cp /tmp/cache/*.xbps /tmp/cache/*.sig2 /snap/ 2>/dev/null || true'
		sha256sum "$dir/$arch-repodata" | cut -d" " -f1 > "$dir/repodata.sha256"
		echo "fetched $arch: $(find "$dir" -name '*.xbps' | wc -l) packages"
	done
	rm -f "$stamp"
	;;
test)
	board=${1:?board to test, for example qemu-virt}
	. "$root/boards/$board/board.env"
	[ -s "$(repodir "$BOARD_ARCH")/$BOARD_ARCH-repodata" ] || { echo "fetch $BOARD_ARCH first" >&2; exit 1; }
	ensure_builder
	set --
	[ "${BOARD_NEEDS_KVM:-}" = 1 ] && set -- --device /dev/kvm
	# The same build as scripts/build-image.sh, with the snapshot as the only Void repository.
	docker run --rm --name "nodo-snapshot-test-$name-$board" \
		-e "BOARD=$board" -e "VOID_MIRROR=/src/out/snapshots/$name" -e "HOST_UID=$(id -u)" -e "HOST_GID=$(id -g)" \
		-v "$root:/src" "$@" "$BUILDER" /src/scripts/_build-image-inner.sh
	if [ "$board" = qemu-virt ]; then
		"$root/tests/qemu-boot-test.sh" qemu-virt
	fi
	sha256sum "$(repodir "$BOARD_ARCH")/$BOARD_ARCH-repodata" | cut -d" " -f1 >> "$stamp"
	echo "snapshot $name passed on $board"
	;;
sign)
	channel=${1:?channel}
	serial=${2:?serial, higher than any published before}
	key=${3:?secret key file}
	restart=${4:-no}
	[ -s "$stamp" ] || { echo "snapshot $name has not passed a test run" >&2; exit 1; }
	set --
	for dir in "$snap/current" "$snap/current/aarch64"; do
		for index in "$dir"/*-repodata; do
			[ -f "$index" ] || continue
			arch=${index##*/}; arch=${arch%-repodata}
			# Refuse to sign an index that changed after its test run.
			grep -qx "$(sha256sum "$index" | cut -d" " -f1)" "$stamp" || { echo "$arch was not tested as it is now" >&2; exit 1; }
			rel=${dir#"$root/out/"}
			set -- "$@" "$arch=$rel=$index"
		done
	done
	[ $# -gt 0 ] || { echo "nothing to sign" >&2; exit 1; }
	cd "$root"
	cargo run -q --release -p nodo-update --bin nodo-snapshot -- manifest "$channel" "$serial" "$name" 30 "$restart" "$@" > "$snap/manifest.toml"
	rm -f "$snap/manifest.toml.sig"
	cargo run -q --release -p nodo-update --bin nodo-snapshot -- sign manifest "$snap/manifest.toml" "$key"
	echo "signed $snap/manifest.toml"
	;;
*) echo "unknown command: $cmd" >&2; exit 2 ;;
esac
