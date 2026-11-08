# Sourced by tests/checks.sh for boards that set BOARD_KERNEL_CHECKS=1. Checks the kernel features
# monokulo-nodo relies on beyond booting: Landlock scoping as nodo-confine uses it, LUKS2, XFS and
# overlayfs. Expects ssh_vm, check and fail. tests/build-landlock-probe.sh must have run first.
probe=/src/build/landlock-probe
if [ -s "$probe/confine-path.txt" ] && [ -x "$probe/confine-test" ] && [ -x "$probe/nodo-confine" ]; then
	# The test binary runs nodo-confine from the path it was built at, so it goes there.
	confine=$(cat "$probe/confine-path.txt")
	ssh_vm "mkdir -p '$(dirname "$confine")' && cat > '$confine' && chmod 755 '$confine'" < "$probe/nodo-confine"
	ssh_vm 'cat > /tmp/confine-test && chmod 755 /tmp/confine-test' < "$probe/confine-test"
	ssh_vm "'$confine' --check" | sed 's/^/  /'
	check "landlock is an active LSM" 'grep -qw landlock /sys/kernel/security/lsm'
	check "landlock ABI is 6 or newer, with abstract socket scoping" \
		"'$confine' --check | grep -Eq '^landlock_abi=([6-9]|[1-9][0-9]+) abstract_scope=yes\$'"
	# The integration tests skip the scoping assertions on a kernel without scoping, so a skip fails here.
	check "nodo-confine blocks abstract sockets outside its sandbox" \
		'out=$(/tmp/confine-test --test-threads 1 --nocapture 2>&1); r=$?; echo "$out" | grep -E "^test |test result"
		[ "$r" = 0 ] && ! echo "$out" | grep -q "skipped:" && echo "$out" | grep -q "test result: ok. 5 passed" && exit 0
		echo "exit $r"; echo "$out" | tail -n 30; exit 1'
	check "nodo-confine blocks signals to processes outside its sandbox" \
		"kill -0 1 && '$confine' --unix -- /bin/sh -c 'kill -0 \$\$ && ! kill -0 1 2>/dev/null'"
else
	echo "FAIL landlock probe binaries are missing: run tests/build-landlock-probe.sh"
	fail=1
fi
check "LUKS2 and XFS on a loop device" 'set -e
f=/var/tmp/luks-check.img; truncate -s 400M $f; d=$(losetup -f --show $f)
printf pw | cryptsetup luksFormat --type luks2 --pbkdf pbkdf2 --pbkdf-force-iterations 1000 --batch-mode --key-file - $d
printf pw | cryptsetup open --key-file - $d kcheck
mkfs.xfs -q /dev/mapper/kcheck; mkdir -p /mnt/kcheck; mount /dev/mapper/kcheck /mnt/kcheck
echo ok > /mnt/kcheck/t; umount /mnt/kcheck; cryptsetup close kcheck; losetup -d $d; rm -f $f'
check "overlayfs mounts" 'set -e; b=/var/tmp/ovl; mkdir -p $b/l $b/u $b/w $b/m
mount -t overlay overlay -o lowerdir=$b/l,upperdir=$b/u,workdir=$b/w $b/m; umount $b/m; rm -rf $b'
