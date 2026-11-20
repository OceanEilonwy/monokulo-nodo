# App feeds

Apps reach devices through feeds: signed xbps repositories the owner adds by hand under Sources
(ARCHITECTURE.md, Feeds), confirming the feed's key. Images ship no default feed
(`packages/nodo-tools/files/default-feeds.toml` is empty). Base system packages never come
through feeds; they reach devices in signed system snapshots (UPDATES.md).

Each of the project's own apps publishes its own feed, from its own repository's releases:

| Apps | Repository | Feed address |
|---|---|---|
| `mk-pay`, `mk-bell`, `mk-push-relay` | OceanEilonwy/monokulo-pay | `https://github.com/OceanEilonwy/monokulo-pay/releases/latest/download` |
| `mk-lws` | OceanEilonwy/monokulo-lws | `https://github.com/OceanEilonwy/monokulo-lws/releases/latest/download` |
| `kringle` | OceanEilonwy/kringle | `https://github.com/OceanEilonwy/kringle/releases/latest/download` |
| `library` | OceanEilonwy/library | `https://github.com/OceanEilonwy/library/releases/latest/download` |

Each repository's `nodo/README.md` gives its feed's key fingerprint, which is the one to compare
with what the device shows when the feed is added. The fingerprint is the one xbps shows (MD5 of
the key in SSH form), which nodo-feed's `KeyFingerprint` uses and
`ssh-keygen -l -E md5 -f <(ssh-keygen -i -m PKCS8 -f nodo/feed-signing-key.pub.pem)` prints.

## How an app publishes its feed

The app keeps its recipe with its source, and tagging a release publishes it:

1. The app's repository is tagged `v<version>` (Kringle's CI tags a release itself when
   `Cargo.toml`'s version is new; the others are tagged by hand, or by their own release
   workflows). Its workflow calls this repository's `.github/workflows/app-release.yml`, which
   runs there, with that repository's token, caches and key.
2. `prepare` (`scripts/app-release.sh pin`): each recipe in the app's `nodo/<package>` is copied
   into a checkout of this repository's `packages/` with `version=` from the tag (`v1.2.0` is
   `1.2.0`, `v1.2.0-rc1` is `1.2.0rc1`), `_commit=` the tag's commit and the first checksum that
   commit's GitHub tarball's. The revision stays when the version is the recipe's own and is 1
   otherwise. `newer` refuses a version that is not newer than every one the repository's other
   releases carry, which devices would refuse as a downgrade or never install. The app's tests
   in `nodo/tests/<crate>/*.rs` (its files read with the device's own code) run against this
   repository's crates.
3. `build`, per architecture: `scripts/build-packages.sh`, in the builder image.
4. `release`:
   - the packages go through the device's own install review (`check-feed`) in a throwaway feed
     signed with a throwaway key;
   - `sign`: the app's key comes from its repository's `NODO_FEED_SIGNING_KEY` secret into a
     0600 temp file for that one step. `xbps-rindex -a`, then `--signedby "<repo> feed" --privkey
     <key> -s` (the public key into the index meta) and `--privkey <key> -S` (a `.sig2` per
     package: RSA PKCS#1 v1.5 over the SHA-256 of the file), in the pinned Void image without
     network;
   - `check`: the signed feed through the device's review again, now requiring the fingerprint of
     the app's `nodo/feed-signing-key.pub.pem` (from its default branch);
   - the feed (`aarch64-repodata`, `x86_64-repodata`, each `<package>-<version>_<revision>.<arch>.xbps`
     and its `.sig2`) is attached to the tag's release, created if the tag has none;
   - the feed is read back from `https://github.com/<owner>/<repo>/releases/download/<tag>`,
     through GitHub's redirect to its storage host, exactly as a device reads it.

Both architectures or neither. `releases/latest/download` serves the newest release that is not
a prerelease or draft, so the release is the feed from then on. Only the newest version is in a
feed; a device keeps the verified file of what it installed, so rollbacks never need an older one.

A recipe change on its own (say, a new pinned nightly) is released by tagging a new version, or,
for the same version, by raising `revision=` in the app's `nodo/<package>/template` and running
the app's `nodo` workflow by hand on that tag. `app-release.yml` takes `nodo-ref` to build with
another branch of this repository's scripts.

## Who can sign

Each app's key lives in its own repository's secrets, not on an offline machine as UPDATES.md asks
for the system keys. Anyone who can get a workflow run with that repository's secrets (push or
merge access, or a tag) can publish a signed package in that app's feed, and only that app's. This
is accepted because a feed is a feed: devices treat it as untrusted (no install scripts, nothing
outside the app's directory, permissions shown to the owner and asked again when they grow,
downgrades refused), whereas a system snapshot is trusted with the whole device. To narrow it,
move the secret into an environment whose policy allows only tags, and protect `main` and tags.

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

## Rotating an app's key

There is no automatic rotation: a device only moves to a new key when its owner approves the key
change. So rotate only when needed (the key leaked or is lost):

1. `openssl genrsa -out feed-signing-key.pem 4096` and
   `openssl rsa -in feed-signing-key.pem -pubout -out nodo/feed-signing-key.pub.pem` on a trusted
   machine, in the app's repository; keep the private key backed up offline.
2. Replace the secret (`gh secret set NODO_FEED_SIGNING_KEY -R <owner>/<repo> < feed-signing-key.pem`),
   put the new fingerprint in the app's `nodo/README.md`, and merge.
3. Release (a new tag, or the `nodo` workflow by hand on the latest tag). Devices that added the
   feed show it as key changed; tell owners the new fingerprint through another channel (the
   README, release notes) so they can compare before trusting it.

If the old key leaked, also say so: anything signed with it after the leak is suspect.

## Adding an app

1. Write its recipe as a feed app: everything under `/opt/nodo/apps/<name>`, a `manifest.toml`,
   no install scripts, accounts or services, and a name that is not a base package name
   (`nodo-feed`'s gate; the `monokulo-` and `nodo-` prefixes are reserved). Put it in its
   repository's `nodo/<name>`, with its first distfile
   `https://github.com/<owner>/<repo>/archive/${_commit}.tar.gz`, and a test of its files in
   `nodo/tests/nodo-install/` (like Kringle's `nodo/tests/nodo-install/kringle_package.rs`).
2. Make its key (as under "Rotating an app's key", step 1), set the `NODO_FEED_SIGNING_KEY`
   secret, commit `nodo/feed-signing-key.pub.pem`, and add a workflow calling `app-release.yml`
   on its tags (secrets: inherit, contents: write).
3. Build and check it locally: copy the recipe into a checkout of this repository's `packages/`,
   `scripts/build-packages.sh aarch64 <name>`, then `scripts/app-release.sh check <name> --
   aarch64` signs a throwaway feed and runs `check-feed` on it; or install it in the dev VM.
4. Tag a release, then add `https://github.com/<owner>/<repo>/releases/latest/download` on the
   device.

## A suite of apps, and a setup page

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

## Keeping paths of a site off some addresses

A site is served whole at every address the owner gives it, unless its manifest says otherwise.
Named rules under a `[[web]]` entry keep a path, and everything under it, off some kinds of
address, which then answer 404 for it:

```toml
[[web]]
name = "vault"
upstream = "127.0.0.1:8080"

# Not from the internet or Tor; still on the home network.
[web.paths.admin]
path = "/admin"
blocked_on = ["public", "onion"]

# Only on the home network.
[web.paths.metrics]
path = "/metrics"
allowed_on = ["local"]
```

Each rule has a `path` and exactly one of `allowed_on` and `blocked_on`, lists of `public`,
`onion` and `local`; `allowed_on = []` blocks the path at every address. The name, which follows
the site-name rules, labels the rule's line in the install review. At most 16 rules a site. The
path is plain: it starts with `/`, is not `/` itself, has no empty, `.` or `..` segments or
trailing `/`, and holds only letters, digits and `/ - _ . ~`. Rules add up, so a longer path
cannot reopen what a shorter one blocks. The path is matched without regard to case and also
when followed by a character that cannot continue a name, so `/admin` blocks `/Admin/x`,
`/admin;x` and `/admin.` but also `/admin.json`, and not `/administrator`. An update that loosens
or removes a rule asks the owner to approve it again; adding or tightening one does not.

## Feeds of other projects

Any project can build a signed feed with these scripts, through `app-release.yml` as the
project's own apps do, or on its own, without copying recipes into `packages/`:

- `NODO_EXTRA_PACKAGES=/abs/path/srcpkgs` adds a directory of recipes laid out like
  void-packages' `srcpkgs/` (a subpackage is a symlink to its recipe). `build-packages.sh`
  hashes, builds and stamps them like this repository's own; a recipe in `packages/` wins on a
  clash. A subpackage is built with its recipe and copied to `out/feed/<arch>` beside it; name
  the recipe, not the subpackage, to `build-packages.sh`.
- `publish-feed.sh` reads its package list from `FEED_PACKAGES` (list subpackages beside their
  recipe), signs with the signer name `FEED_SIGNER` (default `monokulo-nodo feed`), and `check`
  requires the fingerprint `FEED_FINGERPRINT`. The key is the argument to `sign`. For a feed
  published as a static site, `site` writes a landing page around it from `FEED_ADDRESS` and
  `FEED_PUBLIC_KEY`.

The owner adds such a feed by hand (`ProbeFeed`, compare the fingerprint, `AddFeed`). Pingers was
the first project to publish one (its `docs/deploy-nodo.md`).
