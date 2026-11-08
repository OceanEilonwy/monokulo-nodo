#!/bin/sh
# Container entry point: runs the VM until the container is stopped. `dev-vm.sh fresh` asks for a
# new VM by leaving /tmp/vm-fresh and stopping QEMU: the VM then boots again on a new overlay of
# the clean image, while files in the container, such as virtual USB drives, stay.
. /src/tests/_qemu-lib.sh
while :; do
	(qemu_exec)
	[ -f /tmp/vm-fresh ] || exit 0
	rm -f /tmp/vm-fresh
done
