#!/bin/sh
# Runs on the device under test, with this build's monokulo-base in /tmp/base-feed. Checks through
# xbps itself that nodo-localnet's block reaches /etc/dhcpcd.conf on a re-install, goes on removal,
# and replaces the unmarked copy an older image appended when a device upgrades from a
# monokulo-base without the hook, which is the case of a device updated in place.
set -u
fail=0
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
conf=/etc/dhcpcd.conf
block=/usr/share/monokulo/dhcpcd-nodo-local.conf
begin='# BEGIN nodo-local (monokulo-base replaces this block on every update; do not edit it)'
arch=$(xbps-uhelper arch)
pkgfile=$(ls /tmp/base-feed/monokulo-base-[0-9]*."$arch".xbps 2>/dev/null | sort -V | tail -n 1)
[ -f "$pkgfile" ] || { echo "FAIL no monokulo-base package in /tmp/base-feed"; exit 1; }
current=$(basename "$pkgfile" ".$arch.xbps")
# The marked block, once, at the end, and the same as the package's file.
block_ok() {
	[ "$(grep -cx 'interface nodo-local' "$conf")" = 1 ] && [ "$(grep -cx "$begin" "$conf")" = 1 ] &&
		[ "$(tail -n 1 "$conf")" = '# END nodo-local' ] &&
		sed -n "/^# BEGIN nodo-local/,/^# END nodo-local/p" "$conf" | sed '1d;$d' | cmp -s - "$block"
}
dhcpcd_settings_kept() { grep -qx 'duid' "$conf" && grep -qx 'controlgroup wheel' "$conf"; }

xbps-rindex -a /tmp/base-feed/*.xbps >/dev/null 2>&1
xbps-install -yf -R /tmp/base-feed monokulo-base >/tmp/dhcpcd-block.log 2>&1
block_ok && dhcpcd_settings_kept
result "a re-install of monokulo-base ($current) leaves the marked block once, at the end" "$?"
sum=$(sha256sum "$conf")
xbps-install -yf -R /tmp/base-feed monokulo-base >>/tmp/dhcpcd-block.log 2>&1
if [ "$(sha256sum "$conf")" = "$sum" ]; then r=0; else r=1; fi
result "a second re-install changes nothing" "$r"

# A monokulo-base from before the hook: this one's files without the script, INSTALL or REMOVE.
rm -rf /tmp/base-old && mkdir -p /tmp/base-old/root
zstd -dqc "$pkgfile" | tar -C /tmp/base-old/root -xf -
rm -rf /tmp/base-old/root/props.plist /tmp/base-old/root/files.plist /tmp/base-old/root/INSTALL \
	/tmp/base-old/root/REMOVE /tmp/base-old/root/usr/libexec/monokulo
(cd /tmp/base-old && xbps-create -q -A "$arch" -n monokulo-base-0.0.9_1 -s "monokulo-base before the dhcpcd hook" \
	-F "/etc/nftables.conf /etc/ssh/sshd_config.d/10-monokulo.conf" root >/dev/null 2>&1 &&
	xbps-rindex -a /tmp/base-old/*.xbps >/dev/null 2>&1)
[ -f "/tmp/base-old/monokulo-base-0.0.9_1.$arch.xbps" ] && [ -f /tmp/base-old/root/usr/bin/nodo-report ]
result "an older monokulo-base without the hook can be made" "$?"
# Without it the device would be left without monokulo-base, so stop here.
[ "$fail" = 0 ] || exit 1

xbps-remove -Fy monokulo-base >>/tmp/dhcpcd-block.log 2>&1
! grep -q 'nodo-local' "$conf" && dhcpcd_settings_kept
result "removing monokulo-base takes the block out and keeps dhcpcd's settings" "$?"
xbps-install -y -R /tmp/base-old monokulo-base >>/tmp/dhcpcd-block.log 2>&1
# What an image built before the hook holds: the block appended without markers, after a blank line.
{ echo; cat "$block"; } >> "$conf"
xbps-query -p pkgver monokulo-base | grep -qx monokulo-base-0.0.9_1 && ! grep -qx "$begin" "$conf"
result "the device runs the older monokulo-base with the image's unmarked block" "$?"
xbps-install -yu -R /tmp/base-feed monokulo-base >>/tmp/dhcpcd-block.log 2>&1
xbps-query -p pkgver monokulo-base | grep -qx "$current" && block_ok && dhcpcd_settings_kept
result "an upgrade replaces the unmarked block with the marked one, once" "$?"
# The same upgrade on a device whose image never had the block.
xbps-remove -Fy monokulo-base >>/tmp/dhcpcd-block.log 2>&1
xbps-install -y -R /tmp/base-old monokulo-base >>/tmp/dhcpcd-block.log 2>&1
! grep -q 'nodo-local' "$conf"
result "the older monokulo-base installs no block" "$?"
xbps-install -yu -R /tmp/base-feed monokulo-base >>/tmp/dhcpcd-block.log 2>&1
xbps-query -p pkgver monokulo-base | grep -qx "$current" && block_ok
result "an upgrade adds the block where the image had none" "$?"
xbps-pkgdb monokulo-base >/dev/null 2>&1
result "the package database is consistent after it" "$?"
for s in dhcpcd monokulo-diag; do
	[ -L "/var/service/$s" ] || continue
	sv status "$s" | grep -q '^run:' || { sleep 3; sv status "$s" | grep -q '^run:'; }
	result "$s still runs" "$?"
done
[ "$fail" = 0 ] || tail -n 30 /tmp/dhcpcd-block.log
exit "$fail"
