#!/bin/sh
set -u
. /src/tests/_vm-lib.sh
fail=0
check() { name=$1; shift; if ssh_vm "$@"; then echo "PASS $name"; else echo "FAIL $name"; fail=1; fi; }
. /src/tests/checks.sh
exit "$fail"
