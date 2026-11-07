# Starts the VM for $BOARD on a throwaway qcow2 layer. Replaces the current process.
# restrict=on keeps tests offline, because a real mainnet sync would fill the test disk.
qemu_exec() {
	img=/src/out/images/$BOARD
	. "/src/boards/$BOARD/board.env"
	qemu-img create -q -f qcow2 -F raw -b "$img/disk.img" /tmp/overlay.qcow2
	exec $BOARD_QEMU -smp 4 -m 4096 -nographic \
		-kernel "$img/vmlinuz" -initrd "$img/initramfs.img" -append "$(cat "$img/cmdline.txt")" \
		-drive file=/tmp/overlay.qcow2,if=virtio,format=qcow2 \
		-netdev user,id=n0,restrict=on,hostfwd=tcp:127.0.0.1:2222-:22 -device virtio-net-pci,netdev=n0
}
