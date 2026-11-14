# Updates, rollback, health and backups

Status: design plus the parts that can be tested without hardware. Sections say what is
implemented, what was proven in the KVM dev VM, and what is still unproven on the NanoPC-T6N.

## 1. System updates from tested snapshots

Void is a rolling distribution, so a device never follows `current` directly. A bad upstream
update must stop at the publisher.

**Publisher** (`scripts/publish-snapshot.sh`). The update server is plain static files under
`$SNAPSHOT_ROOT` (default `out/`), served over HTTPS:

```text
snapshots/<name>/current[/aarch64]        Void's repository, exactly as Void signed it
snapshots/<name>/nodo/<arch>              this project's packages, signed with the owner's xbps key
snapshots/<name>/manifest.toml{,.sig}
channels/<channel>/manifest.toml{,.sig}   the channel's newest snapshot
channels/<channel>/keys.toml{,.sig}       an optional key rotation statement
```

1. `fetch <name> <arch>...` copies Void's `<arch>-repodata` exactly as Void signed it, plus every
   package an image needs. Packages are fetched by `xbps-install -D`, so each one is checked
   against Void's index and `.sig2` signature. `import <name> <arch> <file>` takes an index
   fetched elsewhere, without packages, which suits tests and snapshots where no Void package
   changes.
2. `nodo <name> <arch> <xbps-key.pem> [dir]` adds this project's packages (default `out/feed/<arch>`)
   as a second repository, indexed and signed with `xbps-rindex` in the builder container.
3. `test <name> <board>` builds an image using only the snapshot's two repositories, and for
   `qemu-virt` boots it with the existing boot test. A stamp records the hash of every index tested.
4. `sign <name> <channel> <serial> <key> [yes|no]` refuses unless each index still matches its
   test stamp, then writes `manifest.toml` and signs it with `nodo-snapshot` (Ed25519). Every
   package of the `nodo` repository is pinned by version. `--untested` skips the stamp, for
   throwaway test keys only.
5. `publish <name> <channel>` copies the signed manifest to `channels/<channel>/`. Upload
   `snapshots/<name>` first and `channels/` last.

The manifest (`crates/nodo-update/src/manifest.rs`) states the channel, a strictly increasing
`serial`, the snapshot name, `published` and `expires` times, whether moving to it needs a restart
(kernel, libc, init), and for each architecture its repositories (`void`, and `nodo` for ours)
with their paths and the SHA-256 of their `repodata`. Signatures are detached in
`manifest.toml.sig`, cover `context || exact bytes`, and are checked with `verify_strict`.
Unknown fields are refused.

**Device trust** (`crates/nodo-update/src/trust.rs`):

- Trust is seeded from `Pins`: keys, threshold and `min_serial`, the serial of the snapshot the
  image was built from. Losing `/var/lib/nodo` therefore cannot allow replays older than the
  image. A newer image raises the floor and never lowers it.
- A manifest is accepted only if enough trusted keys signed it, it is for the device's channel and
  architecture, its serial is higher than the last applied one, and, when the clock is
  synchronised (from nodo-health), it has not expired. A second, different manifest with the same
  serial is refused as a conflict. Equal serial and identical bytes is "unchanged".
- **Key rotation:** a `KeyStatement` lists new keys and a threshold, with an increasing sequence,
  and must be signed by enough of the current keys. A device that was offline through several
  rotations applies them in order. Older statements are refused, so a retired key cannot be
  restored. A manifest signed only by unknown keys is reported as "signed by an unknown key" and
  needs the user, who can approve one exact change (`KeyApproval` names the old and new keys).
- **A key statement that cannot be read:** 404 or 410 means none is published. Any other failure is
  tried once more; if it still fails, the update goes ahead under the keys already trusted, and only
  if the manifest is then refused does the download failure become the error. The stable channel
  always publishes one (`feed/snapshot-keys/README.md`): a no-op restating the pinned keys, because
  GitHub Pages answers a missing file in a directory with 503 and the client in nodo-tools 12
  stopped on that.
- **Withdrawing a bad snapshot** is a new manifest with a higher serial pointing at an older
  snapshot, naming the pulled one in `withdraws` (`publish-snapshot.sh sign ... --withdraws
  <name>`). That is not a downgrade of trust: the serial must still be higher than the record and
  the image's floor, so neither the withdrawal nor anything older can be replayed. A device
  recorded on a withdrawn snapshot moves this project's packages (the `nodo` repository, every
  one of them pinned) back to the offered versions; every other device refuses a manifest that
  would move one of them back, before xbps runs, saying which package and that it does not
  withdraw this device's snapshot. That includes a device that lost its record, which cannot say
  which snapshot it is on; it waits for the next snapshot that moves forward. Void's own packages
  are never moved back: `-u` leaves a newer one as it is, because an older libc or library
  under newer programs is not a state anyone tested. A bad Void package is fixed by a newer
  snapshot.

**Device client** (`crates/nodo-update/src/client.rs`, run as root by `nodo-sysupdate check|apply`,
shipped in nodo-tools). The server comes from `/etc/nodo/system-update.toml` (`server = "..."`) or
`--server`, the pins from `/usr/share/nodo/snapshot-pins.toml`, and the record of the last applied
snapshot is `/var/lib/nodo/update/system-trust.toml`. `apply`:

1. applies the channel's key rotation statement, if there is a new one;
2. downloads and checks the manifest as above;
3. downloads each repository index the manifest names for this architecture and requires its
   pinned hash. The index names the key its packages are signed with, so the owner's xbps key is
   authenticated by the snapshot signature and need not ship in the image: the client writes it to
   `/var/db/xbps/keys` itself;
4. writes the verified indexes where xbps keeps its copy of each repository and runs
   `xbps-install --ignore-conf-repos -R <void> -R <nodo> -yu` without `-S`, so xbps uses exactly
   those indexes. xbps checks every package's hash and `.sig2` before unpacking it, Void's against
   Void's key. `/etc/xbps.d` still applies its noextract and ignorepkg settings. Before any of
   this, a snapshot that would move one of this project's packages back is refused unless it
   withdraws this device's snapshot (see above). For a withdrawal, it then runs `xbps-install --ignore-conf-repos -R <void> -R <nodo> -yf` with
   exactly the packages that go back, from the same verified indexes;
5. requires every pinned package that is installed to be at its pinned version;
6. restarts the enabled, running services of the packages that changed, unless the snapshot needs
   a restart, and only then records the snapshot. A refused or failed update changes no record, so
   it can be retried.

`--json` prints one line (`nodo_update::client::Report`) for the helper, `--expect-serial N`
applies only the serial the owner was shown, and `--keep-running NAME` leaves a service running
even if its package changed, listing it as deferred.

**Connected** (`crates/nodo-helper/src/sysupdate.rs`). The helper runs `nodo-sysupdate --json`
as a child, so the trust rules have one implementation, and keeps the automatic switch, the last
check and what it found, the last error and the last update in
`/var/lib/nodo/updates/system.toml` (backed up with the other update choices). Requests:
`SystemUpdateStatus`, `CheckSystemUpdate`, `ApplySystemUpdate { serial }` (only the serial the
last check found, which the screen showed), `SetSystemAutoUpdate` and `RunSystemUpdate` (root
only). An apply waits while the disk is low, holds the same lock as package installs, and passes
`--keep-running nodo-helper`, so the helper never restarts under its own request; a changed
helper runs its new version after its next restart, and the result says so. **Automatic system
updates are off by default.** The `nodo-systemupdate` runit service in nodo-tools asks for one
run every ten minutes; the helper checks every six hours (an hour after a failed check) and only
records what it found. Only when the owner turned automatic system updates on does a run apply
a snapshot, inside the package update window, while the disk has room, and never one that needs
a restart. The TUI shows the snapshot as the `system base` row of the Updates screen (with
"Restart needed" or "Replaces a withdrawn snapshot" and what that means), applies it after the
selected package updates, lists it switched off on the auto-update screen, and adds a line with
the installed snapshot and the last check. `nodo-tui --ctl system-status|system-check|
system-apply SERIAL|system-auto 0|1|system-run` reach the same requests.

The server may be `http://` as well as `https://`, for the same reason as feeds: the manifest is
signed, and everything else is pinned by it. Someone on the network can still withhold updates,
which `expires` bounds once the clock is synchronised.

**Keys, and keeping them offline.** There are two, both made and used on a machine that is never
online, such as a laptop with its network hardware removed, booted only for signing:

- The snapshot key (Ed25519): `nodo-snapshot keygen <file>` writes the secret key with mode 600
  and prints the public key, which goes into `snapshot-pins.toml` in the image. Keep two or
  three, on separate encrypted USB sticks in separate places, and either list them all with
  `threshold = 1` (any one can sign, losing one is harmless) or require two. Rotate with a key
  statement signed by the current keys (`nodo-snapshot sign keys keys.toml <key>`), published
  as `channels/<channel>/keys.toml`.
- The xbps key (RSA, `openssl genrsa -out xbps.pem 4096`), used by the `nodo` step. Its public half
  travels inside the signed index.

The online machine fetches, builds and tests; the snapshot directory is carried to the offline
machine for `nodo` (which needs the builder image there) and `sign`, and the signed files are
carried back and uploaded. The tests use throwaway keys made in `build/snapshot-test` by
`tests/snapshot-prepare.sh`; no real key exists in this repository.

**Checked in the KVM VM** by `tests/snapshot-update.sh`, run as a whole by the VM check
(`scripts/test.sh vm` runs `tests/snapshot-prepare.sh` first, and `tests/snapshot-checks.sh`
pushes this project's packages and runs the script whenever the VM holds test snapshots), with
Caddy as the server in a network namespace: a signed snapshot applies over plain HTTP and installs its
xbps key; unsigned, tampered, foreign-key and expired manifests, a tampered index and a tampered
package are refused with nothing changed; the next snapshot applies over HTTPS, updates a test
service package, and the service comes back on the new version while the helper keeps running;
the older snapshot and a conflicting one with the same serial are refused; and with the record
deleted, an older snapshot above the image's floor still cannot move a package back. Then, through
the helper: a newer snapshot that would move a package back without withdrawing anything is
refused; with automatic system updates off, the `nodo-systemupdate` schedule finds a withdrawal of
the current snapshot and applies nothing, nor does a scheduled run; the helper refuses a serial
the owner was not shown; applying the withdrawal moves the test package back and its service
comes back on the old version; and the withdrawn snapshot cannot come back. Last, with automatic
system updates turned on: a run leaves a snapshot waiting outside the package update window, the
`nodo-systemupdate` schedule applies it by itself inside the window and records it as automatic,
and a snapshot that needs a restart waits for the owner even inside the window.

The TUI journeys (`tests/tui-e2e.sh`, `grow` and `system`) drive the Updates screen against the
real helper: an update that grows `tui-shop`'s encrypted volume from 256 to 320 MiB asks for the
device passphrase first, Esc cancels, a wrong passphrase changes and holds nothing, and the right
one grows the volume in place and updates; and with the test snapshots served in the VM, the
`system base` row offers test-b, says automatic system updates only notify, and applies it.

Not yet: `fetch` has not been run against a live mirror; no real Void package was updated in a
VM test (the test snapshots reuse the VM's own Void index); and a snapshot that needs a restart
only says so, and nothing restarts the device for it.

### The T6N's published snapshots

Every NanoPC-T6N (mainline) image trusts `https://nodo.monokulo.com/snapshots`
(`/etc/nodo/system-update.toml`, a configuration file the owner may edit) and two Ed25519 keys on
the `stable` channel (`/usr/share/nodo/snapshot-pins.toml`, `threshold = 1`, `min_serial = 0`
because images are built from `main`, not from a snapshot). `nodo-systemupdate` starts at boot, so
the helper checks every six hours and the Updates screen shows a new snapshot as the `system base`
row. **Automatic system updates stay off**: nothing is installed until the owner applies it, or
turns automatic system updates on, which still never applies a snapshot that needs a restart.

**Hosting.** The snapshots share the GitHub Pages site with the app feed. Pages refuses files over
100 MB and sites over 1 GB, and the image's Void packages do not fit (`linux-firmware-network`
alone is 440 MiB). So a published snapshot copies only Void's signed index, a few megabytes
(`publish-snapshot.sh index`), and its manifest names Void's mirror
(`https://repo-fastly.voidlinux.org/current/aarch64`) as the `mirror` the `void` repository's
packages are downloaded from. The device still fetches the index from the snapshot and requires
its pinned hash, places it where xbps keeps the mirror's copy, and runs xbps without syncing, so
xbps installs exactly the pinned versions and checks each package against that index and Void's
`.sig2` signature. The mirror decides only whether a package can be downloaded, never which one;
the security properties above are unchanged. This project's packages (the `nodo` repository,
about 60 MB: the kernel is 31 MB) are on Pages, and the three newest snapshots are kept, so a bad
one can be withdrawn back to either of the two before it. Void removes superseded packages from its
mirror after a while, so a snapshot made this way stays installable only while its Void versions
are still there: CI makes a new snapshot every week even when nothing of ours changed.

**Publishing.** `.github/workflows/system.yml` builds `feed/base-packages` for aarch64 on every push
to `main` that changes one of them, every Monday, and by hand. Its jobs keep keys away from
third-party code: `build` compiles (no secrets); `sign-repo` signs the `nodo` repository with the
RSA key `NODO_SNAPSHOT_XBPS_KEY`, whose public half is pinned in `feed/snapshot-xbps-key.pub.pem`
with the key file xbps keeps for it in `feed/xbps-keys/` (`nodo-snapshot xbps-key`; a test ties the
two together), which every image build copies into `/var/db/xbps/keys` so the key is trusted from
the start, never accepted on first sight (a branch run seeds its throwaway key the same way,
through `NODO_REPO_KEYS`); `test` copies Void's index, builds a T6N image from only the
snapshot (`publish-snapshot.sh test`, which also requires that Void's mirror still served the same
index before and after the build) and runs `tests/inspect-t6n-platform.sh` and
`tests/inspect-t6n-mainline.sh` on it, which is the test run the stamp records (no secrets); and
`publish` signs the manifest with `NODO_SNAPSHOT_SIGNING_KEY` (refused unless every index is the
one the test stamped), checks it against the shipped pins as a device would, publishes it on the
channel, keeps the three newest snapshots and deploys. `publish-snapshot.sh restart` marks the
manifest as needing a restart when the kernel, glibc or runit changed since the previous snapshot.
The serial is the UTC time it was made (`YYYYMMDDHHMMSS`) and the name the same time
(`YYYY.MM.DD-HHMMSS`). A branch run builds and tests with a throwaway key and publishes nothing.
The secrets live in the `github-pages` environment, which only `main` may use.

Both workflows deploy the whole Pages site, so each writes its own half and copies the other half
from the live site (`scripts/pages-site.sh`): the feed's `index.html`, `CNAME`,
`signing-key.pub.pem` and `feed/`, listed in `files/feed.txt`, and the snapshots' `snapshots/`,
listed in `files/system.txt`, each file with its SHA-256. Their publish jobs share one concurrency
group, so they never overlap.

**Keys.** The snapshot keys were made with `nodo-snapshot keygen`. The first signs in CI; the second
exists only offline (`~/.local/share/monokulo-nodo/snapshot-key-2` on the machine that made them,
to be copied to encrypted storage and deleted there) and can sign a key rotation statement that
replaces a lost or exposed first key, which devices then apply by themselves. Signing in CI
differs from the offline signing described above: anyone who can run a workflow on `main` can
publish a snapshot, the same trade-off as the feed's key (docs/FEED.md), bounded by automatic
system updates being off.

**Kernel updates on the T6N.** The bootloader reads the kernel and device tree from their own
partitions (`kernel` and `resource`), not from the root filesystem, and the SD card has no second
slot. So `linux-mainline-rk3588`'s INSTALL script, on a running board, copies the package's
`kernel.img` and `resource.img` to those partitions after saving their old contents in
`/var/lib/nodo/boot/previous/`; the image builder has no such partitions and writes them itself.
A snapshot that changes the kernel says it needs a restart, so it is never installed
automatically, and the new kernel runs after the owner restarts. Rollback here covers this
project's packages (a withdrawal) but not a kernel that does not boot: that card is recovered on
another computer by writing `kernel.part` and `resource.part` back to their partitions, or by
flashing a new image. The modules of the old kernel are replaced by the update, so a kernel cannot
be swapped back on the running board either.

## 2. Auto-update

`crates/nodo-update/src/auto.rs`. Off by default and chosen per package, with a daily, weekly or
manual check and a window (default 03:00 to 05:00, may cross midnight).

An update always waits for the user if it asks for new permissions, comes with a changed feed key,
failed install checks, is a held version, or needs a system restart. All the reasons are listed
together. Blockers are reported before waits. An automatic update otherwise waits outside the
window, while the device is locked, or while disk is low (`refuse_installs` from nodo-health).

Installs run one at a time with the node last, and conditions are checked again before each one,
so locking the device or filling the disk mid-run postpones the rest. Each install goes through
the `Installer` trait (install, health check, rollback), which nodo-install implements. A failed
install or health check is rolled back, that version is held, and the outcome (`RolledBack`, or
`RollbackFailed` when the old version did not come back) stays in `attention` for Home until
dismissed. Nothing retries a held version until the user releases it.

**Connected** (`crates/nodo-helper/src/updates.rs`). The helper keeps the policy, holds, history
and the last check's candidates in `/var/lib/nodo/updates`. A check refreshes the feeds and
reviews every newer version with nodo-install, so the permission delta, a changed key and failed
checks are known before anyone selects it. Each update keeps the previous version's verified
package file in `/var/lib/nodo/packages/<id>` and a copy of the package's data (inside the volume
for encrypted packages), and a rollback reinstalls that file with `xbps-install -f` and puts the
data back. Updates the user selects follow the same rules, and one that asks for more needs the
reviewed permission lines quoted back. The `nodo-autoupdate` runit service is the timer: every ten
minutes it asks the helper for one run (`RunAutoUpdate`, root only), and the helper decides whether
a check is due and whether the window is open. With automatic updates off, the run still reads
any feed not read for six hours, so the device's list of available packages stays current
(docs/FEED.md, When devices read feeds); it installs nothing. Proven in the KVM VM by `tests/helper-e2e.sh` with
a feed signed in the VM: an update asking for internet access is held, one failing its health
check is rolled back with its data and held, a held version is not retried, and a healthy one
installs.

**Rolling back by hand.** A successful update keeps the previous version's verified file one
generation deep, with the data layout its data was in (`previous` in the feed state). `ListRollbacks`
offers it, and `RollbackPackage` (the TUI's System › Updates › Undo an update) puts it back through the same engine:
the data is copied, the kept file reinstalled, the old version health checked, and on failure the
newer version and its data come back. The version left is then held. It refuses data already in a
newer layout than the old version's, a kept file asking for permissions no longer approved, a locked
device, low disk, and data on the data drive. After every update, rollback, install and USB
restore the helper removes kept files nothing names: all of a package that is no longer installed
(the helper has no uninstall request yet, so today that means a first install that failed, or files
a restored device's records do not name), and for an installed package anything beyond its current
version's file and the one previous version, so a reinstall or an interrupted earlier run never
leaves a second older copy behind (`prune_kept`, unit tested in `manage_tests.rs`).

**Encrypted packages across updates.** An update never touches the LUKS volume itself: the new
version starts on the same volume with the same key. Two manifest fields describe what a version
needs from it. `storage.size_mib` is the volume size (256 MiB when not given, 32 MiB to 1 TiB),
and `storage.data_layout` is the version of the package's own data layout (1 when not given). The
helper records the layout the data is in at the top of the volume (`.nodo-layout`, root-owned, out
of the service's reach) when the volume is created and after an update passes its health check.

- A version asking for a larger volume grows it in place before it starts, while it stays
  mounted: the file is extended with real blocks, the loop device re-read, the LUKS2 mapping
  resized and the ext4 filesystem grown online. LUKS2 keeps the volume key in the kernel keyring,
  so resizing needs the key again, and the helper never keeps the device key after an unlock.
  Such an update therefore needs the device passphrase (`ApplyUpdatesWithPassphrase`), and
  automatic updates leave it waiting for the user ("needs the device passphrase to grow its
  encrypted storage"). `UpdateStorageGrowth` lists the updates that grow, so the Updates
  screen marks them ("Needs the device passphrase") and, when a selected batch holds one, asks
  for the passphrase before applying it, also after a permission review and on a retry. Volumes never shrink; a rollback keeps the larger volume, which holds the
  old data just as well.
- A version declaring a newer layout converts the data itself on its first start. A version
  declaring an older layout than the data's is refused before anything is stopped, since it could
  not read the data.
- Rollback is copy-based. Before the old version stops, the whole volume (data, logs, rendered
  settings and the layout record) is copied into `.nodo-rollback` inside the same volume, so the
  copy is encrypted like the data. A failed install or health check stops the new version, puts
  the copy back and starts the old version on exactly the data it left; a successful one removes
  the copy. The copy needs as much free space in the volume as the data takes, plus 8 MiB, and an
  update that does not fit is refused before anything changes. ext4 on a loop file has no
  snapshots, and an LVM or btrfs layer would add a moving part to every volume for this one case,
  which is why the copy was chosen. Before this change the margin was 256 MiB, which no default
  volume could ever satisfy, so every encrypted update refused to start.

Proven in the KVM VM by `tests/helper-e2e.sh` with `e2e-store`, an encrypted test feed package:
1.0 writes data in layout 1; 2.0 asks for 320 MiB and layout 2, waits for the passphrase, then
grows the same LUKS volume (same UUID) and its mounted filesystem and converts the data; 3.0
destroys the data on its way to layout 3 and fails its health check, and the rollback brings back
2.0 and its data byte for byte in layout 2; 4.0 declares layout 1 and is refused unchanged.

## 3. A/B system rollback on the NanoPC-T6N

### What the FriendlyElec boot chain does

From `sd-fuse_rk3588` at `430f0c0` (`prebuilt/parameter*.txt`, `prebuilt/boot.img`, `dtbo*.img`):

| Partition | Start (sectors) | Size | Content |
|---|---|---|---|
| idbloader | 0x40 | | DDR init and SPL, raw |
| uboot | 0x4000 | 4 MiB | U-Boot FIT |
| misc | 0x6000 | 4 MiB | Android-style bootloader message, read by U-Boot |
| dtbo | 0x8000 | 4 MiB | overlay that sets `bootargs_ext`, including `root=` |
| resource | 0xa000 | 16 MiB | device trees and logos (from our kernel package) |
| kernel | 0x12000 | 40 MiB | `KRNL`-wrapped kernel (from our kernel package) |
| boot | 0x26000 | 32 MiB | `KRNL`-wrapped gzip cpio: an Ubuntu initramfs-tools ramdisk |
| recovery | 0x36000 | 32 MiB | unused by us |
| rootfs | 0x46000 | rest | ext4 root |

- U-Boot loads `kernel`, `resource` and `boot` by partition name. `root=` comes from the dtbo,
  which holds two fragments, `/dev/mmcblk0p8` and `/dev/mmcblk2p8`, presumably one per boot medium.
  In the "plain" dtbo the root is mounted directly; the default one adds `data=/dev/...p9` and an
  overlayfs.
- The ramdisk is ours to replace. Its `init` sources `/conf/param.conf` after every
  `init-premount` script (`scripts/init-premount/ORDER`), so a hook can override `ROOT`, and its
  `resolve_device` accepts `PARTLABEL=`.
- **On first boot the ramdisk grows the root partition to 100% of the disk** unless
  `/etc/fs.resized` exists (`scripts/local`, `local_resize_part`). The current SD image does not
  ship that file. Any A/B layout must ship it in both slots, or the first boot destroys slot B.
- The vendor kernel config has `CONFIG_DW_WATCHDOG=y`, `CONFIG_WATCHDOG_HANDLE_BOOT_ENABLED=y`,
  `CONFIG_WATCHDOG_OPEN_TIMEOUT=0` and `CONFIG_PANIC_TIMEOUT=5`, and no `CONFIG_KEXEC`.

### Design without changing U-Boot

Layout on the eMMC (partitions 1 to 7 unchanged):

| # | Label | Size | Use |
|---|---|---|---|
| 8 | rootfs_a | 3 GiB | root slot A, ships `/etc/fs.resized` |
| 9 | rootfs_b | 3 GiB | root slot B, ships `/etc/fs.resized` |
| 10 | nodoboot | 1 MiB | raw A/B boot record, two copies |
| 11 | nodo-state | rest | ext4: `/var/lib/nodo`, `/var/log`, keys, settings, volumes |

The dtbo keeps pointing at partition 8, so if our hook is missing the device still boots slot A.

Boot flow (`crates/nodo-update/ramdisk/nodo-ab`, an `init-premount` hook):

1. Read the newest valid copy of the boot record from `nodoboot` (sectors 0 and 8). A copy is
   valid only if it parses exactly, so a torn write leaves the other copy in charge.
2. If the active slot is known good, boot it. If it is on trial and has tries left, decrement and
   boot it. If its tries are exhausted and the other slot is good, switch back, write the record,
   and leave `/run/nodo/boot-fallback` so Home can say what happened.
3. Write `ROOT=PARTLABEL=rootfs_<slot>` to `/conf/param.conf` and `/run/nodo/boot-slot`.

The running system marks the slot good (`slots::mark_good`) once runit stage 2 is up, the helper
answers and the node's run script has started; it does not wait for sync. An update writes the
new root into the inactive slot, verifies it, and calls `slots::stage`, which gives it three trial
boots. `stage` refuses unless the running slot is active and known good, so there is always a
fallback.

The Rust state machine and the shell hook implement the same rules. `tests/ramdisk.rs` runs the
hook against the Rust code on a regular file for every case, including torn and malformed copies
and a full trial-then-fallback sequence, with the host shell and with busybox 1.34's `sh` and
applets (`NODO_AB_BUSYBOX=/path/to/busybox.static`). A deliberately broken hook fails the tests.

### What this cannot cover, and what is unproven

- **The kernel, device tree and ramdisk are shared.** U-Boot loads fixed partitions, so a bad
  kernel cannot be rolled back by the ramdisk. Kernel updates are therefore "restart needed", are
  never automatic, keep both slots' `/usr/lib/modules` able to run the kernel in `kernel`, and
  need the recovery path below if they fail. Rolling back kernels too needs either U-Boot's own
  A/B support (Rockchip U-Boot has `CONFIG_ANDROID_AB`, but whether FriendlyElec's build enables
  it is unknown) or `kexec` from the ramdisk (not enabled in the vendor kernel, untested on RK3588).
- **Hangs.** A kernel panic reboots after 5 s, and a crash in userspace before the slot is marked
  good is counted on the next boot. A hang is only caught if something keeps the watchdog armed:
  with `WATCHDOG_OPEN_TIMEOUT=0` the kernel pets it forever until userspace opens it, so runit
  stage 1 must open `/dev/watchdog` early and stop petting it if boot does not reach the
  good-marker within a few minutes. Whether U-Boot enables the watchdog at all is unknown.
- Not proven without hardware: that U-Boot ignores the root partition's name and size, that
  `PARTLABEL=` resolves in the FriendlyElec ramdisk on this board, that this busybox build behaves
  like 1.34 for the hook, and the eMMC device numbering. None of it has been run on a T6N.
- The hook has not been added to the image build. That needs a change to `boards/nanopc-t6n`
  (repack `boot.img` with the hook in `scripts/init-premount/` and an updated `ORDER`).

## 4. Migrating from microSD to eMMC

The current image boots from microSD and leaves the eMMC alone. The migration runs from the SD
system, as an explicit user action in the TUI, never automatically:

1. Check the eMMC is present, show its current contents and size, and require typing the device
   label, the same rule as formatting a data drive.
2. Write partitions 1 to 7 from the same pinned sd-fuse files the SD image uses, then the table
   above. Copy the running root to `rootfs_a` (an image file, not a live copy), write an initial
   boot record (`BootRecord::initial`), and create `nodo-state`.
3. Copy `/var/lib/nodo` and the logs to `nodo-state` with services stopped, then verify by hash.
4. Ask the user to remove the SD card and reboot. The SD card stays a working fallback until the
   user erases it.

The chain data stays on the NVMe drive and is not touched.

## 5. Backups and USB recovery

`crates/nodo-backup`. A backup holds settings (`/var/lib/nodo/settings`, `exposure.toml`,
`node-onion.toml`, `bindings.toml`, `/etc/apps/<id>`), secrets, Caddy's storage, the unwrapped device key, the
contents of unlocked encrypted volumes and Tor onion keys, never chain data. It also holds
`feeds.toml` (feeds, pinned keys and version floors, approvals, what each package was installed
from), the auto-update policy and holds in `/var/lib/nodo/updates`, and the verified package files
kept for rollback in `/var/lib/nodo/packages`. Format: magic, a TOML header with Argon2id parameters, salt
and nonce, then the payload sealed with the STREAM construction over XChaCha20-Poly1305
(RustCrypto `aead::stream`), with the header as associated data on every chunk. The backup
passphrase has at least 12 characters and the same KDF cost as the device key.

Because the device key is inside, a backup restores encrypted storage even after the device
passphrase is lost; the user sets a new device passphrase on restore. Restores verify the entire
file first, then write into a new staging directory, so a wrong passphrase, a flipped byte,
truncation, appended data, reordered chunks or a header with a lowered KDF cost leave nothing
behind. Paths inside are validated even though they are authenticated, setuid bits are dropped,
and symlinks are never followed when backing up. All of this is unit tested.

**Connected** (`crates/nodo-helper/src/backup.rs`). Backups are written into the root-only
`/var/lib/nodo/backups` under generated names, so no request names a path. Including the device
key needs the device passphrase, checked like an unlock. A restore needs the typed word
`restore`, verifies the whole file into a staging directory, stops the services whose settings or
storage it replaces, moves everything into place, renders settings again with this device's
owners, starts what was running and deletes the staging copy. The restored site exposure and
public host names are applied to Caddy, the firewall, Tor and dynamic DNS as part of the restore
(on a new device once its packages are back), not at the proxy's next reload; if the proxy
refuses, the restore stands and its report says why. A device whose volumes already use
their own key keeps it, since replacing it would lock them for good. Proven in the KVM VM: a
wrong passphrase, a flipped byte and a missing typed word restore nothing, and a real restore
brings back settings, secrets and a file inside an encrypted volume. On a device that is already
set up, feeds, approvals and packages stay as they are, since they match what is installed there.

**USB drives** (`crates/nodo-helper/src/usb.rs`). The helper reads `/sys/block` and only ever uses
a disk the kernel marks removable or that is attached over USB. It refuses a disk when any of its
filesystems is mounted anywhere else, a device mapper or swap holds it, a partition or filesystem
carries the `NODO_BLOCKCHAIN` label, or it is the configured data drive, so neither the system
disk nor chain data can be named by a request. Device names are kernel names only (`sdb1`),
checked again on every request. SD and eMMC slots, loop, device mapper and optical devices are
never candidates. Only vfat, exfat and ext4 are mounted, always `nosuid,nodev,noexec,noatime`,
FAT as root with no access for others, on the private `/run/nodo/usb`, read-only for listing,
inspecting and restoring. Backups live in a `monokulo-nodo` folder that must be a real directory;
files are opened without following links. A backup is written as `.nodo-<time>.nodobak.partial`,
synced, renamed and the folder synced, then the drive is unmounted. Before writing, the helper
estimates the size and refuses a full drive, and a backup over 4 GB on FAT32. A write error is
reported by cause: full, read-only (write-protect switch) or unplugged (the device is gone or
answered with an I/O error), and the partial file is removed, or cleaned up on the next write
when the drive vanished. Listing never shows partial files. The TUI's Backups screen
(`docs/design/tui/Backup.dc.html`) shows the device's backups and the first USB drive's, `u`
writes one to the drive and Tab moves to its list for a restore.

**Restore onto a new device.** First boot offers Ctrl-R, "restore from a USB backup", next to
setting the passphrase. The owner picks a backup on the drive, types the backup passphrase, sees
what it holds, types `restore` and chooses the device passphrase. The helper then wraps the
backup's device key with that passphrase and keeps it in memory (unlocked), puts back settings,
bindings, exposure, secrets, Tor keys, Caddy's storage, feeds, pins, approvals and update choices,
and for every package in the backup's feed records reinstalls it, creates its volume at the
declared size with the restored key, fills `data` and `log` and the layout record from the backup
while the service is still held down (a runit `down` file), renders its settings and secrets, and
starts it. Packages are reinstalled from the verified copies inside the backup, not downloaded:
that works offline and with feeds that moved or vanished, and installs exactly the version the
owner approved. Each copy is inspected again under the install rules and must ask for nothing
beyond the stored approval; only a package whose copy is missing is fetched from its feed, under
the restored pin, at the recorded version. The feed's version floor stays, so future updates
still need the pinned key. A device that is not set up and a backup without a device key gets a
new key instead, and the report says so.

Proven in the KVM VM by `tests/usb-e2e.sh`, which runs on the host because it plugs disk image
files in and out as USB drives through the QEMU monitor (`scripts/dev-vm.sh usb-create`,
`usb-plug`, `usb-unplug`) and replaces the VM with a clean one (`scripts/dev-vm.sh fresh`) while
the stick, a file in the VM's container, stays. It checks the refusals (system disk, a path, a
`NODO_BLOCKCHAIN` USB disk, a missing device passphrase, a read-only stick, a full stick, a stick
pulled out mid-write), then destroys the first VM, boots the clean image with the same stick,
restores it and checks that the device is unlocked, the feed key, approval and package version
are back without the feed, the volume has its declared size, data and layout, the service runs,
the sealed secret opens, Caddy's files are identical and owned by `_caddy`, the package's public
site and host are in Caddy's configuration straight after the restore, that the node's onion
address is back on with the same address (Tor from the pinned mirror runs on the restored keys),
and that after a helper restart the new passphrase unlocks the restored storage while the old one
does not.

USB recovery plan, in order of what exists today:

1. **Restore onto a fresh install.** Done, as above, from the first-boot screen.
2. **Boot failure after an update.** The A/B fallback above. If both slots fail, boot the SD card
   image, which can repair the eMMC slots from a downloaded or USB-provided snapshot image.
3. **Bricked bootloader.** Rockchip MaskROM mode with `rkdeveloptool` from another computer, using
   the same pinned sd-fuse files. Documented for the user, not automated.

## 6. Disk-full protection, logs, memory and time

`crates/nodo-health`, daemon `nodo-healthd` (root, under runit). Per filesystem: a warn threshold
(Home warns, installs and updates wait), a stop threshold (the services writing there are stopped)
and a resume margin above the stop threshold (no flapping). Each threshold is the larger of a byte
count and a percentage. Defaults: system disk warns below max(1 GiB, 15%) and stops its app
services below max(384 MiB, 5%); the chain drive warns below max(20 GiB, 5%) and stops the node
below max(4 GiB, 1%), resuming 2 GiB or 0.5% above that. Free space is measured as an unprivileged
writer sees it, so root keeps the ext4 reserve on the system disk.

A service is stopped through runit, which runs its `control/t`, so the node shuts down cleanly
while there is still room. A `down` file keeps runit from starting it again at boot while the
disk is still full, and each tick stops it again if anything starts it. Only services the daemon
stopped are restarted; a service the user stopped stays stopped. An unmounted data drive is
reported rather than measured, and its services stay stopped. State survives daemon restarts.

Logs: every svlogd directory gets `config` with a size and count limit, restored if the service
user rewrites it; when the system disk is low, rotated files are pruned oldest first, never
`current`. Memory: each configured service's processes are moved into
`/sys/fs/cgroup/nodo/<service>` with `memory.max` and `memory.high`, and OOM kills are reported.
Time: `chronyc -n tracking` decides `synced`, which nodo-update uses for manifest expiry. The
report is `/run/nodo/health.json`.

VM proof, `tests/health/disk-full.sh`, all 16 checks passing in the x86_64 KVM dev VM with the
real cuprated: its data goes on a 300 MB loop filesystem. The control starts cuprated on a full
disk without protection and sees it crash-loop (50 starts and 50 `No space left on device` panics
in 45 s), which shows the checks can fail. With the daemon running: a low disk only warns; below
the stop threshold cuprated is stopped with a clean "Shutdown complete", a `down` file is written
and Home's report says why; filled to 100% for 40 s there are no restarts and no new ENOSPC
errors; once space is freed it starts by itself, RPC listens again, and its database opens with no
panic or corruption message. The same run shows cuprated placed in a cgroup with `memory.max`,
svlogd limits written, and the time status matching chrony. An idle, offline node writes too
little to fill a disk by itself, so the control uses a first start, where the database must be
created; a syncing node on a real drive was not tested.

**Connected.** The helper writes `/etc/nodo/health.toml` from installed manifests at start and
after installs and updates: apps on the system disk, the node on the data drive when the board has
one. The `nodo-healthd` runit service in nodo-tools runs the daemon, and the helper restarts it
when the configuration changes. Home reads the report, the boot fallback notice and failed updates
through the helper (`Request::Health`, `UpdateStatus.attention`). Proven in the KVM VM with a 6 GB
sparse loop filesystem as the data drive: the report marks it low and Home's note says so, without
stopping anything. Manifests cannot ask for memory limits yet, so no limits are written.

## 7. Encrypted packages leak to the unencrypted disk

Implemented as proposed below and proven in the KVM dev VM (`tests/helper-e2e.sh`), with these
differences: the volume's top is root-owned 0711 and `config` root-owned 0755 (files inside are
0640 with the service's group), so the service user cannot swap `config` for a link that
redirects the helper's writes; the resolved settings environment file goes to memory under
`/run/nodo/secrets/<id>/env` rather than into the volume; data the old layout kept at the top of
a volume moves into `data/` at the next unlock. Not done: moving an existing package's
`/etc/apps/<id>` and `/var/log/apps/<id>` into the volume when an update makes it encrypted,
and `fstrim` afterwards.

Today an encrypted package's data is in its LUKS volume, but `/etc/apps/<id>` (rendered settings,
which can include granted secrets) and `/var/log/apps/<id>` stay on the system disk, and
`ServiceSpec::new` refuses a data mount for encrypted packages, so large encrypted volumes must
live on the small eMMC. Proposed change, for the owners of nodo-gen and nodo-helper:

**nodo-gen**

- `Layout::new(id, profile, encrypted: bool)`: for encrypted apps, `config_dir` becomes
  `/run/nodo/volumes/<id>/config` and `log_dir` `/run/nodo/volumes/<id>/log`, with
  `data_dir` staying the sandbox path `/var/lib/apps/<id>` bound from
  `/run/nodo/volumes/<id>/data`. Inside the sandbox the config is still bound read-only at
  `/etc/apps/<id>`, so packages see no difference.
- `run_script`: in `encrypted_preamble`, set `data=$mount/data` and bind
  `--ro-bind $mount/config /etc/apps/<id>`.
- `log_run`: for encrypted packages, wait like the service does
  (`mountpoint -q /run/nodo/volumes/<id> || { sleep 5; exit 1; }`) before
  `exec chpst ... svlogd -tt /run/nodo/volumes/<id>/log`. runsv buffers nothing while the logger
  is down, and the service is waiting for the same unlock, so nothing is lost or written in clear.
- `ServiceSpec::new`: drop the "encrypted storage cannot use a data mount" error. The data mount
  then decides where the volume file lives, not where plaintext goes.
- `Artifacts::directories`: for encrypted packages, list `config`, `log` and `data` inside the
  volume (owner: service user, `config` root-owned 0750 with group read), created by the helper
  after unlock rather than at install.

**nodo-helper**

- `volumes.rs`: `volume_file(id)` returns `$DATA_MOUNT/.nodo-volumes/<id>.luks` when the package
  has a data mount grant, else `/var/lib/nodo/volumes/<id>.luks`. `open_volume` refuses while the
  data mount is not mounted (never create a volume underneath it), and `create_volume` allocates
  there. After mounting, create the three subdirectories with the modes above.
- `settings.rs`: for encrypted packages, write rendered settings into the volume's `config` and
  return a "locked" error while the volume is closed, instead of writing `/etc/apps/<id>`.
- Install and migration: when an existing package becomes encrypted, move `/etc/apps/<id>` and
  `/var/log/apps/<id>` into the opened volume, then delete the originals and run `fstrim`. Be
  honest in the TUI: plaintext already written to flash cannot be reliably erased.

## 8. What is still missing

The API, helper, TUI and installer parts listed here before are connected (sections 2, 5 and 6).
Still open:

- **nodo-manifest**: a way to ask for a memory limit, so `health.toml` can set one.
- **nodo-helper**: copying the data of a package kept on the data drive before an update (such an
  update is refused for now). A backup restored onto a device whose volumes keep their own key
  keeps that key, so sealed secrets from the other device cannot be opened there.
- **System snapshots**: the remaining gaps are listed at the end of section 1.
- **boards/nanopc-t6n**: ship `/etc/fs.resized`, add the ramdisk hook, the A/B layout for eMMC,
  and open the watchdog early. The mainline card (`boards/nanopc-t6n-mainline`) now enables
  `nodo-systemupdate` and ships the update server and keys (section 1, "The T6N's published
  snapshots"); the vendor 6.1 card does not start the timer.
- **System snapshots on the T6N**: a kernel that does not boot is recovered by hand on another
  computer, since the SD card has no second slot; a previous kernel kept on the card for the
  bootloader to fall back to would close that.
