#!/bin/sh
# Runs monokulo-base's dhcpcd-nodo-local against stand-in root directories: a fresh dhcpcd.conf, one
# with the unmarked block images appended before, an older marked block, lines added after the
# block, a second run and removal. Needs no device: the fast tier runs it in the checks container,
# and tests/localnet-dhcp.sh checks the same through xbps in the VM.
#   tests/dhcpcd-nodo-local.sh
set -u
src=$(cd "$(dirname "$0")/.." && pwd)/packages/monokulo-base/files
fail=0
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
begin='# BEGIN nodo-local (monokulo-base replaces this block on every update; do not edit it)'

# dhcpcd's own settings, as its package ships them, shortened.
cat > "$tmp/dhcpcd.conf" <<'EOF'
# A sample configuration for dhcpcd.
hostname
duid
persistent
vendorclassid

option domain_name_servers, domain_name, domain_search
option interface_mtu
require dhcp_server_identifier
slaac private
controlgroup wheel
EOF
# A root as a package install leaves it: dhcpcd's file and monokulo-base's files.
newroot() {
	rm -rf "$tmp/r"
	mkdir -p "$tmp/r/etc" "$tmp/r/usr/share/monokulo" "$tmp/r/usr/libexec/monokulo"
	cp "$tmp/dhcpcd.conf" "$tmp/r/etc/dhcpcd.conf"
	cp "$src/dhcpcd-nodo-local.conf" "$tmp/r/usr/share/monokulo/"
	cp "$src/dhcpcd-nodo-local" "$tmp/r/usr/libexec/monokulo/"
}
run() { (cd "$tmp/r" && sh ./usr/libexec/monokulo/dhcpcd-nodo-local "$1" .); }
conf=$tmp/r/etc/dhcpcd.conf
# The expected result: dhcpcd's settings, one blank line, then the marked block at the end.
{ cat "$tmp/dhcpcd.conf"; printf '\n%s\n' "$begin"; cat "$src/dhcpcd-nodo-local.conf"; echo '# END nodo-local'; } > "$tmp/want"
once() { [ "$(grep -cx 'interface nodo-local' "$conf")" = 1 ] && [ "$(grep -cx "$begin" "$conf")" = 1 ]; }

newroot
run install
cmp -s "$conf" "$tmp/want"
result "a fresh install adds the marked block at the end and keeps dhcpcd's settings" "$?"
touch -d '2000-01-01' "$conf"
run install
cmp -s "$conf" "$tmp/want" && [ "$(stat -c %Y "$conf")" = "$(date -d 2000-01-01 +%s)" ]
result "a re-install changes nothing and does not write the file" "$?"

# The block as images appended it before the markers, when the file began with a blank line.
newroot
{ echo; cat "$src/dhcpcd-nodo-local.conf"; } >> "$conf"
run install
cmp -s "$conf" "$tmp/want" && once
result "the unmarked block from an older image is replaced, not repeated" "$?"
# An older version of the block, without its comment, after a blank line.
newroot
printf '\ninterface nodo-local\n\tnogateway\n\tmetric 3000\n' >> "$conf"
run install
cmp -s "$conf" "$tmp/want" && ! grep -q 'metric 3000' "$conf"
result "an older unmarked block is replaced by the current one" "$?"

# An upgrade from an older marked block.
newroot
{ printf '\n%s\n' "$begin"; printf 'interface nodo-local\n\tnogateway\n\tmetric 3000\n'; echo '# END nodo-local'; } >> "$conf"
run install
cmp -s "$conf" "$tmp/want" && once
result "an upgrade replaces an older marked block" "$?"

# Settings the owner added after the block would only apply to nodo-local, so they move above it.
newroot
run install
printf 'noarp\n' >> "$conf"
run install
{ cat "$tmp/dhcpcd.conf"; echo noarp; printf '\n%s\n' "$begin"; cat "$src/dhcpcd-nodo-local.conf"; echo '# END nodo-local'; } > "$tmp/want2"
cmp -s "$conf" "$tmp/want2"
result "lines added after the block are kept, above it" "$?"

newroot
run install
run remove
cmp -s "$conf" "$tmp/dhcpcd.conf"
result "removal leaves dhcpcd's own file as it was" "$?"
newroot
rm "$conf"
run install && [ ! -e "$conf" ]
result "without dhcpcd.conf nothing is created" "$?"
newroot
rm "$tmp/r/usr/share/monokulo/dhcpcd-nodo-local.conf"
! run install 2>/dev/null && cmp -s "$conf" "$tmp/dhcpcd.conf"
result "a missing block file fails and leaves dhcpcd.conf alone" "$?"
[ -z "$(find "$tmp/r/etc" -name '*.new')" ]
result "no temporary file is left behind" "$?"
exit "$fail"
