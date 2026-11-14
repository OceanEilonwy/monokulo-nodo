# monokulo-nodo architecture

A from-scratch Monero appliance for the FriendlyElec NanoPC-T6N: Void Linux (glibc), runit, one Rust
codebase, and signed xbps packages from user-added feeds.

## Decisions already made

- **Node:** `cuprated` is the default and `monerod` an alternative. Packages depend on the role
  `monero-daemon`, never a specific implementation. Cuprate has no ZMQ, I2P or RPC login.
- **Isolation:** Every service gets its own user, a bubblewrap sandbox and per-user nftables rules, all
  generated from its manifest. AppArmor is not used. Packages never write system configuration.
- **Sandbox hardening:** Sandboxes keep the host network, so the per-user firewall rules apply, and
  with it the abstract Unix socket namespace. `nodo-confine` (nodo-tools) runs last in every sandbox
  and then becomes the service: it puts the service in its own Landlock domain scoped against
  abstract sockets and signals (Linux 6.12, Landlock ABI 6), and loads a seccomp filter refusing
  io_uring, foreign syscall ABIs and, for services granted no Unix socket, `socket(AF_UNIX)`. A
  seccomp filter alone cannot do it, because it sees the address of `connect` and `bind` only as a
  pointer. Per-service network namespaces would, but would replace the per-user firewall with
  per-interface routing. On older kernels, such as the T6N's 6.1, services granted a Unix socket
  (Caddy, apps with a socket upstream or dependency) can still reach abstract sockets, and their
  log says so; services without one cannot create Unix sockets at all. What is left open there
  is whatever listens on an abstract socket, so nodo-healthd lists those (listening stream and
  seqpacket sockets, and bound datagram sockets) with the process and user holding each, from
  `/proc/net/unix` and `/proc/<pid>/fd`. Home warns about any not on the allow-list
  (`abstract_sockets.expected` in `/etc/nodo/health.toml`, optionally tied to a user), naming the
  process and user and saying whether this kernel's sandbox can block it (Landlock ABI 6 or
  later, detected at runtime). The allow-list is empty, because the base system needs no
  abstract socket, and manifests cannot add to it. `nodo-healthd --abstract-sockets` prints the
  list and exits 1 when something unexpected listens, and nodo-base's `nodo-report`
  includes it (or the kernel's bare list when nodo-tools is missing). Sandboxes also get
  `--unshare-user --disable-userns`, so a service cannot create nested user namespaces. The
  cuprated run script has both, like every generated one: cuprated-service depends on
  nodo-tools for nodo-confine, and cuprated gets no Unix socket.
- **GPU:** A package asks for the GPU with `[devices] gpu = true`, a permission the owner approves
  like any other (a new one on an update waits for the owner). Granted, its service runs with the
  `gpu` group (render nodes are root:gpu 0660, and no account is a member) and its sandbox gets
  `/dev/dri/renderD*`, never card nodes, plus the few sysfs entries libdrm reads. Without it a
  sandbox has no `/dev/dri` and no `/sys`. Only the mainline T6N card has Vulkan (Void's loader
  and `mesa-panvk`, a headless panvk-only Mesa); [MAINLINE_KERNEL.md](MAINLINE_KERNEL.md) has the
  details, and `nodo-gpu-check` (nodo-tools) probes it.
- **Node RPC:** cuprated serves only restricted RPC, on `127.0.0.1:18081`; the unrestricted
  server is off and nothing puts RPC on the network. Apps reach it through the role
  (`public_rpc_url` or `rpc-port`), which grants their sandbox that loopback port; the
  helper reads `get_info` from it for Home, since the TUI may not. The firewall opens only P2P
  (18080) inbound. cuprated-service's default `Cuprated.toml` and the helper's template in
  nodo-tools must agree, which `crates/nodo-install/tests/cuprated_package.rs` checks.
- **Ethernet address:** the T6N's RTL8125B has no programmed address, so the kernel picks a random
  one every boot and the router hands out a new IP each time. `/etc/runit/core-services/03-nodo-fixed-mac.sh`
  runs `/usr/libexec/nodo/nodo-fixed-mac` after udev and before dhcpcd: every wired card whose
  address is locally administered (random) gets a fixed, locally administered unicast address
  from SHA-256 of the board serial (`/proc/device-tree/serial-number`) and the interface name. A
  card with a burned-in address, Wi-Fi and virtual interfaces are left alone. So DHCP reservations
  and port forwards keep working (`tests/nodo-fixed-mac.sh`).
- **Images:** nodo-base keeps its `dhcpcd-nodo-local.conf` at the end of `/etc/dhcpcd.conf`
  (dhcpcd owns that file and has no include): its INSTALL script puts the block there between
  marker lines on every install, upgrade and re-install, replacing an older copy, including the
  unmarked one images appended before, and REMOVE takes it out. The image builder runs the same
  script, so devices updated in place and new images agree. The `nodo-local` lease brings only an
  address, with no routes, DNS, hostname, IPv6 or 169.254 fallback. nodo-localnet reads that address
  from the kernel, not with `dhcpcd -U`: dhcpcd's control socket serves only a client of the running
  daemon's version, and an update replaces the client before the daemon restarts. Images leave out files nothing
  here reads (`scripts/_build-image-inner.sh` lists them and why), and `scripts/build-image.sh`
  fails a board over its size budget (`tests/size-check.sh`). The T6N images carry the platform (nodo-tools,
  nodo-caddy, nodo-ddns, nodo-localnet) and start nodo-helper, nodo-healthd and
  nodo-autoupdate at boot; the helper starts the web services when sites need them. They also
  start udevd, which loads the board's driver modules and firmware and handles USB hotplug, and
  `nodo-sound` for the mixer state; [MAINLINE_KERNEL.md](MAINLINE_KERNEL.md) lists the hardware
  setup. The helper's run script does not rely on udev's `/dev/disk` links: it takes the data
  drive from the `NODO_BLOCKCHAIN` line in
  `/etc/fstab` (the device mounted there, or the partition `blkid` finds by probing) unless its
  `conf` names one, and the health daemon guards the node's writes on that drive. Monokulo
  is a feed app and is installed through the helper, never baked into an image.
- **Service rules:** Run scripts must `exec </dev/null`, because a console stdin makes cuprated flood its
  log. bwrap does not forward signals and `--die-with-parent` would SIGKILL the child, so `control/t`
  signals the real process. `packages/cuprated-service/` is the reference implementation.
- **Privilege:** The TUI is unprivileged. A small root helper performs a fixed action list (`nodo-api`).
- **Settings:** Packages describe settings in sandboxed Lua with no io, os or network access, plus memory
  and instruction limits. It never runs as root. Settings are declarative, and the API is versioned.
  A package removes a setting by listing it under `retired`: a value a device stored for it is
  dropped, with a line in the helper's log, at the next write of that package's settings. A stored
  value for a setting neither declared nor retired is refused, so an update that loses a setting by
  accident fails its install check and rolls back. cuprated retires `restricted_rpc`.
- **References:** Public settings are referenced as `$(package.setting)`. Only Rust resolves them, inserting
  typed, escaped values, and never through a shell. Secrets are never public and need an explicit grant
  approved by the user. A reference implies the dependency and the sandbox and firewall access.
- **Encrypted storage:** Packages opt in. One single device passphrase unlocks all of it at once after a
  reboot. There are no per-package passphrases. Each volume is mounted only inside its own sandbox.
  A manifest may ask for a volume size and declare a data layout version. An update keeps the
  volume, grows it in place when asked (with the device passphrase, since LUKS2 needs the key to
  resize), records the layout, refuses a version older than the data's layout, and rolls back by
  copying the whole volume into itself first (`docs/UPDATES.md`, section 2).
- **Backups:** Encrypted with their own passphrase and holding the device key, settings, secrets,
  storage, Caddy's storage, feeds, pins, approvals and the verified package files. They go to the
  device's backup folder or a removable USB drive (removable or USB-attached only, never the system
  disk or the `NODO_BLOCKCHAIN` drive, vfat, exfat or ext4, `nosuid,nodev,noexec`, temporary name,
  sync, rename). A device that is not set up can be restored whole from a USB drive at first boot;
  packages come back from the kept verified copies, so no feed is needed (`docs/UPDATES.md`, section 5).
- **Sealed secrets:** Secret setting values are stored sealed with XChaCha20-Poly1305 under a key derived
  from the device key (HKDF-SHA-256, `nodo-secrets-v1`), as `v1.` and base64 of nonce and ciphertext, with
  the package and setting as associated data. That key exists only in the helper's memory after an
  unlock. An app with secret settings reads its rendered settings from `/run/nodo/secrets/<id>`, so
  after a reboot it waits for the unlock like encrypted storage. Secrets stored in clear by older
  helpers are sealed at the first unlock. Secret values cannot be set before the passphrase exists.
- **Settings to services:** Resolved settings reach a service as `NODO_*` environment variables from a
  root-only file read by its run script, never in argv. Every settings change regenerates the sandbox
  and firewall fragment from the references it resolves, re-renders the packages referencing it, and
  rolls all of them back together if any fails. Role bindings, approved grants and data drive choices
  are kept in `/var/lib/nodo/bindings.toml`, and the install marker records the feed.
- **Feeds:** Users add feeds, so packages are untrusted. Signing keys are pinned, and key changes and
  downgrades need approval. Reject install scripts, setuid files, paths outside the app directory and
  base package names. Feeds stay out of the system repo config. Permissions are shown for approval, and
  again when an update asks for more. Feeds may be `https://`, `http://` or a directory. Plain HTTP is
  allowed because the index must name the pinned key and every package must match the index and carry
  that key's signature, so nothing changed on the way installs. What HTTP leaves open, and the TUI says
  so when such a feed is added: someone on the path can hold back updates, offer an older signed
  version still above the installed one, and on first use show a key of their own, which only
  comparing the fingerprint with the publisher catches. Downloads are bounded in size and time,
  follow at most five redirects to any host (GitHub release downloads use two), and never from HTTPS
  down to HTTP.
- **Default feed:** Every image ships the project's app feed, `https://nodo.monokulo.com/feed`
  (GitHub Pages), in `/usr/share/nodo/default-feeds.toml` from nodo-tools, with its key's
  fingerprint. The helper adds it once, already pinned to that key, so there is no trust on first
  use; the address is then remembered, so a feed the owner removed never comes back and a
  fingerprint changed in a later image never replaces a pin (another key from the server is the
  normal key change). CI signs it (`.github/workflows/feed.yml`, `scripts/publish-feed.sh`) with a
  key kept as an Actions secret, and runs every package through the device's install review before
  deploying. Unlike the system keys, which UPDATES.md keeps offline, anyone who can get a workflow
  run on `main` can sign for this feed. That is accepted because feed packages are untrusted on the
  device anyway (gate, permissions, no downgrades), while a snapshot is trusted with the whole
  system. [FEED.md](FEED.md) covers publishing, the key and adding an app.
- **Auto-update:** Off by default and chosen per package, within a scheduled window. It never installs
  an update that needs new permissions, has a changed feed key, fails install checks, is on hold or needs
  a system restart, and it waits while the device is locked or low on disk. A failed update rolls back
  and that version is held until the user chooses to retry. System snapshot updates are a separate
  switch, also off: the schedule then only checks and reports, and a snapshot needing a restart
  always waits. A withdrawn snapshot is replaced by a newer manifest that names it; only devices on
  it move this project's packages back (`docs/UPDATES.md`, section 1).
- **Web:** Local network, Tor onion and public exposure are independent, so a site can use all three.
  Caddy is the proxy, with config generated by the helper. Domains are bring-your-own only. Dynamic DNS
  is a Rust service with provider plugins. Providers without an API are not supported, and there is no
  guided DNS migration. Public hosts are served on the main LAN address, which the router forwards to. Local hosts
  are served only on a second LAN address, at 443 with no port in the URL, from a macvlan interface
  with its own DHCP lease or a static address, and their `.local` names come from a small mDNS
  responder. Forwarded traffic is always addressed to the main address, so even a router that
  masquerades it cannot reach local hosts. Without a second address, local sites stay unavailable
  and are never moved to the main address or a port. Admin paths are always blocked on public and
  onion addresses.
- **TUI navigation:** Four tabs, Home, Apps, Web and System (`1`–`4`, Tab, or a click). Home shows
  what needs the owner first, one card and one button each (unlock, an app that keeps stopping, updates
  ready), then the other apps and one line of device health. Apps lists the installed apps; each has
  its own page with its update, Restart, Stop and Remove on the breadcrumb row (folded into "⋯ More"
  under 100 columns), and rows for Hosting, Settings and Logs. Get apps (`g`) searches the sources,
  and Sources (the old Feeds) sits under it, a card per source. System holds the Nodo's own update,
  then Updates (the Nodo and the apps on one page, each with its own button), Backups, Network,
  Device logs and About; automatic updates and undoing an update are reached from Updates. Every
  screen uses the same pieces (`crates/nodo-tui/src/view/v3.rs`): surfaces instead of lines, cards
  as even rectangles, inset fields, buttons inside the card they act on with a ▸ on the one Enter
  presses, and one dim line of keys. Screens drawn as bordered panels (backups, restore, undo,
  network, onion, setup, domains) go through one converter into the same look.
- **Removing an app:** `RemovePackage` stops an app installed from a feed and removes its files,
  manifest and installed record. Its data volumes stay on the drive, so installing it again brings
  everything back; deleting data is never a side effect. Base system packages are refused, and so
  is an app with a site still online (take it offline in Hosting first). The TUI asks first on its
  own screen, with the focus on Cancel.
- **Web screens (flow v3):** The TUI's Web tab lists only the sites that are online, each with who can
  open it in words; clicking one, or pressing `h` on an installed app's page in Apps, opens that
  app's Hosting. There the owner ticks Home network, Tor and Internet in any combination, and for the
  internet picks a domain from any connected provider with an address part (`pay` + `.example.com`). Home
  network and Tor apply at once; the internet only turns on after the reachability checks pass and
  the owner presses "Put … online" on a screen that shows the address and what becomes public, with
  the focus starting on Cancel (the helper still requires the host quoted back, which the screen
  sends). Domain providers are added once each (Web › Domain providers): pick the company and paste
  the key; the helper asks the provider which domains the key controls, which also proves the key,
  stores the key sealed, and the TUI returns to the Hosting screen it came from. A provider whose API
  cannot list domains (GoDaddy's Personal Access Token API, as implemented) asks for the domain, which
  the helper proves with a temporary record. The owner can connect several providers; the providers
  screen lists each with its domains, with Remove (refused while an app's internet address is under
  one of its domains) and "look again" (`r`). Technical terms (records, ports, the challenge) are
  behind "Show details"; certificates and home names live under Web › Domain providers › Certificates
  and home names.
- **Domain providers (helper):** `ListDomainProviders`, `AddDomainProvider`, `RemoveDomainProvider`,
  `RefreshDomainProvider` and `AddProviderDomain` manage them; credentials are sealed per field in the
  root-only `/var/lib/nodo/secrets/web-dns.toml` and never returned to clients. A domain belongs to
  the first provider that holds it, so one account publishes its records and answers its
  certificates; another provider listing the same domain shows it as held there. A site's provider
  follows from its address: the longest connected domain the host is under. nodo-ddns gets one zone
  per domain with public records, each with its own state file
  (`/var/lib/nodo-ddns/<domain>.json`), and Caddy gets one ACME policy per provider, each with that
  provider's DNS-01 credentials. A device updated from the single-provider form keeps its provider as
  the first one, converted on the helper's next apply without opening the sealed values, and its
  dynamic DNS state moves to that domain's file. `SetDnsProvider` and `TestDnsProvider` still work on
  the first provider.
- **Mouse:** The TUI turns on mouse reporting (crossterm's `EnableMouseCapture`; it is turned off
  again on exit and from the panic hook). Screens declare click targets by the text they draw, which
  are found in the drawn frame (`crates/nodo-tui/src/hit.rs`), so a click lands on exactly what is
  shown at any size. Left click activates, the wheel scrolls like the arrow keys, and pointer moves
  give the target under the pointer a hover fill one surface lighter, distinct from the keyboard
  selection; a move only redraws when the hovered target changes. Buttons get rounded ends (Nerd Font
  Powerline caps) where `[tui] rounded_buttons` in `~/.config/nodo/tui.toml` or `/etc/nodo/tui.toml`
  says `"on"`, or with `"auto"` (the default) in kitty, WezTerm and Ghostty, which bundle the symbols;
  elsewhere they are flat with plain ends.
- **Node onion address:** The node is not a site, so it has its own switch (`SetNodeOnion`, the TUI's
  Web › Onion screen). Tor forwards the onion address straight to the node's restricted RPC on
  loopback, never to a full RPC: to the port the node exports as its public `restricted-rpc-port`
  setting, which for cuprated is 127.0.0.1:18081, its only RPC. So the switch needs no setting
  first; a node that exports no such port is never put on Tor. When every onion address is off,
  the helper rewrites the torrc without services and stops Tor, which before kept publishing them.
- **Local address changes:** When the local address changes (a new lease, a lost one, or a new
  static address), nodo-localnet sends the helper a root-only `LocalAddressChanged` as it writes
  its new state, and the helper also checks once a minute in case that message was lost. The helper
  compares the address in the plan with the one it last applied (`/var/lib/nodo-web/local-address`)
  and on a difference loads Caddy's local listeners and the firewall again, restarts the mDNS
  responder, which reads its address at start, and rewrites the A records of local names under the
  owner's domain. Records need the sealed provider credentials, so while locked they wait for the
  unlock and `LocalSiteStatus` says why in plain words; while the address is gone they are left
  alone rather than removed and written again.
- **Local CA trust page:** While any `.local` site uses Caddy's internal CA, Caddy serves a plain
  HTTP page at `http://nodo.local` (also by the bare local address) on the local address's port
  80, which nftables sends to a listener bound to that address alone, for LAN sources only. It
  offers the root in DER and PEM, its SHA-256 fingerprint, and short install steps per platform.
  The helper writes the files to `/var/lib/nodo-web/trust` (root:_caddy) from the root Caddy
  reports, and the TUI shows the same fingerprint under Web › Domain providers › Certificates and home names ›
  Local CA so the owner can
  compare before trusting a page that is necessarily unencrypted. No public, onion or main-address
  listener has a route to it.
- **Certificates:** Public certificates come from Let's Encrypt through Caddy's ACME issuer, with an
  optional email and a staging choice. The helper picks the challenge: DNS-01 when the provider has a
  bundled caddy-dns module and its credentials are stored, else HTTP-01 and TLS-ALPN-01, which need
  80 and 443 forwarded. GoDaddy's module takes its own key and secret. DNS-01 credentials are opened
  from the sealed store into the Caddy config, root:_caddy 640, so renewals go on while locked; that
  file is the one copy at rest in clear. Caddy's storage, `/var/lib/caddy` (700 `_caddy`), is backed
  up with the secrets. Status per host comes from that storage and a certificate log Caddy writes,
  and success is shown only once a certificate is stored. `.local` names with Caddy's internal CA
  stay the default. Local sites can instead be named under the owner's domain: the helper publishes
  an A record with the local address through the provider, the certificate comes through DNS-01,
  and the name is still served only on the local listener. This publishes the LAN address, and
  routers with DNS rebinding protection can hide it, which a check on the Domains screen detects.
  Tests point the helper at Pebble with `/etc/nodo/acme-test.toml`; nothing else changes.
- **Radios:** Wi-Fi and Bluetooth are off until the owner turns them on from the TUI's Network
  screen (Home, `w`), and the helper soft-blocks a radio left off with rfkill at every start,
  since rfkill state does not survive a reboot. Wi-Fi is wpa_supplicant in nodo-tools' `nodo-wifi`
  service on the first wireless interface by name, never Void's `wpa_supplicant` service (the
  helper refuses to manage Wi-Fi while that one is enabled). The helper drives it over its control
  socket in `/run/nodo/wpa`, never through `wpa_cli` arguments. A network is added to the running
  wpa_supplicant and selected, and saved only once it connected; a refused passphrase removes it
  again. Saved networks are in `/var/lib/nodo/wifi` (0700): for WPA2 only the PBKDF2 key derived
  from the passphrase and the name, for WPA3 the passphrase, which SAE needs. They are not sealed
  with the device key, because Wi-Fi may be how the owner reaches the device to unlock it, and
  they are left out of backups. Open networks need the owner to accept that the traffic is not
  encrypted; WEP, WPA without WPA2 and enterprise networks are refused. dhcpcd, which manages
  every interface, leases the address. Its default route metric is 1000 plus the interface index,
  plus 2000 for a wireless interface, so a wired default route always wins and Wi-Fi carries
  traffic only while the wired link is down; the TUI asks before turning Wi-Fi off, or forgetting
  the joined network, while the wired link is down. Bluetooth is BlueZ through `bluetoothctl`
  with checked addresses: on starts D-Bus and bluetoothd, powers the controller and makes it not
  pairable and not discoverable, so only a pairing the owner starts can happen. Pairing runs on
  its own thread with a KeyboardDisplay agent, so the TUI shows a passkey to type on a keyboard,
  then trusts and connects the device. Off powers it down, takes bluetoothd down with a `down`
  file and blocks it. The VM tests join a WPA2 hostapd access point on a second mac80211_hwsim
  radio in a network namespace (`tests/wifi-prepare.sh` builds the module, which Void's kernel
  lacks); Bluetooth is tested with fakes only, as the VM has no virtual controller to pair with.
- **Screen:** The NanoPC-T6N's built-in 1920x1080 touchscreen shows package widgets as OLED
  tiles (design A made dark, approved on 2026-11-13; `docs/design/tui/ScreenTilesOled*.dc.html`,
  replacing the earlier terminal panels, H3): thin rounded outlines on a pure black background so
  unlit OLED pixels stay off, with the TUI's amber accent for the selection and bars and
  sparklines drawn in block characters from the screen's own console font. Panels sit on an 8x4 grid; the owner adds, hides, moves
  and resizes them by touch, mouse or keys (drag to move, drag the corner grip to resize; `hjkl` move, `HJKL` resize), and packages offer the panels,
  each in a range of sizes. A manifest declares its panels (`[[panels]]`); the screen fetches each
  one's data from the package's own service on loopback, runs the package's Lua `panel` function in
  the settings sandbox to compute the values, checks them against a fixed schema and draws them
  itself, so a package never draws or reaches beyond its own service. `nodo-gui` draws it in pixels
  straight to the display through KMS (Slint, software rendering), as `_nodo_screen`, in place of
  tty1's login prompt; `nodo-screen` is the same screen in a terminal, for SSH. Its Shut down button asks `nodo-power`, a
  root service whose socket only `_nodo_screen` may open and which accepts only `poweroff`: it stops
  every service (cuprated given five minutes to save), syncs and turns the device off.
  [SCREEN.md](SCREEN.md) is the contract.
- **Logging in over SSH** opens `nodo-tui` (`/etc/profile.d/nodo-tui.sh`, from `nodo-tools`).
  Only an interactive login shell on a terminal does: `ssh nodo some-command`, scp and sftp are
  untouched, and a shell started from that login does not open it again. Quitting it (`q`) leaves
  the normal shell, where `nodo-tui` opens it again; if it fails to start, the shell is there
  anyway. To skip it, `touch ~/.hushtui`, or log in with `ssh -t nodo sh` (not a login shell).

## Layout

- `packages/`: xbps recipes.
- `feed/`: the app feed's package list and public key ([FEED.md](FEED.md)); `.github/workflows/feed.yml` publishes it.
- `boards/`, `scripts/`, `tests/`: image builds and test tiers (`scripts/test.sh fast|vm|full`).
  Images take Void's packages from the live mirror (`VOID_MIRROR`), and xbps-src builds ours
  against the void-packages recipes at `VOID_PACKAGES_REV`. A Void dependency whose mirror version
  differs from its pinned recipe stops a package build, since xbps-src would otherwise compile the
  old recipe; the fix is a `VOID_PACKAGES_REV` with the mirror's versions. Image builds list every
  such difference in `out/images/<board>/void-drift.txt` (`scripts/_void-drift.sh`).
- `crates/`: the Rust workspace.
- `docs/design/tui/`: the TUI design, one `.dc.html` per screen, grouped in `canvas.json`.
