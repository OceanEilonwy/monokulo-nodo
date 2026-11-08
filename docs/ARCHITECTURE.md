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
  log says so; services without one cannot create Unix sockets at all. Sandboxes also get
  `--unshare-user --disable-userns`, so a service cannot create nested user namespaces. The frozen
  cuprated run script has neither change yet and needs both when it is next updated.
- **Service rules:** Run scripts must `exec </dev/null`, because a console stdin makes cuprated flood its
  log. bwrap does not forward signals and `--die-with-parent` would SIGKILL the child, so `control/t`
  signals the real process. `packages/cuprated-service/` is the reference implementation.
- **Privilege:** The TUI is unprivileged. A small root helper performs a fixed action list (`nodo-api`).
- **Settings:** Packages describe settings in sandboxed Lua with no io, os or network access, plus memory
  and instruction limits. It never runs as root. Settings are declarative, and the API is versioned.
- **References:** Public settings are referenced as `$(package.setting)`. Only Rust resolves them, inserting
  typed, escaped values, and never through a shell. Secrets are never public and need an explicit grant
  approved by the user. A reference implies the dependency and the sandbox and firewall access.
- **Encrypted storage:** Packages opt in. One single device passphrase unlocks all of it at once after a
  reboot. There are no per-package passphrases. Each volume is mounted only inside its own sandbox.
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
- **Auto-update:** Off by default and chosen per package, within a scheduled window. It never installs
  an update that needs new permissions, has a changed feed key, fails install checks, is on hold or needs
  a system restart, and it waits while the device is locked or low on disk. A failed update rolls back
  and that version is held until the user chooses to retry.
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

## Layout

- `packages/`: xbps recipes.
- `boards/`, `scripts/`, `tests/`: image builds and test tiers (`scripts/test.sh fast|vm|full`).
- `crates/`: the Rust workspace.
- `docs/design/tui/`: the TUI design, one `.dc.html` per screen, grouped in `canvas.json`.
