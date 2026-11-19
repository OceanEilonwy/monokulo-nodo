# The app feed

`https://nodo.monokulo.com/feed` is the project's own feed of apps: Monokulo (`mk-pay`), the Bell
(`mk-bell`), the push relay (`mk-push-relay`), the light wallet server (`mk-lws`), Kringle and the
Library (`library`). It is an ordinary xbps feed, checked on the device like any user-added feed (ARCHITECTURE.md, Feeds),
with one difference: every image ships it as a default, its key already pinned. Base system
packages never go in it; they reach devices in signed system snapshots (UPDATES.md).

| | |
|---|---|
| Address | `https://nodo.monokulo.com/feed` |
| Key | `62:54:a2:57:c2:57:9d:7e:11:8f:4c:30:a9:32:93:07` (RSA 4096, `feed/signing-key.pub.pem`) |
| Signed by | `monokulo-nodo feed` |
| Architectures | aarch64, x86_64 |

The fingerprint is the one xbps shows (MD5 of the key in SSH form), which nodo-feed's
`KeyFingerprint` uses and `ssh-keygen -l -E md5 -f <(ssh-keygen -i -m PKCS8 -f feed/signing-key.pub.pem)`
prints.

## How it is published

`.github/workflows/feed.yml` runs on every push to `main` and by hand (Actions, feed, Run
workflow; a run from any other branch builds but does not publish). It is never triggered by a pull
request.

1. **build** (one job per architecture, without the key): `scripts/publish-feed.sh build <arch>`
   builds the recipes listed in `feed/packages` with `scripts/build-packages.sh`, in the builder
   image, exactly as locally. The per-recipe stamps and built packages are cached, so a run only
   compiles recipes that changed; distfiles, cargo and npm downloads, xbps's package cache, the
   compiler caches (sccache for Rust, ccache for C) and the builder image are cached for the runs
   that do. Each package's log ends with its sccache and ccache hit rates. A run on any branch can
   be started by hand (`gh workflow run feed.yml --ref <branch>`); it builds but skips publish.
   Any file near GitHub Pages' 100 MB limit fails the job.
2. **publish** (main only, in the `github-pages` environment, which only `main` may deploy to):
   - `sign`: the key comes from the `NODO_FEED_SIGNING_KEY` secret into a 0600 temp file for that
     one step. `xbps-rindex -a`, then `--signedby "monokulo-nodo feed" --privkey <key> -s` (the
     public key into the index meta) and `--privkey <key> -S` (a `.sig2` per package: RSA PKCS#1
     v1.5 over the SHA-256 of the file), run in the pinned Void image without network.
   - `check`: `cargo run -p nodo-install --example check-feed` reads the signed feed as a device
     does, requires the key to be the one in `packages/nodo-tools/files/default-feeds.toml`, and
     runs every package through the installer's own review (size and hash against the index,
     signature against the pinned key, the package gate). Nothing is published if one fails.
   - `site`: the landing page (`index.html`, with the address, fingerprint and the package list
     taken from that check), `CNAME` and the public key, then the official Pages actions deploy
     `out/site`. The same site serves the system snapshots under `snapshots/`, published by
     `.github/workflows/system.yml` (docs/UPDATES.md, "The T6N's published snapshots"). A
     deployment replaces the whole site, so this job lists its own files in `files/feed.txt` and
     copies the snapshots from the live site by `files/system.txt` (`scripts/pages-site.sh`), and
     the two workflows' publish jobs share one concurrency group.

A run where no recipe changed takes a few minutes (restore the stamps, sign, check, deploy). A
cold run compiles everything on 4-core runners and can take two to three hours (x86_64 took 19
minutes locally on 32 cores); the build jobs time out at 330 minutes. The largest file today is
mk-pay at about 20 MB and the whole site about 66 MB, far inside Pages' 100 MB per file and 1 GB
per site.

If one architecture fails to build, the others still publish (aarch64 is required) and the run
shows a warning; the next good run brings it back.

The site:

```
/index.html                 what the feed is, its address, key and packages
/CNAME                      nodo.monokulo.com
/signing-key.pub.pem
/feed/aarch64-repodata      signed index, one per architecture
/feed/x86_64-repodata
/feed/<name>-<version>.<arch>.xbps
/feed/<name>-<version>.<arch>.xbps.sig2
```

Only the newest version of each app is published. A device keeps the verified file of what it
installed, so rollbacks never need an older one from the feed.

Locally, with the key: `scripts/publish-feed.sh build aarch64`, `sign <key> aarch64`,
`check aarch64`, `site aarch64`; the site lands in `out/site`.

## Who can sign

The key lives in GitHub, not on an offline machine as UPDATES.md asks for the system keys. Anyone
who can get a workflow run on `main` (push or merge access) can publish a signed package, and
anyone with write access could push a branch whose workflow reads a repository secret. This is
accepted because the feed is a feed: devices treat it as untrusted (no install scripts, nothing
outside the app's directory, permissions shown to the owner and asked again when they grow,
downgrades refused), whereas a system snapshot is trusted with the whole device. To narrow it, move
the secret from the repository to the `github-pages` environment (Settings, Environments), whose
branch policy allows only `main`, and protect `main` with required reviews.

## When devices read feeds

Reading a feed only updates what the device lists as available; it never installs anything. The
helper reads every enabled feed:

- when it starts (in the background, so its socket answers at once);
- when the owner adds a feed (the confirmed index is stored with it);
- on the `nodo-autoupdate` timer, whenever a feed was never read or was last read six hours ago or
  more. This does not depend on automatic updates, which stay off until the owner chooses
  packages for them;
- when the owner refreshes Sources (Apps › Get apps › Sources), and when the TUI finds a feed that was never read.

A feed that cannot be read keeps its last package list and records the problem, which Sources
and an empty Get apps search show.

## The default feed on devices

`nodo-tools` installs `/usr/share/nodo/default-feeds.toml` (from `packages/nodo-tools/files`).
The first time the helper reads its feed state on a device that never had a default, it adds it
as `monokulo`, enabled, pinned to the shipped fingerprint, before ever contacting the server; the
first refresh fills its package list. That first refresh happens straight away: the helper reads
every enabled feed in the background when it starts, so a new device lists the feed's packages
within seconds of booting. The address is then recorded in `defaults_seeded` in
`/var/lib/nodo/feeds.toml`, so:

- if the owner removes it, it stays removed, also across image updates (it can still be added by
  hand, confirming the key like any feed);
- a later image with another fingerprint never replaces the device's pin;
- if the owner already had that address, their feed and pin are kept;
- a server signing with another key is a key change, blocked until the owner trusts it by typing
  the feed's name.

A missing file means no defaults; a broken one is logged and ignored.

## Rotating the key

There is no automatic rotation: a device only moves to a new key when its owner approves the key
change. So rotate only when needed (the key leaked or is lost):

1. `openssl genrsa -out feed-signing-key.pem 4096` and
   `openssl rsa -in feed-signing-key.pem -pubout -out feed/signing-key.pub.pem` on a trusted
   machine; keep the private key backed up offline.
2. Put the new fingerprint in `packages/nodo-tools/files/default-feeds.toml` (the helper test
   `the_shipped_default_feeds_file_parses` checks it matches `feed/signing-key.pub.pem`), bump
   `nodo-tools`' revision, and replace the `NODO_FEED_SIGNING_KEY` secret
   (`gh secret set NODO_FEED_SIGNING_KEY < feed-signing-key.pem`).
3. Merge. New devices get the new key pinned. Existing devices show the feed as key changed;
   tell owners the new fingerprint through another channel (this page, release notes) so they can
   compare before trusting it.

If the old key leaked, also say so: anything signed with it after the leak is suspect.

## Adding an app to the feed

1. Write its recipe in `packages/<name>/` as a feed app: everything under
   `/opt/nodo/apps/<name>`, a `manifest.toml`, no install scripts, accounts or services, and a
   name that is not a base package name (`nodo-feed`'s gate; the `monokulo-` and `nodo-` prefixes
   are reserved). Add a test like `crates/nodo-install/tests/mk_pay_package.rs` for its files.
2. Build and check it locally: `scripts/build-packages.sh aarch64 <name>`, then sign a test feed
   with a throwaway key and run `check-feed` on it, or install it in the dev VM.
3. Add the name to `feed/packages` and merge. The next run builds it, the check runs it through
   the device's review, and the page lists it.

### A suite of apps, and a setup page

An app made of several packages can be installed in one go. One package, the head, lists the
others in `[bundle]`; each member is an ordinary package of the same feed with its own manifest and
permissions. The owner sees every package's review on one screen, answers the questions once, and
the members install in order, then the head. Its setup page then shows what is left to do.

```toml
[bundle]
# Packages of the same feed set up together with this one, installed in this order, before it.
members = ["pingers-push-relay", "pingers-map-host", "pingers-server", "pingers-ohttp-relay"]
# Members whose data the owner may keep on the data drive (offered on when one is present).
data_drive = ["pingers-map-host"]

# Grants approved with the install. The provider must come before the consumer.
[[bundle.grants]]
consumer = "pingers-server"
provider = "pingers-push-relay"
setting = "server_credential"

# Settings asked once, before anything installs, and applied before the package first starts.
# Its own settings file still checks the value. A secret is masked and sealed on the device.
[[bundle.ask]]
package = "pingers-push-relay"
setting = "fcm_service_account"
label = "Firebase service account"
help = "Paste the JSON key file. It is sealed on this device and never shown again."
secret = true

# A row on the app's page, opening the checklist its own service serves on loopback.
[owner_page]
title = "Setup"
url = "http://127.0.0.1:9475/owner"

[[network.inbound]]
port = 9475
protocol = "tcp"
scope = "local"
```

Limits: at most 16 members, grants and asks; a label up to 64 characters and help up to 512, on
one line; the owner page title up to 24 characters, its URL a `127.0.0.1` address on a TCP port the
package declares, with a path. A pasted file reaches a secret setting without its line breaks (the
TUI drops control characters from pastes), which for JSON changes nothing, since line breaks
there are only spacing and the settings' environment file cannot hold them.

The page itself is JSON, version 1, answered to a GET within 30 seconds, at most 64 KiB, with
status 200 (`nodo_manifest::owner_page::PageDoc` has the types and limits):

```json
{"version": 1, "title": "Pingers setup", "intro": "optional short paragraph",
 "steps": [{"title": "Put the four sites online", "state": "auto", "items": [
   {"kind": "text", "text": "ssh root@{device_address}, then …"},
   {"kind": "value", "label": "PINGERS_SERVER_HOST", "value": "https://pingers.example.org"},
   {"kind": "check", "label": "Push relay answers", "state": "ok", "detail": "…"},
   {"kind": "qr", "label": "Scan with the phone's camera", "data": "https://pingers.example.org/j#s1…"},
   {"kind": "site", "package": "pingers-server", "site": "api"},
   {"kind": "service", "package": "pingers-server"},
   {"kind": "domain", "domain": "example.org"},
   {"kind": "backup"}]}]}
```

A step's `state` is `done`, `todo`, `problem`, `unknown`, or `auto` (from its checks: all `ok` is
done, any `failed` a problem, otherwise to do; no checks is unknown). A check's `state` is `ok`,
`failed`, `waiting` or `unknown`. `site`, `service`, `domain` and `backup` are filled in by the
device: a site's address, the DNS record to create and live checks of it, whether a service runs,
whether a domain provider holds the domain, and whether a recent backup holds the device key. In
`text` and `value` items the device replaces `{device_address}`, `{public_address}` and
`{data_dir:<package>}` with its own values ("unknown" when it has none). Lengths: `text` and
`value` 2048 characters, titles and labels 128, `intro` 1024, a check's `detail` 512, `qr.data`
1024; at most 32 steps of 32 items. No control characters, except line breaks in `text`, `value`,
`intro` and `detail`.

## Feeds of other projects

Another project can build its own signed feed with these scripts, without copying recipes into
`packages/`:

- `NODO_EXTRA_PACKAGES=/abs/path/srcpkgs` adds a directory of recipes laid out like
  void-packages' `srcpkgs/` (a subpackage is a symlink to its recipe). `build-packages.sh`
  hashes, builds and stamps them like this repository's own; a recipe in `packages/` wins on a
  clash. A subpackage is built with its recipe and copied to `out/feed/<arch>` beside it; name
  the recipe, not the subpackage, to `build-packages.sh`.
- `publish-feed.sh` reads its package list from `FEED_PACKAGES` (default `feed/packages`; list
  subpackages beside their recipe), signs with the signer name `FEED_SIGNER` (default
  `monokulo-nodo feed`), and `check` requires the fingerprint `FEED_FINGERPRINT` (default: the
  one images pin). The key is still the argument to `sign`.

Such a feed is not added to devices by default: the owner adds it (`ProbeFeed`, compare the
fingerprint, `AddFeed`). Pingers is the first user (its `docs/deploy-nodo.md`).
