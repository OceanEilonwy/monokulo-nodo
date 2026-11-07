#!/bin/sh
# Test tiers, from fastest to slowest. Each tier includes the ones before it.
#   fast  static checks only, a few seconds
#   vm    rebuild changed packages and test them in the running KVM dev VM
#   full  rebuild the aarch64 image, then cold boot, reboot and size checks
set -eu
. "$(dirname "$0")/lib.sh"
tier=${1:-fast}
case $tier in fast|vm|full) ;; *) echo "usage: test.sh fast|vm|full" >&2; exit 2 ;; esac

tag="monokulo-checks:$(tree_hash docker/checks.Dockerfile versions.env)"
docker image inspect "$tag" >/dev/null 2>&1 || docker build -q -t "$tag" --build-arg "VOID_IMAGE=$VOID_IMAGE" \
	--build-arg "VOID_MIRROR=$VOID_MIRROR" -f "$root/docker/checks.Dockerfile" "$root/docker" >/dev/null
docker run --rm --network none --cap-add NET_ADMIN -v "$root:/src:ro" "$tag" /src/tests/_static-checks.sh
[ "$tier" = fast ] && exit 0

if [ "$tier" = vm ]; then
	# Service logic runs on the fast KVM machine. Stale images are rebuilt only when missing.
	board=${DEV_BOARD:-x86-kvm}
	. "$root/boards/$board/board.env"
	changed=$("$root/scripts/build-packages.sh" --changed "$BOARD_ARCH")
	"$root/scripts/build-packages.sh" "$BOARD_ARCH"
	[ -s "$root/out/images/$board/disk.img" ] || "$root/scripts/build-image.sh" "$board" "$root/build/test-key.pub"
	"$root/scripts/dev-vm.sh" up
	if [ -n "$changed" ]; then "$root/scripts/dev-vm.sh" push $changed; fi
	exec "$root/scripts/dev-vm.sh" check
fi

# The release gate runs on the emulated target architecture.
"$root/scripts/build-packages.sh" aarch64
"$root/scripts/build-image.sh" qemu-virt "$root/build/test-key.pub"
"$root/tests/qemu-boot-test.sh" qemu-virt
"$root/tests/size-check.sh" qemu-virt
