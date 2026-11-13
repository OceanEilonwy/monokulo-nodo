# monokulo-nodo

A from-scratch Monero appliance for the FriendlyElec NanoPC-T6N, built on Void Linux and runit, with one
Rust codebase and signed xbps packages.

**Status: early development. It has not yet been proven on real hardware. Do not rely on it for funds.**

- [Architecture and decisions](docs/ARCHITECTURE.md)
- [The app feed](docs/FEED.md), published at https://nodo.monokulo.com
- [TUI design](docs/design/tui/)

## Building

Docker is the only requirement for images. KVM speeds up service tests on x86_64.

```sh
scripts/test.sh fast                      # static checks, a few seconds
scripts/test.sh vm                        # changed packages in a running KVM VM
scripts/test.sh full                      # aarch64 image, cold boot, reboot and size checks
scripts/build-packages.sh aarch64 cuprated cuprated-service nodo-base linux-nanopc-t6n
scripts/build-image.sh nanopc-t6n ~/.ssh/id_ed25519.pub
# The same image on the mainline longterm kernel, see docs/MAINLINE_KERNEL.md
scripts/build-packages.sh aarch64 linux-mainline-rk3588
scripts/build-image.sh nanopc-t6n-mainline ~/.ssh/id_ed25519.pub
scripts/build-image.sh qemu-virt-mainline build/test-key.pub && tests/qemu-boot-test.sh qemu-virt-mainline
tests/inspect-t6n-mainline.sh out/images/nanopc-t6n-mainline/monokulo-nanopc-t6n-mainline-sd.img \
	out/images/nanopc-t6n/monokulo-nanopc-t6n-sd.img
```

- [Hardware test](docs/HARDWARE_TEST.md) and [mainline kernel notes](docs/MAINLINE_KERNEL.md)

## License

GPL-3.0-or-later. Packaged software, such as Cuprate, keeps its own license.
