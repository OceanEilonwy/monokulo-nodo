#!/bin/sh
# Drives the real nodo-tui against the real nodo-helper in the dev VM, through a pseudo-terminal,
# as an unprivileged member of the nodo group, and checks what the screen shows. Runs on the host.
# It needs a fresh VM, because it starts with first boot:
#   scripts/dev-vm.sh down; scripts/dev-vm.sh up
#   scripts/dev-vm.sh push nodo-tools nodo-caddy nodo-localnet nodo-ddns
#   tests/acme-prepare.sh          (optional: the Certificate screen then follows a real Pebble order)
#   tests/tui-e2e.sh [journey...]
# The journeys are in tests/tui/journeys.py. Naming some runs only those, for a quick loop on a
# VM that is already past first boot. The driver needs python3 and the Web journey curl, which
# come from the pinned mirror.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
vm() { "$root/scripts/dev-vm.sh" "$@"; }
vm ssh 'command -v python3 >/dev/null && command -v curl >/dev/null' || vm mirror python3 curl
tar -C "$root/tests" -cf - tui acme/fake-dns.py |
	vm ssh 'rm -rf /tmp/tui && mkdir -p /tmp/tui-src && tar -C /tmp/tui-src -xf - && mv /tmp/tui-src/tui /tmp/tui && mv /tmp/tui-src/acme/fake-dns.py /tmp/tui/ && rm -rf /tmp/tui-src'
args=
for a in "$@"; do args="$args '$a'"; done
vm ssh "python3 /tmp/tui/journeys.py $args </dev/null"
