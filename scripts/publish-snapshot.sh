#!/bin/sh
# Builds the static files of the system update server, in explicit steps. Nothing is signed until
# the snapshot has been built and tested.
#
#   scripts/publish-snapshot.sh fetch  <name> <arch>...          copy Void's repodata and the packages images need
#   scripts/publish-snapshot.sh import <name> <arch> <repodata>  use a Void index fetched elsewhere, without packages
#   scripts/publish-snapshot.sh nodo   <name> <arch> <xbps-key.pem> [package-dir]
#                                                                 add this project's packages, signed for xbps
#   scripts/publish-snapshot.sh test   <name> <board>            build an image from only the snapshot
#   scripts/publish-snapshot.sh sign   <name> <channel> <serial> <secret-key> [yes|no] [--untested] [--withdraws <name>]...
#   scripts/publish-snapshot.sh publish <name> <channel>         make it the channel's newest snapshot
#
# The server's files land under $SNAPSHOT_ROOT (default out/):
#
#   snapshots/<name>/current[/aarch64]   Void's repository, exactly as Void signed it
#   snapshots/<name>/nodo/<arch>         this project's packages, signed with the owner's xbps key
#   snapshots/<name>/manifest.toml{,.sig}
#   channels/<channel>/manifest.toml{,.sig}   copied there by publish
#
# Serve that directory as static files over HTTPS. Devices read channels/<channel>/manifest.toml
# and everything else by the paths it names. See docs/UPDATES.md for the keys: the snapshot key
# (Ed25519, nodo-snapshot keygen) and the xbps key (RSA) both belong on an offline machine, and
# sign and nodo are the steps that need them.
#
# import is for tests and for snapshots in which no Void package changes: the index is served,
# but a device that needed a Void package from it would find none. --untested is only for
# throwaway test keys; it skips the rule that a snapshot is signed only as it was tested.
#
# Withdrawing a bad snapshot: sign an older, good snapshot again under a new, higher serial with
# --withdraws <bad name>, and publish it. Devices recorded on the bad snapshot then move this
# project's packages back to the good snapshot's versions; every other device refuses a manifest
# that would move a package back. The serial rules stay as they are, so this is never a replay.
set -eu
. "$(dirname "$0")/lib.sh"
cmd=${1:?usage: publish-snapshot.sh fetch|import|nodo|test|sign|publish <name> ...}
name=${2:?snapshot name, for example 2026.12}
case $name in *[!A-Za-z0-9._-]* | .* | -*) echo "bad snapshot name: $name" >&2; exit 2 ;; esac
[ $# -ge 2 ] && shift 2
out=${SNAPSHOT_ROOT:-$root/out}
mkdir -p "$out"
out=$(cd "$out" && pwd)
snap=$out/snapshots/$name
stamp=$snap/tested

repodir() { case $1 in aarch64) echo "$snap/current/aarch64" ;; *) echo "$snap/current" ;; esac; }
nododir() { echo "$snap/nodo/$1"; }
upstream() { case $1 in aarch64) echo "$VOID_MIRROR/current/aarch64" ;; *) echo "$VOID_MIRROR/current" ;; esac; }
plain() { case $1 in *[!A-Za-z0-9._-]* | .* | -* | '') echo "bad $2: $1" >&2; exit 2 ;; esac; }

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
		echo "fetched $arch: $(find "$dir" -name '*.xbps' | wc -l) packages"
	done
	rm -f "$stamp"
	;;
import)
	arch=${1:?architecture}
	file=${2:?a Void <arch>-repodata file}
	plain "$arch" architecture
	dir=$(repodir "$arch")
	mkdir -p "$dir"
	cp "$file" "$dir/$arch-repodata"
	echo "imported the Void index for $arch, without packages"
	rm -f "$stamp"
	;;
nodo)
	arch=${1:?architecture}
	key=${2:?the xbps signing key, an RSA private key in PEM form}
	pkgdir=${3:-$root/out/feed/$arch}
	plain "$arch" architecture
	found=
	for f in "$pkgdir"/*."$arch".xbps "$pkgdir"/*.noarch.xbps; do [ -f "$f" ] && found=1; done
	[ -n "$found" ] || { echo "no $arch packages in $pkgdir" >&2; exit 1; }
	ensure_builder
	dir=$(nododir "$arch")
	rm -rf "$dir"
	mkdir -p "$dir"
	for f in "$pkgdir"/*."$arch".xbps "$pkgdir"/*.noarch.xbps; do
		[ -f "$f" ] && cp "$f" "$dir/"
	done
	# Only the newest version of each package stays, as xbps-rindex -c would keep it.
	docker run --rm --name "nodo-snapshot-nodo-$name-$arch" -e "ARCH=$arch" -e "HOST_UID=$(id -u)" -e "HOST_GID=$(id -g)" \
		-v "$dir:/repo" -v "$(realpath "$key"):/key.pem:ro" "$BUILDER" sh -euc '
			trap "chown -R $HOST_UID:$HOST_GID /repo" EXIT
			XBPS_TARGET_ARCH=$ARCH xbps-rindex -a /repo/*.xbps >/dev/null
			XBPS_TARGET_ARCH=$ARCH xbps-rindex -r /repo >/dev/null
			XBPS_TARGET_ARCH=$ARCH xbps-rindex --signedby monokulo-nodo --privkey /key.pem -s /repo >/dev/null
			XBPS_TARGET_ARCH=$ARCH xbps-rindex --privkey /key.pem -S /repo/*.xbps >/dev/null'
	echo "added $(find "$dir" -name '*.xbps' | wc -l) packages for $arch, signed"
	rm -f "$stamp"
	;;
test)
	board=${1:?board to test, for example qemu-virt}
	. "$root/boards/$board/board.env"
	[ -s "$(repodir "$BOARD_ARCH")/$BOARD_ARCH-repodata" ] || { echo "fetch $BOARD_ARCH first" >&2; exit 1; }
	ensure_builder
	set --
	[ "${BOARD_NEEDS_KVM:-}" = 1 ] && set -- --device /dev/kvm
	nodo=$(nododir "$BOARD_ARCH")
	case $snap in "$root"/*) ;; *) echo "test needs SNAPSHOT_ROOT inside the repository" >&2; exit 1 ;; esac
	rel=${snap#"$root/"}
	feed=/src/out/feed/$BOARD_ARCH
	[ -s "$nodo/$BOARD_ARCH-repodata" ] && feed=/src/$rel/nodo/$BOARD_ARCH
	# The same build as scripts/build-image.sh, with the snapshot as the only repositories.
	docker run --rm --name "nodo-snapshot-test-$name-$board" \
		-e "BOARD=$board" -e "VOID_MIRROR=/src/$rel" -e "NODO_REPO=$feed" -e "HOST_UID=$(id -u)" -e "HOST_GID=$(id -g)" \
		-v "$root:/src" "$@" "$BUILDER" /src/scripts/_build-image-inner.sh
	if [ "$board" = qemu-virt ]; then
		"$root/tests/qemu-boot-test.sh" qemu-virt
	fi
	for index in "$(repodir "$BOARD_ARCH")/$BOARD_ARCH-repodata" "$nodo/$BOARD_ARCH-repodata"; do
		[ -s "$index" ] && sha256sum "$index" | cut -d" " -f1 >> "$stamp"
	done
	echo "snapshot $name passed on $board"
	;;
sign)
	channel=${1:?channel}
	serial=${2:?serial, higher than any published before}
	key=${3:?secret key file}
	restart=${4:-no}
	untested=
	withdraws=
	[ $# -ge 4 ] && shift 4 || shift $#
	while [ $# -gt 0 ]; do
		case $1 in
		--untested) untested=--untested ;;
		--withdraws) plain "${2:?--withdraws takes a snapshot name}" "withdrawn snapshot"; withdraws="$withdraws withdraws:$2"; shift ;;
		*) echo "unknown sign option: $1" >&2; exit 2 ;;
		esac
		shift
	done
	plain "$channel" channel
	if [ "$untested" = --untested ]; then
		echo "warning: signing $name without a test run, which is only for throwaway test keys" >&2
	else
		[ -s "$stamp" ] || { echo "snapshot $name has not passed a test run" >&2; exit 1; }
	fi
	set --
	for index in "$snap"/current/*-repodata "$snap"/current/aarch64/*-repodata "$snap"/nodo/*/*-repodata; do
		[ -f "$index" ] || continue
		arch=${index##*/}; arch=${arch%-repodata}
		dir=${index%/*}
		# Refuse to sign an index that changed after its test run.
		if [ "$untested" != --untested ]; then
			grep -qx "$(sha256sum "$index" | cut -d" " -f1)" "$stamp" || { echo "$dir was not tested as it is now" >&2; exit 1; }
		fi
		repo=$arch
		case $dir in "$snap"/nodo/*)
			repo=$arch/nodo
			# Devices check that every one of these that they have installed is at this version.
			for f in "$dir"/*.xbps; do
				pv=${f##*/}; pv=${pv%.*.xbps}
				set -- "$@" "pkg:${pv%-*}=${pv##*-}"
			done ;;
		esac
		set -- "$@" "$repo=${dir#"$out/"}=$index"
	done
	[ $# -gt 0 ] || { echo "nothing to sign" >&2; exit 1; }
	# shellcheck disable=SC2086 # each word is one withdraws:<name> argument, checked by plain above
	set -- "$@" $withdraws
	cd "$root"
	cargo run -q --release -p nodo-update --bin nodo-snapshot -- manifest "$channel" "$serial" "$name" 30 "$restart" "$@" > "$snap/manifest.toml"
	rm -f "$snap/manifest.toml.sig"
	cargo run -q --release -p nodo-update --bin nodo-snapshot -- sign manifest "$snap/manifest.toml" "$key"
	echo "signed $snap/manifest.toml"
	;;
publish)
	channel=${1:?channel}
	plain "$channel" channel
	[ -s "$snap/manifest.toml.sig" ] || { echo "sign $name first" >&2; exit 1; }
	mkdir -p "$out/channels/$channel"
	# Each file is replaced whole. A device reading between the two renames finds a signature that
	# does not match and is refused, and tries again later; it never accepts a mixed pair.
	cp "$snap/manifest.toml.sig" "$out/channels/$channel/.manifest.toml.sig.new"
	cp "$snap/manifest.toml" "$out/channels/$channel/.manifest.toml.new"
	mv "$out/channels/$channel/.manifest.toml.sig.new" "$out/channels/$channel/manifest.toml.sig"
	mv "$out/channels/$channel/.manifest.toml.new" "$out/channels/$channel/manifest.toml"
	echo "channel $channel now offers $name"
	;;
*) echo "unknown command: $cmd" >&2; exit 2 ;;
esac
