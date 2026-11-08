#!/bin/sh
# Runs in a container with no network. Checks everything that needs no build or VM.
set -u
fail=0
step() { name=$1; shift; if out=$("$@" 2>&1); then echo "PASS $name"; else echo "FAIL $name"; printf '%s\n' "$out" | cut -c1-200 | head -30; fail=1; fi; }
cd /src || exit 1
scripts=$(find scripts tests boards packages -type f | while read -r f; do head -n 1 "$f" | grep -q '^#!/bin/sh' && echo "$f"; done)
step "shell syntax" sh -c 'for f in "$@"; do sh -n "$f" || exit 1; done' sh $scripts
step "shellcheck" shellcheck -S warning -x -e SC1090,SC1091,SC2086,SC2154 $scripts
step "recipe syntax" sh -c 'for t in packages/*/template; do bash -n "$t" || exit 1; done'
step "toml config" python3 -I -c 'import sys,tomllib,glob
for f in glob.glob("packages/*/files/*.toml"): tomllib.load(open(f,"rb"))'
step "service layout" sh -c 'for d in packages/*/files/*/; do [ -f "$d/run" ] || continue; [ -x "$d/run" ] && [ -x "$d/log/run" ] || { echo "$d: run and log/run must be executable"; exit 1; }; grep -q "^exec </dev/null" "$d/run" || { echo "$d: run must detach stdin"; exit 1; }; if grep -q bwrap "$d/run"; then [ -x "$d/control/t" ] || { echo "$d: control/t is required for bwrap services"; exit 1; }; fi; done'
# Rules need the service users to exist, as they do on a real device.
for t in packages/*/template; do
	for u in $(sed -n 's/^system_accounts="\(.*\)"/\1/p' "$t"); do useradd -r "$u" 2>/dev/null; done
done
mkdir -p /etc/nftables.d
cp packages/*/files/*.nft /etc/nftables.d/
step "firewall rules" nft -c -f packages/monokulo-base/files/nftables.conf
exit "$fail"
