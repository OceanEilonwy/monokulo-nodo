#!/bin/sh
# Container entry point: runs the VM until the container is stopped.
. /src/tests/_qemu-lib.sh
qemu_exec
