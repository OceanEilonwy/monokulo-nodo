#!/bin/sh
# Runs on the device under test, as root, with nodo-tools installed (python3 from the mirror is
# used when present). The VM has no Mali, so a render node stands in: vgem's when the kernel has
# it, or a character device made at /dev/dri/renderD200 (with a card node beside it). Two apps go
# through the real helper, so their run scripts come from nodo-gen:
#
#   gpu-yes  asks for the GPU ([devices] gpu = true)
#   gpu-no   does not
#
# Each records, from inside its sandbox, what it sees of /dev/dri and /sys and whether it can open
# the render node, and with python3 makes a DRM ioctl on it (DRM_IOCTL_VERSION, through
# nodo-confine's seccomp filter and Landlock domain). Its groups are read from outside, where
# they show the real group ids. The same user outside the sandbox, without the gpu group, is the
# control showing the group is what grants access.
#   scripts/dev-vm.sh ssh 'cat > /tmp/gpu-sandbox.sh' < tests/gpu-sandbox.sh
#   scripts/dev-vm.sh ssh 'sh /tmp/gpu-sandbox.sh </dev/null'
set -u
fail=0
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
ctl() { nodo-tui --ctl "$@"; }
apps=/opt/nodo/apps
made=

cleanup() {
	for id in gpu-yes gpu-no; do
		[ -d "/etc/sv/$id" ] || continue
		touch "/etc/sv/$id/down"
		sv -w 15 down "$id" >/dev/null 2>&1
	done
	for n in $made; do rm -f "$n"; done
}
trap cleanup EXIT

getent group gpu >/dev/null
result "nodo-tools created the gpu group" "$?"
if [ -z "$(getent group gpu | cut -d: -f4)" ]; then r=0; else r=1; fi
result "the gpu group has no members" "$r"
gpu_gid=$(getent group gpu | cut -d: -f3)

# A render node: vgem's if the kernel has it, else a stand-in. The udev rule is the T6N
# overlay's, so apply what it does here: root:gpu 0660.
modprobe vgem 2>/dev/null
node=$(ls /dev/dri/renderD* 2>/dev/null | head -n 1)
if [ -n "$node" ]; then
	kind=vgem
else
	kind=stand-in
	mkdir -p /dev/dri
	node=/dev/dri/renderD200
	mknod "$node" c 1 3 && made="$made $node"
	mknod /dev/dri/card200 c 1 3 && made="$made /dev/dri/card200"
fi
card=$(ls /dev/dri/card* 2>/dev/null | head -n 1)
chown root:gpu /dev/dri/renderD*
chmod 0660 /dev/dri/renderD*
echo "  render node: $node ($kind), card node: ${card:-none}"
if [ -c "$node" ] && [ -n "$card" ]; then r=0; else r=1; fi
result "a render node and a card node exist" "$r"

[ -L /var/service/nodo-helper ] || ln -s /etc/sv/nodo-helper /var/service/
for _ in $(seq 1 30); do ctl services >/dev/null 2>&1 && break; sleep 1; done
mkdir -p /var/lib/nodo/manifests
cat > /tmp/gpu-probe.py <<'EOF'
import fcntl, os, struct, sys
# struct drm_version on 64-bit: three ints, padding, then three (size_t, char *) pairs.
fd = os.open(sys.argv[1], os.O_RDWR)
buf = bytearray(64)
fcntl.ioctl(fd, 0xC0406400, buf)
major, minor, patch = struct.unpack_from("iii", buf, 0)
name_len = struct.unpack_from("Q", buf, 16)[0]
print(f"ioctl ok: version {major}.{minor}.{patch}, name length {name_len}")
EOF
for id in gpu-yes gpu-no; do
	mkdir -p "$apps/$id/bin"
	cp /tmp/gpu-probe.py "$apps/$id/probe.py"
	cat > "$apps/$id/bin/run" <<EOF
#!/bin/sh
out=/var/lib/apps/$id/probe.txt
{
	echo "dri: \$(ls /dev/dri 2>&1 | tr '\n' ' ')"
	echo "sys: \$(find /sys -maxdepth 4 2>/dev/null | sort | tr '\n' ' ')"
	if (exec 3<>"$node") 2>/dev/null; then echo "open: ok"; else echo "open: refused"; fi
	if [ -x /usr/bin/python3 ]; then echo "\$(python3 /opt/nodo/apps/$id/probe.py "$node" 2>&1 | tail -n 1)"; fi
	echo "icd: \$(ls /usr/share/vulkan/icd.d 2>&1 | tr '\n' ' ')"
} > \$out.tmp
mv \$out.tmp \$out
while :; do sleep 1; done
EOF
	chmod 755 "$apps/$id/bin/run"
	chmod -R a+rX "$apps/$id"
	printf 'api_version = 1\nid = "%s"\nversion = "1.0"\nexec = ["bin/run"]\nstop_timeout_secs = 5\n' "$id" > "/var/lib/nodo/manifests/$id.toml"
	rm -f "/etc/sv/$id/down" "/var/lib/apps/$id/probe.txt"
done
printf '\n[devices]\ngpu = true\n' >> /var/lib/nodo/manifests/gpu-yes.toml

ctl install gpu-yes > /tmp/gpu.out 2>&1 && ctl install gpu-no >> /tmp/gpu.out 2>&1
r=$?; [ "$r" = 0 ] || cat /tmp/gpu.out
result "both apps install through the helper" "$r"

grep -q 'exec chpst -u _gpu-yes:_gpu-yes:gpu bwrap' /etc/sv/gpu-yes/run && grep -q 'for node in "$dri"/renderD\*; do' /etc/sv/gpu-yes/run &&
	grep -q 'exec chpst -u _gpu-no:_gpu-no bwrap' /etc/sv/gpu-no/run && ! grep -q '/dev/dri\|/sys' /etc/sv/gpu-no/run
result "generated scripts: the GPU group and render nodes only for gpu-yes" "$?"

for id in gpu-yes gpu-no; do
	for _ in $(seq 1 30); do [ -s "/var/lib/apps/$id/probe.txt" ] && break; sleep 1; done
	echo "  $id:"; sed 's/^/    /' "/var/lib/apps/$id/probe.txt" 2>/dev/null
done
groups_of() { p=$(pgrep -u "_$1" -x run | head -n 1); [ -n "$p" ] && sed -n 's/^Groups:[[:space:]]*//p' "/proc/$p/status"; }
yes_groups=$(groups_of gpu-yes); no_groups=$(groups_of gpu-no)
echo "  groups (host ids): gpu-yes [$yes_groups], gpu-no [$no_groups], gpu is $gpu_gid"

p=/var/lib/apps/gpu-yes/probe.txt
case " $yes_groups " in *" $gpu_gid "*) r=0 ;; *) r=1 ;; esac
result "gpu-yes runs with the gpu group" "$r"
grep -q "^dri: ${node##*/} $" "$p"
result "gpu-yes sees the render node and nothing else in /dev/dri, no card node" "$?"
grep -qx 'open: ok' "$p"
result "gpu-yes can open the render node" "$?"
if [ -x /usr/bin/python3 ] && [ "$kind" = vgem ]; then
	grep -q '^ioctl ok: version .*, name length 4$' "$p"
	result "gpu-yes makes a DRM ioctl on vgem's render node under nodo-confine" "$?"
	n=$(cat "/sys/class/drm/${node##*/}/dev")
	grep -q " /sys/dev/char/$n " "$p" && grep -q " /sys/devices/platform/vgem " "$p"
	result "gpu-yes sees the render node's sysfs link and device, for libdrm" "$?"
	! grep -q '/sys/class\|/sys/kernel\|/sys/firmware\|/sys/block' "$p"
	result "gpu-yes sees no other part of /sys" "$?"
elif [ -x /usr/bin/python3 ]; then
	# The stand-in is a memory device, which answers any ioctl with ENOTTY. Getting that answer
	# shows the call reached the driver: the sandbox's filter would have refused it with EACCES
	# or ENOSYS before.
	grep -q 'Errno 25\] Inappropriate ioctl for device' "$p"
	result "gpu-yes's ioctl on the stand-in reaches the device driver under nodo-confine" "$?"
	echo "SKIP the vgem DRM_IOCTL_VERSION and sysfs checks: this kernel has no vgem"
else
	echo "SKIP the ioctl and sysfs checks: they need python3"
fi
p=/var/lib/apps/gpu-no/probe.txt
case " $no_groups " in *" $gpu_gid "*) r=1 ;; *) r=0 ;; esac
result "gpu-no runs without the gpu group" "$r"
grep -q '^dri: ls: .*No such file or directory' "$p" && grep -qx 'sys: ' "$p" && grep -qx 'open: refused' "$p"
result "gpu-no has no /dev/dri and no /sys at all" "$?"

# The control: the same user outside any sandbox, without and with the gpu group.
if chpst -u _gpu-yes:_gpu-yes sh -c "exec 3<>'$node'" 2>/dev/null; then r=1; else r=0; fi
result "control: without the gpu group the service user cannot open the render node" "$r"
chpst -u _gpu-yes:_gpu-yes:gpu sh -c "exec 3<>'$node'"
result "control: with the gpu group it can" "$?"

r=0
for id in gpu-yes gpu-no; do
	sv status "$id" | grep -q "^run:" || { sv status "$id"; r=1; }
done
result "both services still run" "$r"
sv -w 30 stop gpu-yes | grep -q '^ok: down'
result "the GPU service stops cleanly through control/t" "$?"
exit "$fail"
