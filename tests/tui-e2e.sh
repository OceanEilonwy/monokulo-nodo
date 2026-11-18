#!/bin/sh
# Drives the real nodo-tui against the real nodo-helper in the dev VM, through a pseudo-terminal,
# as an unprivileged member of the nodo group, and checks what the screen shows. Runs on the host.
#   tests/tui-e2e.sh --fresh [journey...]   one command from nothing (see below)
#   tests/tui-e2e.sh [journey...]           on a VM that is already up
# --fresh builds the packages that changed, builds the KVM image when it is missing or older than
# its board files (as scripts/test.sh vm does), starts the dev VM or puts it back to the clean
# image, and pushes the packages the journeys drive. Without it the VM must be fresh already,
# because the full run starts with first boot:
#   scripts/dev-vm.sh down; scripts/dev-vm.sh up
#   scripts/dev-vm.sh push nodo-tools nodo-caddy nodo-localnet nodo-ddns
#   tests/acme-prepare.sh          (optional: the Certificate screen then follows a real Pebble order)
#   tests/tui-e2e.sh [journey...]
# The journeys are in tests/tui/journeys.py and tests/tui/device_flows.py. Naming some runs only
# those, for a quick loop on a VM that is already past first boot; "device" names every journey of
# device_flows.py, the flows that broke on a real device. Those set themselves up, so they also run
# alone on a fresh VM. The driver needs python3, the Web journey curl and the Onion journey tor,
# which come from the pinned mirror, and the Network journey virtual radios. The Updates journey for
# the system base row needs the test snapshots, which tests/snapshot-prepare.sh makes when the VM
# has none.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
vm() { "$root/scripts/dev-vm.sh" "$@"; }
if [ "${1:-}" = --fresh ]; then
	shift
	board=${DEV_BOARD:-x86-kvm}
	. "$root/boards/$board/board.env"
	# The image lets in this key, and the first boot journey looks for its comment.
	if [ ! -s "$root/build/test-key" ]; then
		mkdir -p "$root/build"
		ssh-keygen -q -t ed25519 -N "" -C monokulo-test -f "$root/build/test-key" </dev/null
	fi
	"$root/scripts/build-packages.sh" "$BOARD_ARCH"
	img=$root/out/images/$board/disk.img
	if [ ! -s "$img" ] || [ -n "$(find "$root/boards/$board" "$root/versions.env" -newer "$img" -print -quit)" ]; then
		"$root/scripts/build-image.sh" "$board" "$root/build/test-key.pub"
	fi
	if [ "$(vm up)" = "already running" ]; then vm fresh; fi
	vm push nodo-tools nodo-caddy nodo-localnet nodo-ddns
fi
# Tor too, so the Onion journey checks the torrc and an onion address Tor really made.
vm ssh 'command -v python3 >/dev/null && command -v curl >/dev/null && [ -d /etc/sv/tor ]' || vm mirror python3 curl tor
# The Network journey joins a virtual access point: tests/wifi-prepare.sh builds mac80211_hwsim for
# the VM kernel and installs hostapd, wpa_supplicant, dnsmasq and iw from the pinned mirror.
# TUI_E2E_WIFI=0 skips that, and the journey then checks the screen of a device without radios.
if [ "${TUI_E2E_WIFI:-1}" = 1 ]; then "$root/tests/wifi-prepare.sh"; fi
tar -C "$root/tests" -cf - tui acme/fake-dns.py |
	vm ssh 'rm -rf /tmp/tui && mkdir -p /tmp/tui-src && tar -C /tmp/tui-src -xf - && mv /tmp/tui-src/tui /tmp/tui && mv /tmp/tui-src/acme/fake-dns.py /tmp/tui/ && rm -rf /tmp/tui-src'
args=
want_system=1
[ $# -gt 0 ] && want_system=
for a in "$@"; do args="$args '$a'"; [ "$a" = system ] && want_system=1; done
# The Updates journey's system base row needs the test snapshots, signed with throwaway keys.
if [ -n "$want_system" ] && ! vm ssh '[ -s /srv/snapshot-test/server/snapshots/test-b/manifest.toml.sig ]'; then
	"$root/tests/snapshot-prepare.sh"
fi
# TUI_E2E_USB=1 plugs in a blank virtual stick (a file in the VM's container, never a host device),
# so the first-boot restore journey also sees a drive it must refuse. Without it, it sees none.
if [ "${TUI_E2E_USB:-}" = 1 ]; then
	vm usb-create tui-blank 64
	vm usb-plug tui-blank
	trap 'vm usb-unplug tui-blank || true' EXIT
fi
vm ssh "python3 /tmp/tui/journeys.py $args </dev/null"
