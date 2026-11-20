# Web path rules

## Intent

A package can keep parts of a site off some of the addresses the owner gives it: Vaultwarden's
`/admin` off the internet, an app's `/metrics` on the local network only. Nodo never decides this
for a package. There are no default rules and no special names such as `/admin`: a site with no
rules is served whole at every address, exactly as today.

This is a new feature, not the removed `admin_paths` (39837be) back under a new name. The rules are
not about admin pages, so nothing in the manifest, code, permissions or review text says "admin".

## Manifest

Rules belong to a site, as a table of named rules under that `[[web]]` entry:

```toml
[[web]]
name = "vault"
upstream = "127.0.0.1:8080"

[web.paths.admin]
path = "/admin"
blocked_on = ["public", "onion"]

[web.paths.metrics]
path = "/metrics"
allowed_on = ["local"]
```

- `paths` is optional; each key is the rule's name, shown in the install review. Names follow the
  site-name rules. At most 16 rules per site.
- Each rule has `path` and exactly one of `allowed_on` or `blocked_on`, each a list of address
  kinds: `public`, `onion`, `local`. Duplicates in a list are refused. `allowed_on = []` blocks
  the path at every address; `blocked_on = []` is refused (it would do nothing).
- `path` is a plain absolute path: it covers itself and everything under it (`/admin` covers
  `/admin` and `/admin/x`, not `/administrator`). Refused: no leading `/`, `/` itself, `//`, `.`
  or `..` segments, a trailing `/`, `?`, `#`, `%`, `*`, `;`, whitespace or control characters,
  anything outside letters, digits and `/ - _ . ~`, and paths over the existing path length limit.
- Unknown keys in a rule are refused. The old `admin_paths` key stays read-and-dropped exactly as
  now (it keeps installed manifests parsing); it never becomes a rule.

## Proxy

A request is blocked at an address kind when any rule matching its path blocks that kind. Rules add
up; a longer path cannot reopen what a shorter one blocks. A blocked request gets the same plain
404, before rate limiting and before the upstream is tried, so it answers the same whether the app
runs or not.

Matching must not be dodged by upstream path normalisation: match case-insensitively, the path,
`path/*` and `path;*`, plus the path followed by any character that cannot continue a segment name
(the old regexp approach: so `/admin.`, `/admin%20`, `/admin%00` are blocked too, at the cost of
`/admin.json`). Requests are matched on Caddy's normalised path as before.

Generated config with no rules is byte-identical to today's.

## Install review and approvals

The review shows one line per rule, e.g. `keeps /admin of 'vault' off public, onion addresses` and
`serves /metrics of 'vault' only on local addresses` (wording left to the builder, without "admin"
unless it is the rule's own name or path).

Rules protect the owner, so loosening one asks for approval again, like a permission that grows:
for each approved rule (site, path) whose blocked set in the new version is missing a kind it had
(including the rule disappearing), the update needs approval. Adding or tightening rules needs
none. Approvals stored before this change, with no rules, still read.

## Approaches considered

- A flat `forbidden_paths = ["/admin"]` list with one fixed meaning (public and onion). Simplest,
  but these rules are not only for admin pages and differ in where they apply.
- Named rules with `allowed_on`/`blocked_on` per path (chosen, the owner's own shape). The name
  labels the review line; either form is allowed because some rules read better one way.
- Longest-path-wins exceptions. Refused: an exception can reopen a path by accident.

## Left to the builder

Exact error and review wording, the Rust types, where the matcher code lives, test layout, and the
docs (ARCHITECTURE.md Web section, FEED.md manifest notes). Package revisions are raised as the
repository's revision check requires.
