#!/bin/sh
# Runs monokulo-base's nodo-report with and without nodo-healthd, using a stand-in for it, and
# checks the abstract socket section in both cases and that the report goes on after it. Needs
# no device: the fast tier runs it in the checks container.
#   tests/nodo-report.sh [path to nodo-report]
set -u
report=${1:-$(cd "$(dirname "$0")/.." && pwd)/packages/monokulo-base/files/nodo-report}
fail=0
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Without nodo-healthd. Only directories without it stay on PATH.
path=
for d in $(printf '%s' "$PATH" | tr ':' ' '); do
	[ -x "$d/nodo-healthd" ] || path=$path${path:+:}$d
done
PATH=$path sh "$report" > "$tmp/without" 2>/dev/null
grep -qx '== abstract Unix sockets' "$tmp/without"
result "without nodo-healthd: the abstract socket section is there" "$?"
sed -n '/^== abstract Unix sockets$/,/^== /p' "$tmp/without" > "$tmp/section"
grep -qx 'nodo-healthd is not installed, so owners are not shown' "$tmp/section"
result "without nodo-healthd: it says owners are not shown" "$?"
# Then the kernel's own list, or a line saying it is empty.
[ "$(grep -cv '^== \|^nodo-healthd is not installed' "$tmp/section")" -ge 1 ]
result "without nodo-healthd: it still lists the kernel's abstract sockets" "$?"
grep -qx '== cuprated log' "$tmp/without" && grep -qx '== kernel network and storage' "$tmp/without"
result "without nodo-healthd: the report goes on to the end" "$?"

# With nodo-healthd, which exits 1 because something unexpected listens.
mkdir "$tmp/bin"
cat > "$tmp/bin/nodo-healthd" <<'EOF'
#!/bin/sh
[ "$*" = --abstract-sockets ] || { echo "unexpected arguments: $*"; exit 2; }
echo "kernel 6.1.141, Landlock ABI none: the sandbox cannot keep services granted a Unix socket away from abstract sockets"
echo "UNEXPECTED @probe Stream inode 42: python3 (pid 7) as user nobody"
exit 1
EOF
chmod 755 "$tmp/bin/nodo-healthd"
PATH=$tmp/bin:$path sh "$report" > "$tmp/with" 2>/dev/null
sed -n '/^== abstract Unix sockets$/,/^== /p' "$tmp/with" > "$tmp/section"
grep -q '^kernel 6.1.141, Landlock ABI none' "$tmp/section" && grep -qx 'UNEXPECTED @probe Stream inode 42: python3 (pid 7) as user nobody' "$tmp/section"
result "with nodo-healthd: its list is in the section" "$?"
grep -qx '(nodo-healthd exit status 1)' "$tmp/section"
result "with nodo-healthd: its exit status is shown" "$?"
grep -qx '== cuprated log' "$tmp/with" && grep -qx '== kernel network and storage' "$tmp/with"
result "with nodo-healthd: the report goes on to the end" "$?"

# The GPU section, with a stand-in nodo-gpu-check and without one.
sed -n '/^== GPU$/,/^== /p' "$tmp/with" | grep -qx 'nodo-gpu-check is not installed'
result "without nodo-gpu-check: the GPU section says so" "$?"
cat > "$tmp/bin/nodo-gpu-check" <<'EOF'
#!/bin/sh
echo "Vulkan loader 1.4.363"
echo "no Vulkan device"
exit 2
EOF
chmod 755 "$tmp/bin/nodo-gpu-check"
PATH=$tmp/bin:$path sh "$report" > "$tmp/gpu" 2>/dev/null
sed -n '/^== GPU$/,/^== /p' "$tmp/gpu" > "$tmp/section"
grep -qx 'no Vulkan device' "$tmp/section" && grep -qx '(nodo-gpu-check exit status 2)' "$tmp/section"
result "with nodo-gpu-check: its output and exit status are in the GPU section" "$?"
grep -qx '== cuprated log' "$tmp/gpu"
result "with nodo-gpu-check: the report goes on after it" "$?"
exit "$fail"
