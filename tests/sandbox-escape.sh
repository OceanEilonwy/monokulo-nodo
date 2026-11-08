#!/bin/sh
# Runs on the device under test, as root, with nodo-tools installed and python3 from the mirror
# (tests/acme-prepare.sh pushes it, or: scripts/dev-vm.sh mirror python3). It installs three apps
# through the real helper, so their run scripts come from nodo-gen, and sends real traffic:
#
#   sb-victim  listens on an abstract socket and on a socket file in /run/apps/sb-victim, which it
#              publishes as a setting
#   sb-client  references that setting, so its sandbox is granted the socket file
#   sb-plain   is granted no socket at all
#
# Inside their sandboxes, sb-client and sb-plain each run one probe: connect to the victim's
# abstract socket, to an abstract socket root holds on the host, to the victim's socket file and
# to an abstract socket of their own, then try `unshare -U`. The same probe run as the same user
# outside the sandbox is the control, showing a refusal comes from the sandbox.
#   scripts/dev-vm.sh ssh 'cat > /tmp/sandbox-escape.sh' < tests/sandbox-escape.sh
#   scripts/dev-vm.sh ssh 'sh /tmp/sandbox-escape.sh </dev/null'
set -u
fail=0
result() { if [ "$2" = 0 ]; then echo "PASS $1"; else echo "FAIL $1"; fail=1; fi; }
ctl() { nodo-tui --ctl "$@"; }
apps=/opt/nodo/apps
host_log=/tmp/sb-host.log

cleanup() {
	for id in sb-client sb-plain sb-victim; do
		[ -d "/etc/sv/$id" ] || continue
		touch "/etc/sv/$id/down"
		sv -w 15 down "$id" >/dev/null 2>&1
	done
	[ -n "${host_pid:-}" ] && kill "$host_pid" 2>/dev/null
	rm -f /tmp/sb-control.json /tmp/sb-host.py
}
trap cleanup EXIT

command -v python3 >/dev/null || { echo "FAIL python3 is needed: scripts/dev-vm.sh mirror python3"; exit 1; }
[ -L /var/service/nodo-helper ] || ln -s /etc/sv/nodo-helper /var/service/
for _ in $(seq 1 30); do ctl services >/dev/null 2>&1 && break; sleep 1; done
echo "kernel $(uname -r): $(nodo-confine --check)"

mkdir -p /var/lib/nodo/manifests "$apps/sb-victim/bin" "$apps/sb-client/bin" "$apps/sb-plain/bin"

# The victim: an abstract socket and a socket file, logging which one each line arrived on.
cat > "$apps/sb-victim/server.py" <<'EOF'
import os, socket, threading
log = "/var/lib/apps/sb-victim/received"
def serve(sock, kind):
    sock.listen(8)
    while True:
        conn, _ = sock.accept()
        line = conn.makefile().readline().strip()
        with open(log, "a") as f:
            f.write(kind + " " + line + "\n")
        conn.close()
a = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
a.bind("\0nodo-sb-victim")
path = "/run/apps/sb-victim/web.sock"
if os.path.exists(path):
    os.unlink(path)
f = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
f.bind(path)
os.chmod(path, 0o666)
threading.Thread(target=serve, args=(a, "abstract"), daemon=True).start()
serve(f, "file")
EOF
# The process name stays "run" for control/t, so python runs as a child.
printf '#!/bin/sh\npython3 /opt/nodo/apps/sb-victim/server.py &\nwhile :; do sleep 1; done\n' > "$apps/sb-victim/bin/run"
cat > "$apps/sb-victim/settings.lua" <<'EOF'
return { api_version = 1, settings = {
  { name = "socket", type = "string", label = "Socket", default = "unix:/run/apps/sb-victim/web.sock",
    endpoint = "unix", public = "socket" },
} }
EOF
cat > /var/lib/nodo/manifests/sb-victim.toml <<'EOF'
api_version = 1
id = "sb-victim"
version = "1.0"
exec = ["bin/run"]
settings = "settings.lua"
stop_timeout_secs = 5

[[web]]
name = "web"
upstream = "unix:/run/apps/sb-victim/web.sock"
EOF

# The probe, shared by both attackers and the control.
cat > /tmp/sb-probe.py <<'EOF'
import json, os, socket, subprocess, sys
who, out = sys.argv[1], sys.argv[2]
def connect(addr):
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    except OSError as e:
        return "socket: " + e.strerror
    try:
        s.settimeout(3)
        s.connect(addr)
        s.sendall(("probe " + who + "\n").encode())
        s.close()
        return "connected"
    except OSError as e:
        return "connect: " + (e.strerror or str(e))
res = {
    "victim_abstract": connect("\0nodo-sb-victim"),
    "host_abstract": connect("\0nodo-sb-host"),
    "victim_file": connect("/run/apps/sb-victim/web.sock"),
}
try:
    own = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    own.bind("\0nodo-sb-own-" + who)
    own.listen(1)
    res["own_abstract"] = connect("\0nodo-sb-own-" + who)
except OSError as e:
    res["own_abstract"] = "socket: " + e.strerror
r = subprocess.run(["unshare", "-U", "true"], capture_output=True, text=True)
res["unshare"] = "ok" if r.returncode == 0 else "failed: " + r.stderr.strip()
with open(out + ".tmp", "w") as f:
    json.dump(res, f)
os.rename(out + ".tmp", out)
EOF
for id in sb-client sb-plain; do
	cp /tmp/sb-probe.py "$apps/$id/probe.py"
	printf '#!/bin/sh\npython3 /opt/nodo/apps/%s/probe.py %s /var/lib/apps/%s/probe.json\nwhile :; do sleep 1; done\n' "$id" "$id" "$id" > "$apps/$id/bin/run"
	printf 'api_version = 1\nid = "%s"\nversion = "1.0"\nexec = ["bin/run"]\nstop_timeout_secs = 5\n' "$id" > "/var/lib/nodo/manifests/$id.toml"
done
printf 'settings = "settings.lua"\n' >> /var/lib/nodo/manifests/sb-client.toml
cat > "$apps/sb-client/settings.lua" <<'EOF'
return { api_version = 1, settings = {
  { name = "victim", type = "string", label = "Victim socket", default = "$(sb-victim.socket)" },
} }
EOF
chmod -R a+rX "$apps/sb-victim" "$apps/sb-client" "$apps/sb-plain"
chmod 755 "$apps"/sb-*/bin/run

# Root's abstract socket on the host, standing in for any host daemon.
cat > /tmp/sb-host.py <<'EOF'
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind("\0nodo-sb-host")
s.listen(8)
while True:
    c, _ = s.accept()
    with open(sys.argv[1], "a") as f:
        f.write(c.makefile().readline())
    c.close()
EOF
: > "$host_log"
python3 /tmp/sb-host.py "$host_log" </dev/null >/dev/null 2>&1 &
host_pid=$!

for id in sb-victim sb-client sb-plain; do
	rm -f "/etc/sv/$id/down" "/var/lib/apps/$id/probe.json" "/var/lib/apps/$id/received"
done
ctl install sb-victim >/tmp/sb.out 2>&1
result "the victim installs through the helper" "$?"
for _ in $(seq 1 20); do [ -S /run/apps/sb-victim/web.sock ] && break; sleep 1; done
if [ -S /run/apps/sb-victim/web.sock ]; then r=0; else r=1; fi
result "the victim listens on its socket file" "$r"
ctl install sb-client >>/tmp/sb.out 2>&1 && ctl install sb-plain >>/tmp/sb.out 2>&1
r=$?; [ "$r" = 0 ] || cat /tmp/sb.out
result "both probes install through the helper" "$r"

grep -q -- '--unshare-user --disable-userns' /etc/sv/sb-client/run &&
	grep -q '/usr/bin/nodo-confine --unix -- /opt/nodo/apps/sb-client/bin/run' /etc/sv/sb-client/run &&
	grep -q '/usr/bin/nodo-confine -- /opt/nodo/apps/sb-plain/bin/run' /etc/sv/sb-plain/run &&
	grep -q -- '--ro-bind /run/apps/sb-victim /run/apps/sb-victim' /etc/sv/sb-client/run &&
	! grep -q /run/apps/sb-victim /etc/sv/sb-plain/run
result "generated scripts: a socket grant only for sb-client, hardening for both" "$?"

for id in sb-client sb-plain; do
	for _ in $(seq 1 30); do [ -s "/var/lib/apps/$id/probe.json" ] && break; sleep 1; done
	echo "  $id: $(cat "/var/lib/apps/$id/probe.json" 2>/dev/null)"
done
chpst -u _sb-client:_sb-client python3 /tmp/sb-probe.py control /tmp/sb-control.json
echo "  control: $(cat /tmp/sb-control.json 2>/dev/null)"
has() { grep -q "\"$2\": \"$3" "$1"; }

c=/tmp/sb-control.json
has "$c" victim_abstract connected && has "$c" host_abstract connected && has "$c" unshare ok
result "control: outside the sandbox the same user reaches both abstract sockets and makes a user namespace" "$?"

p=/var/lib/apps/sb-client/probe.json
if nodo-confine --check | grep -q abstract_scope=yes; then
	has "$p" victim_abstract "connect: Operation not permitted"
	result "sb-client cannot reach the victim's abstract socket" "$?"
	has "$p" host_abstract "connect: Operation not permitted"
	result "sb-client cannot reach root's abstract socket on the host" "$?"
else
	echo "SKIP sb-client abstract checks: this kernel has no Landlock scoping"
fi
has "$p" victim_file connected
result "sb-client still reaches the socket file it was granted" "$?"
has "$p" own_abstract connected
result "sb-client's own abstract socket works" "$?"
has "$p" unshare failed
result "unshare -U fails inside sb-client's sandbox" "$?"

p=/var/lib/apps/sb-plain/probe.json
has "$p" victim_abstract "socket: Permission denied" && has "$p" host_abstract "socket: Permission denied"
result "sb-plain cannot even create a Unix socket" "$?"
has "$p" victim_file "socket: Permission denied"
result "sb-plain cannot reach the socket file it was not granted" "$?"
has "$p" unshare failed
result "unshare -U fails inside sb-plain's sandbox" "$?"

echo "  victim received: $(tr '\n' ';' < /var/lib/apps/sb-victim/received 2>/dev/null)"
grep -qx "file probe sb-client" /var/lib/apps/sb-victim/received && grep -qx "abstract probe control" /var/lib/apps/sb-victim/received &&
	! grep -q "abstract probe sb-" /var/lib/apps/sb-victim/received && ! grep -q "probe sb-plain" /var/lib/apps/sb-victim/received
result "the victim heard only the granted connection and the control" "$?"
grep -qx "probe control" "$host_log" && ! grep -q "probe sb-" "$host_log"
result "root's socket heard only the control" "$?"

for id in sb-victim sb-client sb-plain; do
	sv status "$id" | grep -q "^run:" || { sv status "$id"; r=1; }
done
result "all three services still run" "${r:-0}"
sv -w 30 stop sb-client | grep -q '^ok: down'
result "a confined service still stops cleanly through control/t" "$?"
exit "$fail"
