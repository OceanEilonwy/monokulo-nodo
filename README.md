# monokulo-nodo

A from-scratch Monero appliance for the FriendlyElec NanoPC-T6N, built on Void Linux and runit, with one
Rust codebase and signed xbps packages.

**Status: early development. It has not yet been proven on real hardware. Do not rely on it for funds.**

- [Architecture and decisions](docs/ARCHITECTURE.md)
- [TUI design](docs/design/tui/)

## Building

Docker is the only requirement for images. KVM speeds up service tests on x86_64.

```sh
scripts/test.sh fast                      # static checks, a few seconds
scripts/test.sh vm                        # changed packages in a running KVM VM
scripts/test.sh full                      # aarch64 image, cold boot, reboot and size checks
scripts/build-packages.sh aarch64 cuprated cuprated-service monokulo-base linux-nanopc-t6n
scripts/build-image.sh nanopc-t6n ~/.ssh/id_ed25519.pub
```

## License

GPL-3.0-or-later. Packaged software, such as Cuprate, keeps its own license.
