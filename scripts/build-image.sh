#!/bin/sh
# Builds a root filesystem and disk image without needing host root or loop devices.
# Usage: scripts/build-image.sh <board> [authorized_keys file]
set -eu
. "$(dirname "$0")/lib.sh"
ensure_builder
board=${1:?usage: build-image.sh <board> [authorized_keys]}
keys=${2:-}
[ -f "$root/boards/$board/board.env" ] || { echo "unknown board: $board" >&2; exit 1; }
[ -n "$keys" ] || echo "warning: no authorized_keys given, the image will have no remote login" >&2
mkdir -p "$root/out/images/$board"
set -- 
[ -n "$keys" ] && set -- -v "$(realpath "$keys"):/keys:ro"
. "$root/boards/$board/board.env"
[ "${BOARD_NEEDS_KVM:-}" = 1 ] && set -- "$@" --device /dev/kvm
docker run --rm \
	-e "BOARD=$board" -e "VOID_MIRROR=$VOID_MIRROR" -e "HOST_UID=$(id -u)" -e "HOST_GID=$(id -g)" \
	-v "$root:/src" "$@" \
	"$BUILDER" /src/scripts/_build-image-inner.sh
