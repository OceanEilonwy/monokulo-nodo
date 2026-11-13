#!/bin/sh
# Checks /etc/profile.d/nodo-tui.sh: it opens nodo-tui only for interactive SSH logins on a
# terminal, never for commands, never twice, and falls back to the shell when nodo-tui fails.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
profile=$root/packages/nodo-tools/files/profile/nodo-tui.sh
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin" "$tmp/home"
cat > "$tmp/bin/nodo-tui" <<'X'
#!/bin/sh
echo opened >> "$NODO_TEST_LOG"
exit "${NODO_TEST_EXIT:-0}"
X
chmod +x "$tmp/bin/nodo-tui"
fail=0
# run <expected opens> <description> <env...>: sources the profile in an interactive shell on a
# pseudo terminal (script(1)), so that -t 0 and -t 1 hold as they do for a real login.
run() {
	want=$1 what=$2
	shift 2
	: > "$tmp/log"
	env -i PATH="$tmp/bin:/usr/bin:/bin" HOME="$tmp/home" TERM=xterm NODO_TEST_LOG="$tmp/log" "$@" \
		script -qec "sh -ic '. $profile; true'" /dev/null > "$tmp/out" 2>&1 </dev/null || true
	got=$(wc -l < "$tmp/log" | tr -d ' ')
	if [ "$got" = "$want" ]; then echo "ok: $what"; else echo "FAIL: $what (opened $got times, wanted $want)"; cat "$tmp/out"; fail=1; fi
}
run 1 "an SSH login opens nodo-tui" SSH_TTY=/dev/pts/0
run 0 "a local login does not" 
run 0 "NODO_NO_TUI skips it" SSH_TTY=/dev/pts/0 NODO_NO_TUI=1
run 0 "a shell started from the first does not open it again" SSH_TTY=/dev/pts/0 NODO_TUI_SHOWN=1
run 0 "a dumb terminal does not" SSH_TTY=/dev/pts/0 TERM=dumb
touch "$tmp/home/.hushtui"
run 0 "a .hushtui file in HOME skips it" SSH_TTY=/dev/pts/0
rm "$tmp/home/.hushtui"
run 1 "a failing nodo-tui still leaves the shell" SSH_TTY=/dev/pts/0 NODO_TEST_EXIT=3
grep -q "This is the Nodo's shell" "$tmp/out" || { echo "FAIL: no shell message after a failure"; fail=1; }
# Not interactive (as for `ssh nodo command`): nothing opens.
: > "$tmp/log"
env -i PATH="$tmp/bin:/usr/bin:/bin" HOME="$tmp/home" TERM=xterm SSH_TTY=/dev/pts/0 NODO_TEST_LOG="$tmp/log" \
	sh -c ". $profile; true" </dev/null >/dev/null 2>&1
[ ! -s "$tmp/log" ] && echo "ok: a non-interactive shell does not" || { echo "FAIL: a non-interactive shell opened it"; fail=1; }
exit $fail
