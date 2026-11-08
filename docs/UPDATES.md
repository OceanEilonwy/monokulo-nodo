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
- **Withdrawing a bad snapshot** is a new manifest with a higher serial pointing at an older
  snapshot. That is not a downgrade of trust. Package versions that went backwards then need
  `xbps-install -f`, which the client does not do yet, so such a manifest is refused by its
  version pins today.

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
   Void's key. `/etc/xbps.d` still applies its noextract and ignorepkg settings;
5. requires every pinned package that is installed to be at its pinned version;
6. restarts the enabled, running services of the packages that changed, unless the snapshot needs
   a restart, and only then records the snapshot. A refused or failed update changes no record, so
   it can be retried.

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

**Proven in the KVM VM** by `tests/snapshot-prepare.sh` and `tests/snapshot-update.sh`, with Caddy
as the server in a network namespace: a signed snapshot applies over plain HTTP and installs its
xbps key; unsigned, tampered, foreign-key and expired manifests, a tampered index and a tampered
package are refused with nothing changed; the next snapshot applies over HTTPS, updates a test
service package, and the service comes back on the new version while the helper keeps running;
the older snapshot and a conflicting one with the same serial are refused; and with the record
deleted, an older snapshot above the image's floor still cannot move a package back.

Not yet: the helper and TUI do not offer system updates and nothing runs `nodo-sysupdate` on a
schedule; `fetch` and `test` have not been run against a live mirror; no real Void package was
updated in a test (the test snapshots reuse the VM's own Void index); and a snapshot that needs a
restart only says so.

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
a check is due and whether the window is open. Proven in the KVM VM by `tests/helper-e2e.sh` with
a feed signed in the VM: an update asking for internet access is held, one failing its health
check is rolled back with its data and held, a held version is not retried, and a healthy one
installs.

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
`/etc/apps/<id>`), secrets, the unwrapped device key, the contents of unlocked encrypted volumes
and Tor onion keys, never chain data. Format: magic, a TOML header with Argon2id parameters, salt
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
owners, starts what was running and deletes the staging copy. A device whose volumes already use
their own key keeps it, since replacing it would lock them for good. Proven in the KVM VM: a
wrong passphrase, a flipped byte and a missing typed word restore nothing, and a real restore
brings back settings, secrets and a file inside an encrypted volume. Not yet: copying a backup to
or from a USB stick, and restoring onto a fresh install where the packages are not installed yet.

USB recovery plan, in order of what exists today:

1. **Restore onto a fresh install.** Flash the SD image, boot, choose "restore from backup", pick
   the file on a USB stick, enter the backup passphrase, set a new device passphrase. The helper
   moves each staged category into place with services stopped and rewraps the device key.
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
- **nodo-helper**: copying a backup to and from a USB stick, restoring onto a fresh install before
  its packages are installed, and copying the data of a package kept on the data drive before an
  update (such an update is refused for now). A backup restored onto a device whose volumes keep
  their own key keeps that key, so sealed secrets from the other device cannot be opened there.
- **System snapshots**: the server side and the `nodo-sysupdate` client exist (section 1), but the
  helper, the TUI and a timer do not use them yet, and withdrawing a snapshot needs `xbps-install -f`.
- **boards/nanopc-t6n**: ship `/etc/fs.resized`, add the ramdisk hook, the A/B layout for eMMC,
  enable the `nodo-healthd` and `nodo-autoupdate` services from nodo-tools, and open the watchdog
  early.
