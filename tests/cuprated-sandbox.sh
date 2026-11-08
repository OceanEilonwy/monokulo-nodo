#!/bin/sh
# Runs on the device under test, as root, with python3 from the mirror (scripts/dev-vm.sh mirror
# python3). Runs a probe inside cuprated's real sandbox: the installed run script, with only the
# program at its end replaced, so bwrap and nodo-confine are exactly what cuprated gets. The probe
# connects to an abstract socket root holds on the host, makes a Unix socket of its own, tries
# `unshare -U` and looks for host files. The same probe run as _cuprated outside the sandbox is the
# control, showing a refusal comes from the sandbox. cuprated keeps running throughout.
#   scripts/dev-vm.sh ssh 'cat > /tmp/cuprated-sandbox.sh' < tests/cuprated-sandbox.sh
#   scripts/dev-vm.sh ssh 'sh /tmp/cuprated-sandbox.sh </dev/null'
set -u
fail=0
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
work=/tmp/cuprated-sandbox
data=/var/lib/cuprated
run=/etc/sv/cuprated/run

cleanup() {
	[ -n "${host_pid:-}" ] && kill "$host_pid" 2>/dev/null
	rm -rf "$work" "$data/probe.py" "$data/probe.json" "$data/probe.json.tmp"
}
trap cleanup EXIT
command -v python3 >/dev/null || { echo "FAIL python3 is needed: scripts/dev-vm.sh mirror python3"; exit 1; }
[ ! -e /etc/sv/cuprated/conf ] || { echo "FAIL this test is for a device without a data drive conf"; exit 1; }
echo "kernel $(uname -r): $(nodo-confine --check)"
mkdir -p "$work"
# The control runs as _cuprated and writes its result here.
chmod 1777 "$work"

grep -q -- '--unshare-user --disable-userns' "$run" && grep -q '/usr/bin/nodo-confine -- /usr/bin/cuprated ' "$run" &&
	! grep -q -- '--unix' "$run"
result "cuprated's run script has the user namespace hardening and nodo-confine, without a Unix grant" "$?"

# The probe lives in the data directory, the one place both the sandbox and the control can read.
cat > "$data/probe.py" <<'EOF'
import json, os, socket, subprocess, sys
out = sys.argv[1]
def connect(addr):
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    except OSError as e:
        return "socket: " + e.strerror
    try:
        s.settimeout(3)
        s.connect(addr)
        s.sendall(b"probe " + os.environ.get("PROBE", "?").encode() + b"\n")
        s.close()
        return "connected"
    except OSError as e:
        return "connect: " + (e.strerror or str(e))
res = {"host_abstract": connect("\0nodo-cuprated-host")}
try:
    own = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    own.bind("\0nodo-cuprated-own")
    res["own_abstract"] = "bound"
except OSError as e:
    res["own_abstract"] = "socket: " + e.strerror
r = subprocess.run(["unshare", "-U", "true"], capture_output=True, text=True)
res["unshare"] = "ok" if r.returncode == 0 else "failed: " + r.stderr.strip()
res["shadow"] = os.path.exists("/etc/shadow")
res["sv_dir"] = os.path.exists("/etc/sv")
res["tcp"] = "ok"
try:
    socket.socket(socket.AF_INET, socket.SOCK_STREAM).close()
except OSError as e:
    res["tcp"] = e.strerror
with open(out + ".tmp", "w") as f:
    json.dump(res, f)
os.rename(out + ".tmp", out)
EOF
chown _cuprated:_cuprated "$data/probe.py"

# Root's abstract socket on the host, standing in for any host daemon.
cat > "$work/host.py" <<'EOF'
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind("\0nodo-cuprated-host")
s.listen(8)
while True:
    c, _ = s.accept()
    with open(sys.argv[1], "a") as f:
        f.write(c.makefile().readline())
    c.close()
EOF
: > "$work/host.log"
python3 "$work/host.py" "$work/host.log" </dev/null >/dev/null 2>&1 &
host_pid=$!
sleep 1

# The installed script, ending in the probe instead of cuprated. Everything before it is unchanged.
sed 's|/usr/bin/nodo-confine -- /usr/bin/cuprated .*|/usr/bin/nodo-confine -- /usr/bin/env PROBE=sandbox python3 /var/lib/cuprated/probe.py /var/lib/cuprated/probe.json|' \
	"$run" > "$work/run"
diff "$run" "$work/run" | grep -c '^[<>]' | grep -qx 2
result "only the program line differs from the installed script" "$?"
(cd /etc/sv/cuprated && timeout 60 sh "$work/run") > "$work/sandbox.log" 2>&1
sandbox=$(cat "$data/probe.json" 2>/dev/null)
rm -f "$data/probe.json"
echo "  sandbox: $sandbox"
[ -n "$sandbox" ] || sed 's/^/  log: /' "$work/sandbox.log"
(cd "$data" && chpst -u _cuprated:_cuprated env PROBE=control python3 "$data/probe.py" "$work/control.json") 2>&1
control=$(cat "$work/control.json" 2>/dev/null)
echo "  control: $control"
has() { printf '%s' "$1" | grep -q "\"$2\": \"\\{0,1\\}$3"; }

has "$control" host_abstract connected && has "$control" unshare ok
result "control: outside the sandbox _cuprated reaches root's abstract socket and makes a user namespace" "$?"
has "$sandbox" host_abstract "socket: Permission denied" && has "$sandbox" own_abstract "socket: Permission denied"
result "inside the sandbox cuprated cannot even create a Unix socket" "$?"
has "$sandbox" unshare failed
result "unshare -U fails inside the sandbox" "$?"
has "$sandbox" shadow false && has "$sandbox" sv_dir false
result "host files such as /etc/shadow are not in the sandbox" "$?"
has "$sandbox" tcp ok
result "TCP sockets still work, which cuprated needs" "$?"
grep -qx "probe control" "$work/host.log" && ! grep -q "probe sandbox" "$work/host.log"
result "root's socket heard only the control" "$?"
sv status cuprated | grep -q '^run:'
result "cuprated still runs" "$?"
exit "$fail"
