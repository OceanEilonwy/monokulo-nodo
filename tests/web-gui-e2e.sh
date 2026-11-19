#!/bin/sh
# End-to-end test of the web GUI inside the dev VM, as root: the real nodo-helper on its real
# sockets, the real nodo-web-gui, and curl speaking to it as Caddy does (on a Unix socket, by the
# name nodo.local). Needs nodo-tools and the web packages installed, and curl. It turns the web
# GUI on and off, sets up the device passphrase when the device has none, and changes one cuprated
# setting, so run it on a disposable VM:
#   scripts/dev-vm.sh push nodo-tools nodo-caddy nodo-localnet nodo-ddns
#   scripts/dev-vm.sh mirror curl             (from the pinned Void mirror)
#   scripts/dev-vm.sh ssh 'cat > /tmp/web-gui-e2e.sh' < tests/web-gui-e2e.sh
#   scripts/dev-vm.sh ssh 'sh /tmp/web-gui-e2e.sh </dev/null'
# After tests/helper-e2e.sh, which leaves the device with another passphrase, name it:
#   scripts/dev-vm.sh ssh 'WEBGUI_E2E_PASSPHRASE="new horse battery staple" sh /tmp/web-gui-e2e.sh </dev/null'
#
# The helper serves its web socket to the web GUI's account alone, so the test runs a second
# nodo-web-gui of its own under that account, on a socket of its own. That one is there before
# the web GUI is turned on and after it is turned off, which is when the helper must refuse it;
# the service the helper starts and stops is checked beside it.
set -u
fail=0
work=/tmp/webgui-e2e
out=$work/out
rm -rf "$work"
mkdir -p "$work/bin"
pass() { echo "PASS $1"; }
bad() { echo "FAIL $1"; fail=1; sed 's/^/    /' "$out" | head -n 25; }
# Runs a test body in a subshell. Its output is shown only on failure.
check() { name=$1; shift; if ( "$@" ) >"$out" 2>&1; then pass "$name"; else bad "$name"; fi; }
# Succeeds when the command fails and its output contains the text.
refused() { text=$1; shift; if "$@" >"$work/r" 2>&1; then cat "$work/r"; return 1; fi; cat "$work/r"; grep -q "$text" "$work/r"; }
PASS=${WEBGUI_E2E_PASSPHRASE:-correct horse battery}
WRONG='wrong horse battery'
ACCOUNT=_nodo_webgui
HELPER_WEB=/run/nodo/webgui.sock
OWN=$work/run/http.sock
SERVICE=/run/nodo-web-gui/http.sock

# Commands rather than functions, so the sh -c bodies below can use them too.
printf '#!/bin/sh\nexec nodo-tui --ctl "$@"\n' > "$work/bin/ctl"
# The helper's web socket, as the web GUI's account.
printf '#!/bin/sh\nexec chpst -u %s:%s nodo-tui --socket %s --ctl "$@"\n' "$ACCOUNT" "$ACCOUNT" "$HELPER_WEB" > "$work/bin/webctl"
# nodo-tui --ctl prints an answer over several lines; this puts it on one, to match a whole status.
printf '#!/bin/sh\ntr -d "\\n" | tr -s " "\n' > "$work/bin/flat"
# web SOCKET PATH [curl arguments]: one request as Caddy passes it on. Prints the status, or
# ERR<curl exit code> when no answer came; the headers and the body go to files.
cat > "$work/bin/web" <<'EOF'
#!/bin/sh
sock=$1; path=$2; shift 2
: > /tmp/webgui-e2e/body
code=$(curl -s -o /tmp/webgui-e2e/body -D /tmp/webgui-e2e/headers -w '%{http_code}' --max-time 150 \
	--unix-socket "$sock" -H 'Host: nodo.local' "$@" "http://nodo.local$path" 2>/dev/null)
rc=$?
if [ "$rc" != 0 ]; then echo "ERR$rc"; else echo "$code"; fi
EOF
# post SOCKET PATH [curl arguments]: the same for a form, sent by a page of the Nodo's own.
cat > "$work/bin/post" <<'EOF'
#!/bin/sh
sock=$1; path=$2; shift 2
exec web "$sock" "$path" -X POST -H 'Origin: https://nodo.local' "$@"
EOF
# login ADDRESS PASSPHRASE: a login from one browser's address, as Caddy reports it.
cat > "$work/bin/login" <<'EOF'
#!/bin/sh
exec post /tmp/webgui-e2e/run/http.sock /login -H "X-Forwarded-For: $1" --data-urlencode "passphrase=$2"
EOF
# What a header of the last answer said.
cat > "$work/bin/header" <<'EOF'
#!/bin/sh
tr -d '\r' < /tmp/webgui-e2e/headers | sed -n "s/^$1: *//Ip" | head -n 1
EOF
chmod 755 "$work/bin/ctl" "$work/bin/webctl" "$work/bin/flat" "$work/bin/web" "$work/bin/post" "$work/bin/login" "$work/bin/header"
PATH=$work/bin:$PATH
export PATH

wait_helper() { for _ in $(seq 1 30); do [ -S /run/nodo/helper.sock ] && ctl services >/dev/null 2>&1 && return 0; sleep 1; done; return 1; }
own_pid=
cleanup() {
	[ -z "$own_pid" ] || kill "$own_pid" 2>/dev/null
	ctl web-gui 0 >/dev/null 2>&1
	[ -z "${old_peers:-}" ] || ctl set cuprated outbound_peers int "$old_peers" >/dev/null 2>&1
}
trap cleanup EXIT

# What it needs

[ -L /var/service/nodo-helper ] || ln -s /etc/sv/nodo-helper /var/service/
check "helper answers on its socket" wait_helper
# Off to begin with, whatever an earlier run left.
ctl web-gui 0 >/dev/null 2>&1
check "the web GUI is installed with its account, its service and its firewall rules" sh -c '
	command -v nodo-web-gui && command -v curl && id "$0" && [ -x /etc/sv/nodo-web-gui/run ] && [ -s /etc/nftables.d/nodo-web-gui.nft ]' "$ACCOUNT"
check "the helper's web socket belongs to the web GUI's account alone" sh -c '
	for _ in $(seq 1 15); do [ -S "$1" ] && break; sleep 1; done
	[ "$(stat -c "%a %U" "$1")" = "600 $0" ]' "$ACCOUNT" "$HELPER_WEB"
check "its service does not run while it is off" sh -c '
	for _ in $(seq 1 15); do sv status nodo-web-gui 2>/dev/null | grep -q "^run:" || exit 0; sleep 1; done; sv status nodo-web-gui; exit 1'
if [ "$fail" != 0 ]; then echo "the web GUI is not installed here; nothing else can be tested"; exit 1; fi

# The device passphrase is its login

if ctl overview | grep -q NotSetUp; then
	check "before the device passphrase is set up, it cannot be turned on" refused "NotSetUp" ctl web-gui 1
	check "and its status stays off" sh -c 'ctl web-gui-status | flat | grep -q "enabled: false"'
	check "first-boot setup creates the device key" sh -c 'printf "%s\n" "$0" | nodo-tui --ctl init' "$PASS"
else
	echo "the device is set up already; logging in with WEBGUI_E2E_PASSPHRASE or the default"
fi
# A right passphrase also clears the count of wrong unlocks, which is measured below.
check "the device passphrase unlocks the device" sh -c 'printf "%s\n" "$0" | nodo-tui --ctl unlock' "$PASS"
ctl web-gui 0 >/dev/null 2>&1

# Off: no service, and the helper refuses a web GUI that runs all the same

install -d -m 750 -o "$ACCOUNT" -g "$ACCOUNT" "$work/run"
chmod 755 "$work"
chpst -u "$ACCOUNT:$ACCOUNT" nodo-web-gui --socket "$HELPER_WEB" --listen-unix "$OWN" > "$work/own.log" 2>&1 &
own_pid=$!
check "a web GUI started by hand answers on its Unix socket" sh -c '
	for _ in $(seq 1 20); do [ -S "$0" ] && break; sleep 1; done
	[ "$(web "$0" /login)" = 200 ] && grep -q "Log in to your Nodo" /tmp/webgui-e2e/body' "$OWN"
check "its socket is closed to every other account" sh -c '[ "$(stat -c "%a" "$0")" = 660 ] && ! chpst -u nobody test -r "$0"' "$OWN"
check "every answer forbids caching and outside scripts" sh -c '
	web "$0" /login >/dev/null; [ "$(header cache-control)" = no-store ] && header content-security-policy | grep -q "default-src" &&
	[ "$(header referrer-policy)" = same-origin ]' "$OWN"
check "a request by another name than nodo.local is refused" sh -c '
	code=$(curl -s -o /dev/null -w "%{http_code}" --max-time 20 --unix-socket "$0" -H "Host: 192.168.77.3" http://nodo.local/login); echo "$code"; [ "$code" = 421 ]' "$OWN"
check "a form that no page of the Nodo sent is refused" sh -c '
	[ "$(web "$0" /login -X POST --data-urlencode "passphrase=$1")" = 403 ] &&
	[ "$(web "$0" /login -X POST -H "Origin: https://evil.example" --data-urlencode "passphrase=$1")" = 403 ]' "$OWN" "$PASS"
check "status says it is off" sh -c 'ctl web-gui-status | flat | tee /dev/stderr | grep -q "enabled: false, url: None, waiting: false, open: false"'
check "while it is off the helper tells its own account only that, and answers nothing else" sh -c '
	webctl web-gui-status | flat | tee /dev/stderr | grep -q "enabled: false, url: None, waiting: false, open: false" &&
	webctl services 2>&1 | tee /dev/stderr | grep -q "web login required"'
check "while it is off the right passphrase is refused and makes no session" sh -c '
	code=$(login 192.168.77.20 "$0"); echo "$code"; cat /tmp/webgui-e2e/body
	[ "$code" = 503 ] && grep -q "The web GUI is turned off" /tmp/webgui-e2e/body && [ -z "$(header set-cookie)" ]' "$PASS"
check "while it is off no page is served" sh -c '[ "$(web "$0" /)" = 303 ] && [ "$(header location)" = /login ]' "$OWN"
# The socket's file may be left from an earlier run of the service (its run script removes it at
# the next start), so what is checked is that nothing answers on it.
check "while it is off the helper's service is down and nothing answers on its socket" sh -c '
	! sv status nodo-web-gui 2>/dev/null | grep -q "^run:" &&
	case $(web "$0" /login) in ERR*) true ;; *) false ;; esac' "$SERVICE"

# On, over SSH

check "nodo-tui --ctl web-gui 1 turns it on" sh -c 'ctl web-gui 1 | flat | tee /dev/stderr | grep -q "enabled: true"'
# Without an address on the home network it is on and waits; with one it says where it answers.
check "status says it is on, with its address or that it waits for one" sh -c '
	ctl web-gui-status | flat | tee /dev/stderr |
		grep -Eq "enabled: true, url: (Some\( \"https://nodo.local\", \), waiting: false|None, waiting: true), open: false"'
check "the helper started its service, on a socket only Caddy's group may open" sh -c '
	for _ in $(seq 1 30); do [ -S "$0" ] && sv status nodo-web-gui | grep -q "^run:" && break; sleep 1; done
	sv status nodo-web-gui; stat -c "%a %U %G" "$0" /run/nodo-web-gui
	[ "$(stat -c "%a %U" "$0")" = "660 $1" ] && [ "$(stat -c "%a %U" /run/nodo-web-gui)" = "2750 $1" ] &&
	[ "$(web "$0" /login)" = 200 ] && ! chpst -u nobody test -r "$0"' "$SERVICE" "$ACCOUNT"
check "the service runs as its account, confined" sh -c '
	p=$(pgrep -u "$0" -f "^/usr/bin/nodo-web-gui$" | head -n 1); [ -n "$p" ] && grep -q "^Seccomp:[[:space:]]*2" "/proc/$p/status" &&
	grep -q "^NoNewPrivs:[[:space:]]*1" "/proc/$p/status"' "$ACCOUNT"
check "before a login the helper still answers its account nothing" sh -c '
	webctl web-gui-status | flat | tee /dev/stderr | grep -q "enabled: true, url: None, waiting: false, open: false" &&
	webctl services 2>&1 | grep -q "web login required"'

# Wrong passphrases

check "a wrong passphrase is 401, says so and sets no cookie" sh -c '
	code=$(login 192.168.77.21 "$0"); echo "$code"
	[ "$code" = 401 ] && grep -q "That passphrase did not unlock this device" /tmp/webgui-e2e/body && [ -z "$(header set-cookie)" ] &&
	! grep -q "$0" /tmp/webgui-e2e/body' "$WRONG"
check "the same address must wait before its next try, which the helper never sees" sh -c '
	code=$(login 192.168.77.21 "$0"); echo "$code"; [ "$code" = 429 ] && [ -z "$(header set-cookie)" ]' "$PASS"
# The helper slows wrong guesses down: 2 s after the first, 4 s after the second, 8 s after the
# third. Web logins and unlocks over SSH each have their own count, so two wrong web logins leave
# the first wrong unlock at 2 s. With one shared count it would be the third, at 8 s.
check "wrong web logins are counted apart from wrong unlocks" sh -c '
	code=$(login 192.168.77.22 "$0"); echo "second wrong login: $code"; [ "$code" = 401 ] || exit 1
	start=$(date +%s); printf "%s\n" "$0" | nodo-tui --ctl unlock > /tmp/webgui-e2e/unlock 2>&1; status=$?
	took=$(( $(date +%s) - start )); cat /tmp/webgui-e2e/unlock; echo "the wrong unlock took ${took}s"
	[ "$status" -ne 0 ] && grep -q BadPassphrase /tmp/webgui-e2e/unlock && [ "$took" -lt 6 ]' "$WRONG"
check "and a right unlock does not clear the web login's count: the third wrong login waits 8 s" sh -c '
	printf "%s\n" "$1" | nodo-tui --ctl unlock >/dev/null || exit 1
	start=$(date +%s); code=$(login 192.168.77.23 "$0"); took=$(( $(date +%s) - start ))
	echo "third wrong login: $code after ${took}s"; [ "$code" = 401 ] && [ "$took" -ge 8 ]' "$WRONG" "$PASS"
check "nothing of a passphrase is in the helper's or the web GUI's log" sh -c '
	! grep -q "horse battery" /var/log/nodo-helper/current /var/log/nodo-web-gui/current /tmp/webgui-e2e/own.log 2>/dev/null'

# A session

session() { cat "$work/cookie"; }
log_in() {
	code=$(login 192.168.77.30 "$PASS"); echo "$code"; cat "$work/headers"
	[ "$code" = 303 ] && [ "$(header location)" = / ] || return 1
	cookie=$(header set-cookie)
	# Only for this origin, only over HTTPS, never to a script, never from another site.
	for part in "__Host-nodo=" "Secure" "HttpOnly" "SameSite=Strict" "Path=/"; do
		case $cookie in *"$part"*) ;; *) echo "the cookie lacks $part"; return 1 ;; esac
	done
	printf '%s' "${cookie%%;*}" > "$work/cookie"
	[ "$(wc -c < "$work/cookie")" -gt 40 ]
}
check "the device passphrase logs in and gives a session cookie" log_in
check "the helper's gate is open for its account now, and never takes its own switch" sh -c '
	webctl web-gui-status | flat | tee /dev/stderr | grep -q "enabled: true.*open: true" && webctl services | grep -q cuprated &&
	webctl web-gui 0 2>&1 | tee /dev/stderr | grep -q "Err(Denied)" && ctl web-gui-status | flat | grep -q "enabled: true"'
page() { [ "$(web "$OWN" "$1" -H "Cookie: $(session)")" = 200 ] && grep -q "$2" "$work/body"; }
check "Home is served to the session" page / "<nav class=\"tabs\""
check "an app's page shows the real device" page /apps/cuprated "cuprated"
check "System shows the web GUI as on, without a switch" sh -c '
	[ "$(web "$0" /system -H "Cookie: $1")" = 200 ] && grep -q "Web GUI" /tmp/webgui-e2e/body && grep -q "● </span>On" /tmp/webgui-e2e/body &&
	! grep -qi "set-web-gui\|Turn it off" /tmp/webgui-e2e/body' "$OWN" "$(session 2>/dev/null)"
check "without the cookie, or with another, no page is served" sh -c '
	[ "$(web "$0" /apps)" = 303 ] && [ "$(header location)" = /login ] &&
	[ "$(web "$0" /apps -H "Cookie: __Host-nodo=0000000000000000000000000000000000000000000000000000000000000000")" = 303 ]' "$OWN"

# One setting, changed from the browser

old_peers=$(sed -n 's/^outbound_connections = //p' /etc/cuprated/Cuprated.toml 2>/dev/null | head -n 1)
new_peers=37
[ "${old_peers:-}" != 37 ] || new_peers=38
change_setting() {
	[ "$(web "$OWN" /apps/cuprated/settings -H "Cookie: $(session)")" = 200 ] && grep -q 'name="f.outbound_peers"' "$work/body" || return 1
	code=$(post "$OWN" /apps/cuprated/settings -H "Cookie: $(session)" --data-urlencode "f.outbound_peers=$new_peers" --data-urlencode do=save)
	echo "save: $code to $(header location)"
	[ "$code" = 303 ] && [ "$(header location)" = /working ] || return 1
	# The app restarts with the new setting, which the page waits for.
	for _ in $(seq 1 120); do
		code=$(web "$OWN" /working -H "Cookie: $(session)")
		[ "$code" = 303 ] && break
		[ "$code" = 200 ] || { echo "waiting answered $code"; return 1; }
		sleep 1
	done
	echo "done: $code to $(header location)"
	[ "$code" = 303 ] && grep -qx "outbound_connections = $new_peers" /etc/cuprated/Cuprated.toml &&
		ctl form cuprated | grep -q "$new_peers"
}
check "a cuprated setting saved in the browser reaches Cuprated.toml" change_setting
# Refused by the page itself (422), or sent and refused by the helper: either way nothing changes.
check "a value out of range is refused, and nothing changes" sh -c '
	code=$(post "$0" /apps/cuprated/settings -H "Cookie: $1" --data-urlencode "f.outbound_peers=500" --data-urlencode do=save)
	echo "$code to $(header location)"
	if [ "$code" = 303 ] && [ "$(header location)" = /working ]; then
		for _ in $(seq 1 120); do [ "$(web "$0" /working -H "Cookie: $1")" = 303 ] && break; sleep 1; done
	else
		[ "$code" = 422 ] || exit 1
	fi
	grep -qx "outbound_connections = $2" /etc/cuprated/Cuprated.toml && ! ctl form cuprated | grep -q 500' \
	"$OWN" "$(session 2>/dev/null)" "$new_peers"

# Off again: every session ends, the gate closes, the service and its socket go

check "nodo-tui --ctl web-gui 0 turns it off" sh -c 'ctl web-gui 0 | flat | tee /dev/stderr | grep -q "enabled: false"'
# The service's stop ends every nodo-web-gui of its account (control/t: pkill -u ... -x), and this
# test's own one with it. Start it again, so there is a web GUI for the helper to refuse; the
# cookie from before is then one this process never gave out, which must lead to the login too.
check "turning it off ended the web GUI started by hand as well" sh -c '
	for _ in $(seq 1 10); do kill -0 "$0" 2>/dev/null || exit 0; sleep 1; done; false' "$own_pid"
rm -f "$OWN"
chpst -u "$ACCOUNT:$ACCOUNT" nodo-web-gui --socket "$HELPER_WEB" --listen-unix "$OWN" > "$work/own.log" 2>&1 &
own_pid=$!
check "started again by hand, it answers" sh -c '
	for _ in $(seq 1 20); do [ -S "$0" ] && break; sleep 1; done
	[ "$(web "$0" /login)" = 200 ]' "$OWN"
check "the session is dead: its cookie is sent back to the login" sh -c '
	[ "$(web "$0" / -H "Cookie: $1")" = 303 ] && [ "$(header location)" = /login ] &&
	[ "$(web "$0" /apps/cuprated -H "Cookie: $1")" = 303 ] && [ "$(header location)" = /login ]' "$OWN" "$(session 2>/dev/null)"
check "a post with the dead session changes nothing" sh -c '
	code=$(post "$0" /apps/cuprated/restart -H "Cookie: $1"); echo "$code to $(header location)"
	[ "$code" = 303 ] && [ "$(header location)" = /login ]' "$OWN" "$(session 2>/dev/null)"
check "the helper's gate is closed to its account again" sh -c '
	webctl web-gui-status | flat | tee /dev/stderr | grep -q "enabled: false, url: None, waiting: false, open: false" &&
	webctl services 2>&1 | tee /dev/stderr | grep -q "web login required"'
check "the right passphrase is refused again" sh -c '
	code=$(login 192.168.77.31 "$0"); echo "$code"; [ "$code" = 503 ] && grep -q "The web GUI is turned off" /tmp/webgui-e2e/body && [ -z "$(header set-cookie)" ]' "$PASS"
check "the helper stopped its service, and the socket Caddy used no longer answers" sh -c '
	for _ in $(seq 1 20); do sv status nodo-web-gui 2>/dev/null | grep -q "^run:" || break; sleep 1; done
	sv status nodo-web-gui; ! sv status nodo-web-gui 2>/dev/null | grep -q "^run:" && ! pgrep -u "$1" -f "^/usr/bin/nodo-web-gui$" &&
	case $(web "$0" /login) in ERR*) true ;; *) false ;; esac' "$SERVICE" "$ACCOUNT"
check "status says it is off, and it stays off across a helper restart" sh -c '
	ctl web-gui-status | flat | grep -q "enabled: false" && sv -w 30 restart nodo-helper >/dev/null && sleep 1 &&
	for _ in $(seq 1 30); do ctl services >/dev/null 2>&1 && break; sleep 1; done
	ctl web-gui-status | flat | tee /dev/stderr | grep -q "enabled: false" && ! sv status nodo-web-gui 2>/dev/null | grep -q "^run:"'

if [ "$fail" = 0 ]; then echo "web GUI end to end: all passed"; else echo "web GUI end to end: FAILED"; fi
exit "$fail"
