# Shared UI core and a web GUI: design

Date: 2026-11-19. Status: built, both parts. The owner asked for the work to proceed without
further questions; the calls below stand as made, "Review resolutions" overrides the text above it
where they differ, and "As built" at the end lists where what was built differs from both.
docs/ARCHITECTURE.md ("Web GUI", and the Layout entries for `nodo-ui-core` and `nodo-web-gui`)
describes the result.

## Intent

The device has two management frontends: `nodo-tui` (ratatui, over SSH) and `nodo-gui` (Slint, on
the touchscreen). The owner wants a third, `nodo-web-gui`, so the admin can manage the device from a
browser on the home network, logging in with the device passphrase, with everything the TUI can do.

Today the logic that decides what happens lives in the TUI crate, and the GUI re-implements parts of
it. A third frontend written the same way would be a third copy. So the logic moves first into a
library crate that all three frontends use, and the TUI and GUI become thin wrappers over it.

This is two projects, built in this order, each with its own plan and pull requests:

- **Part A:** the library `nodo-ui-core`, and the TUI and GUI moved onto it.
- **Part B:** `nodo-web-gui`, plus the helper, Caddy and packaging changes it needs.

## What the owner decided

- The logic behind the TUI and GUI is lifted into its own crate; the TUI and GUI become thin wrappers.
- The web GUI uses Rust, maud, htmx 4, hyper and axum.
- It is for the admin, on the local network.
- Login is the device passphrase. There is no separate web password.
- It is set up over SSH.
- It is as powerful as the TUI.

Everything else in this document is a call made while writing it, marked **(call)** where it
matters. The owner did not choose between the three library approaches below; approach 2 is the
writer's recommendation.

## Goals

- One place for every rule and every sequence of helper calls. A rule is written once.
- `nodo-tui` behaves exactly as before. Its 39 snapshot files stay byte-identical.
- `nodo-gui` keeps its screens and look, and gains the rules it skips today (see Part A, step 4).
- The web GUI covers every TUI feature (the parity table in Part B).
- The web GUI is off until the owner turns it on over SSH.

## Non-goals

- Access from the Internet, or through Tor.
- More than one admin, roles, or a second factor.
- New screens on the touchscreen. It stays a subset; what it sends to the TUI today, it still sends.
- Progress reporting inside the helper. Long calls stay one blocking request.
- Arranging the touchscreen's panels from the browser.

## Anti-goals

These make the work a failure even if it runs:

- A rule that exists in two frontends after Part A, or three after Part B.
- The device passphrase sent over plain HTTP, ever.
- Any web GUI listener or Caddy route present while the web GUI is off.
- A changed TUI snapshot or a changed `nodo-tui --ctl` output that this work did not intend.
- The helper trusting the web process more than it must: a compromised `nodo-web-gui` that has not
  been logged into must be able to do nothing.

## Constraints found in the code

- `nodo-gui` already depends on `nodo-tui` as a library for `model`, `secret`, `helper_backend` and
  `mock`. `nodo-helper` dev-depends on it for `contract`, `helper_backend` and `model`.
- The TUI is split: `update::update(&mut App, Event) -> Vec<Effect>` is pure, and
  `runtime::execute_effect(backend, Effect) -> Vec<Event>` makes the helper calls. `Effect` and the
  result variants of `Event` hold no terminal types. The input variants (`Key`, `Mouse`, `Paste`,
  `Tick`, `Resize`) and every `*_key` handler do.
- Most per-screen `*State` structs in `app.rs` mix domain data and rules with cursor and focus.
- The GUI has its own flow in `manage.rs` (2,576 lines): a closure-based worker, its own `Screen`
  enum, and copies of `load_snapshot`, reference fit rules, save consequences and more. It never
  calls `validate`, and it applies updates without approvals or the growing-storage passphrase.
- The helper protocol is blocking and synchronous. Nothing in the workspace is async today.
- The helper authorizes by peer uid. The screen socket (`/run/nodo/screen.sock`, user
  `_nodo_screen`) is the pattern for a second policy: its own listener, its own allow list
  (`nodo-helper/src/screen.rs`).
- Every passphrase check goes through `verify_passphrase` and shares one failure counter. A wrong
  guess sleeps 2 s doubling to 60 s with the mutex held. There is no lockout. There is no request
  that only verifies.
- Only tcp 22 is open on the LAN by default. Caddy serves local sites on a second LAN address
  (`nodo-localnet`) at 443 with its internal CA, and a plain HTTP trust page at `http://nodo.local`
  only while a `.local` site is online. Sites come only from package `[[web]]` manifests; the trust
  page is the one special case (`nodo-web/src/caddy/mod.rs`).
- Most binaries ship in one package, `packages/nodo-tools`; `nodo-ddns` and `nodo-mdns` (built from
  `crates/nodo-web`) ship in `packages/nodo-ddns` and `packages/nodo-localnet`. Any change under `crates/` needs its
  `revision` raised. `Cargo.lock` is enforced with `--locked`. CI tests only `nodo-api`,
  `nodo-helper` and `nodo-tui`. Root filesystem budgets are per board (`tests/size-check.sh`).
- The crate name `nodo-web` is taken (Caddy, DNS, mDNS).

## Approaches considered for the library

1. **Move the data layer only.** `model`, the helper backend, the mock and `Secret` go to the new
   crate; each frontend keeps its own flow. Cheap and safe, but the flow logic then exists three
   times and the frontends are not thin.
2. **Shared flows, per-frontend navigation (chosen by the writer).** The library also owns the
   effects, their results, the code that runs them, and each feature's state and rules. A frontend
   keeps input mapping, focus and scrolling, navigation, and drawing.
3. **One shared screen state machine.** The whole `App` and `update` move, with keys replaced by
   semantic actions, and all frontends render one `App`. Thinnest wrappers, but it rewrites about
   7,000 lines of key handlers and all of `manage.rs`, forces the touchscreen's PIN and no-keyboard
   flows into the terminal's shape, and makes the web server hold a cursor-driven `App` per session,
   which fights browser tabs and the back button.

Approach 2 is chosen because the three frontends have different interaction models (keyboard focus,
touch with a PIN, HTTP requests), and it shares everything that decides what happens while leaving
each its own model.

---

## Part A: `nodo-ui-core`

A library crate at `crates/nodo-ui-core`. No ratatui, crossterm, Slint, tokio or axum. Its
dependencies are `nodo-api`, `zeroize`, and whatever the moved modules already need. It carries the
same `#![deny(clippy::let_underscore_must_use, unused_must_use)]` as the TUI.

### The rule for what goes where

- **Core:** anything that would have to be the same in a second frontend. Domain types, helper
  access, what an action does, what must be asked first, what a result means, validation, step
  order in a multi-step flow, wording of consequences and errors.
- **Frontend:** what key or tap or request maps to an action, where the focus is, scrolling, the
  navigation stack, layout and drawing, and device specifics (terminal modes, OSC 52, the screen
  PIN prompt, backlight, HTTP sessions).

### Steps

Each step lands green on its own: `cargo test --locked -p nodo-api -p nodo-helper -p nodo-tui
-p nodo-ui-core`, the TUI clippy line, and unchanged snapshots.

**Step 1: move the terminal-free modules.** `model`, `secret`, `refs`, `contract`,
`helper_backend`, `helper_manage`, `mock` move as they are. `nodo-tui` re-exports nothing from
them (call): every user is changed to the `nodo_ui_core::` path, so there is one name for each
thing. Users to change: the TUI itself, its tests and `examples/boards.rs`, `nodo-gui`,
`nodo-helper`'s dev-dependency and `tui_contract_tests.rs`. `clipboard`, `hit`, `input`, `theme`,
`view`, `proposal`, `ctl` and `main` stay in the TUI. `ctl.rs` stays whole for now; its `parse`
and `follow_up` are terminal-free but nothing else needs them yet.

**Step 2: move effects and their execution.**

- `Effect` moves to the core unchanged, with the small types it carries (`ExposureAfter`,
  `UsbView` and the like).
- `Event` splits. The result variants become `nodo_ui_core::Outcome`. The TUI keeps
  `Event { Key, Mouse, Paste, Tick, Resize, Done(Outcome) }`. This touches every place that builds
  or matches a result event; it is mechanical.
- From `runtime.rs`, `load_snapshot`, `execute_effect` and its helpers, `run_batch`, and
  `EffectWorker` move. They become usable with a `dyn` backend (`&mut dyn Data`, where
  `Data: Backend + ScreenData + Send` moves from the GUI), so the GUI can drop its copy of
  `load_snapshot`. `dispatch` and `dispatch_async` stay in the TUI because they call `update`.
- Derived queries on `App` that only read the snapshot move to functions on `Snapshot`:
  `active_sites`, `has_provider`, `domain_choices`, `pending_updates`, `is_locked`, `update_rows`,
  `search_results`, `home_items`, `app_sections`.

**Step 3: extract rules and flows, one feature at a time, TUI first.** For each feature below the
domain half of its `*State` becomes a core type in `nodo_ui_core::flows::<feature>`, and the TUI's
state becomes that type plus cursor fields.

A flow type has three kinds of method:

- **Actions**, named for what the owner means (`save`, `toggle_internet`, `add_alias`,
  `answer(ask, value)`, `confirm(typed)`), returning `Vec<Effect>`. No keys, no indices into a
  drawn list.
- **`apply(&mut self, &Outcome) -> Step`**, which takes a result and says what it means:
  `Step::Stay`, `Step::Done(Notice)`, `Step::Failed(Notice)`, `Step::Ask(Question)` for a
  confirmation the owner must answer, or `Step::Go(Dest)`.
- **Queries** for drawing: rows, errors per field, whether Save is allowed, the words of a
  consequence.

`Dest` is a core enum of destinations by meaning (`Hosting(site)`, `Providers { return_to }`,
`WebConfirm(..)`, `Unlock { resume }`, and so on). Each frontend maps a `Dest` to its own screen or
URL. `Question` replaces the TUI's `Modal`/`ModalAction` pair for the wording and the action; the
TUI keeps which button is focused.

Features, each one unit of work, in this order (simplest first, so the pattern settles early):

1. Services and app page: start, stop, restart, remove, the "site still online" block.
2. Unlock and first setup: retry delay, passphrase rules (`MIN_PASSPHRASE_CHARS`), setup steps.
3. Sources: probe, trust, toggle, remove, refresh, key change.
4. Get apps and install: search filter, gate, review and approval, install outcome, rejection,
   capability screen.
5. Settings: `validate`, `error_for`, previews, references (`fits`, `search`, grant), undo,
   `suggestions`, `save_consequences`, `apply_draft`, rollback explanation.
6. Updates: preselection, `apply_batch` with approvals and the growing-storage passphrase, held
   packages, auto-update prefs and `parse_window`, results, rollbacks.
7. Web: hosting draft and `save` chains, aliases, providers, reachability check, public confirm,
   CGNAT, ACME and domain, certificates and trust, node onion address.
8. Backups and restore: both step machines, USB.
9. Network: Wi-Fi and Bluetooth rules, polling, the open-network and Wi-Fi-off questions.
10. Bundles and owner pages.
11. Logs, documents, drive actions, SSH keys, copy report.

The text editing buffer `secret::Input` and `refs::PieceEditor` hold a cursor, but they are already
in the moved modules and the GUI's on-screen keyboard can use them; they stay in the core.

**Step 4: move the GUI onto the core.** `manage.rs` keeps its `Screen` enum, its Slint bindings,
the PIN prompt with re-run on `ApiError::Locked`, and its string action ids. Its closure `Job`
worker is replaced by the core's `EffectWorker`, `Effect` and `Outcome`. Each feature it has uses
the same flow type as the TUI. This changes GUI behaviour where it skipped a rule, and that is
intended:

- fields are validated before save;
- updates that need approvals or the growing-storage passphrase are sent to the TUI, as new
  permissions already are, instead of being applied without them;
- reference fit rules, save consequences and domain lists match the TUI's.

Features the GUI does not have stay absent.

### Tests

- Moved unit tests move with their modules.
- Each flow gets unit tests in the core against `MockBackend`, written before the extraction where
  the TUI's `flows.rs` does not already cover the rule.
- `nodo-tui/tests` keep passing unchanged except for import paths and `Event::Done(..)`.
- `nodo-gui`'s unit tests keep passing; tests for the newly gained rules are added.
- CI (`.github/workflows/crates.yml`): add `nodo-ui-core` to the test list and give it the same
  clippy line as the TUI. `nodo-gui` stays out of CI, as today (call: Slint's build is heavy and
  that is a separate decision).

### Packaging

`nodo-ui-core` is a library, so `packages/nodo-tools/template` needs no new `-p`; its `revision`
is raised in every pull request that touches `crates/`.

---

## Part B: `nodo-web-gui`

### What the admin sees

1. Over SSH, the admin turns it on: a row in the TUI's System tab, or
   `nodo-tui --ctl web-gui on`. The TUI then shows the address, `https://nodo.local`, and the
   local CA's fingerprint, with the same trust instructions local sites use.
2. In a browser on the home network they open `https://nodo.local`, get a login page, and type the
   device passphrase.
3. They get the same four tabs as the TUI: Home, Apps, Web, System, and everything under them.
4. They log out, or the session ends after 15 minutes idle.

Turning it off (`web-gui off`, or the same TUI row) ends every session and removes the listener and
the route.

### Pieces

**The process.** A new binary crate `crates/nodo-web-gui`: axum 0.8 on hyper and tokio, maud for
HTML, htmx 4 for partial updates. It runs as a new system user `_nodo_webgui` under a runit service
`nodo-web-gui`, listens only on `127.0.0.1:10082` (call), and is reached only through Caddy. htmx
is one vendored file compiled into the binary; nothing is loaded from the Internet, since the
device may be offline. CSS is hand-written and also compiled in.

**Caddy.** A new special case next to the trust page in `nodo-web`'s `generate()`: when the web GUI
is on, `nodo.local` and the bare local address get an HTTPS route on the local listener
(`<local_ip>:10444`, reached as 443) with the internal CA, proxying to `127.0.0.1:10082`, behind
the same LAN `remote_ip` matcher local sites use. The HTTP trust page at `http://nodo.local` is
then served whether or not a `.local` site is online, and gains a link to the HTTPS address. The
nft fragment for the local address is rendered for the same condition. Without a local address
from `nodo-localnet` the web GUI waits, as local sites do, and the TUI says so.

**The helper.**

- A third listener, `/run/nodo/webgui.sock`, mode `0600`, owned by `_nodo_webgui`, created only
  when `--web-uid` is given, serving only that peer uid. It follows the screen socket's code.
- A gate like `ScreenGate`. While closed, the socket answers only `WebLogin` and `WebStatus`;
  every other request, reads included, is refused. `WebLogin { passphrase }` verifies the device
  passphrase and opens the gate. The gate closes on `WebLogout`, after 15 minutes without a
  request, when the web GUI is turned off, and when the helper restarts.
- While open, the socket accepts what the main socket accepts from a member of `nodo`, except the
  root-only requests and `SetWebGui` (turning the web GUI on or off stays an SSH act).
- `WebLogin` also unlocks the device if it is locked (call): it is the same secret, and the admin
  would otherwise type it twice. Flows that ask for the passphrase again in the TUI (backups,
  owner pages, growing storage, bundles) ask again in the browser.
- `WebLogin` has its own failure counter and mutex, with the same backoff, separate from the one
  SSH unlock uses (call). Otherwise someone on the LAN guessing at the login page could hold the
  shared mutex and delay the owner's own unlock by up to a minute per guess.
- `SetWebGui(bool)` on the main socket, refused while the passphrase is not set up. It records the
  choice, starts or stops the `nodo-web-gui` service, and regenerates Caddy and nft. The choice
  and the address are reported in the overview so the TUI and GUI can show them.
- New `Request`/`Response` variants are appended at the end of the enums, as the protocol's rule
  requires; `PROTOCOL_VERSION` changes only if that rule cannot be kept.

**Sessions.** Held in the web process's memory, lost on restart. A session is a random 256-bit
token in a cookie named `__Host-nodo`, `Secure`, `HttpOnly`, `SameSite=Strict`, `Path=/`. Idle
timeout 15 minutes, hard limit 12 hours, at most 4 sessions (call). Every request that is not a
GET must carry an `Origin` matching the host it was sent to. Every response has
`Cache-Control: no-store` and a content security policy allowing only the page's own scripts and
styles. The web process adds its own limit of one login attempt in flight and a short per-address
delay before it forwards a guess to the helper. Passphrases travel in POST bodies, are held in
`Secret`, and are never logged.

**Using the core.** A handler loads what it shows with the core's `load_snapshot` and queries,
turns a POST into a flow action, runs the returned `Effect`s with `execute_effect`, feeds the
`Outcome`s to the flow's `apply`, and maps the `Step` to a response: a re-rendered fragment, a
notice, a confirmation fragment for `Step::Ask`, or a redirect for `Step::Go(Dest)`.

- Helper calls are blocking, so they run on tokio's blocking pool, with a limit below the helper's
  16 connections per listener.
- Single-request features keep no state on the server: the form carries the draft.
- Multi-step flows (bundle questions, backup, restore, adding a provider, going public) keep their
  core flow value in the session under a flow id, dropped when the flow ends or the session does.
- Long calls (install, update, backup, restore, checks) start a job in the session and return a
  fragment that asks again every second until the `Outcome` is there. The helper reports no
  progress, so the page shows what is running and for how long, like the TUI.
- Pages must keep answering while a long job runs. How backends are shared between a session's
  job and its page loads is left to the builder, within that requirement.

**URLs.** One per screen, readable, stable enough to bookmark: `/`, `/apps`, `/apps/<id>`,
`/apps/<id>/settings`, `/apps/<id>/hosting`, `/apps/get`, `/sources`, `/web`, `/web/providers`,
`/web/domain`, `/web/certificates`, `/system`, `/system/updates`, `/system/auto-update`,
`/system/logs`, `/system/backups`, `/system/restore`, `/system/network`, `/system/keys`,
`/system/onion`, `/login`, `/logout`. The exact list follows the parity table.

**Look.** (call) Dark, with the TUI's amber accent, built from the touchscreen design's tokens
(`docs/design/gui`), readable on a phone and a desktop. Mockups of Home, an app page, Settings and
Hosting are shown to the owner before the screens are built.

**Mock mode.** `nodo-web-gui --mock` serves plain HTTP on `127.0.0.1` over `MockBackend`, logging
in with `MOCK_PASSPHRASE`, for development and tests. It refuses to bind any other address.

### Parity with the TUI

Every `ScreenId` in the TUI has a page or fragment in the web GUI, with these exceptions:

| TUI | Web GUI |
|---|---|
| Help overlay and key hints | Not applicable |
| Copy through OSC 52 | A browser copy button |
| "Terminal too small" | Not applicable |
| Shell (a stub today) | Absent |
| Turning the web GUI on or off | Shown, not changeable |

A test in `nodo-web-gui` walks `ScreenId`'s list (moved to the core in Part A so both can name
it) and fails if a screen has neither a route nor an entry in the exceptions list.

### Tests

- Handler tests drive the axum router in-process over `MockBackend`: login and its failures,
  session expiry, the `Origin` check, and one journey per feature in the Part A list.
- Helper tests for the new socket: closed gate refuses everything but `WebLogin` and `WebStatus`;
  wrong uid refused; gate expiry; separate counter; `SetWebGui` refused on the web socket.
- `nodo-web` tests for the generated Caddy and nft with the web GUI on and off, including "off
  leaves no route".
- An end-to-end shell test next to `tests/helper-e2e.sh`: turn it on with `--ctl`, log in with
  curl against the VM, change one setting, turn it off, see the address stop answering.
- CI: add `nodo-web-gui` to the test and clippy lists.

### Packaging

In `packages/nodo-tools`: `-p nodo-web-gui`, its `vbin`, the `nodo-web-gui` service directory
(`run` with `exec </dev/null`, `log/run`), `/var/log/nodo-web-gui`, the `_nodo_webgui` account,
`--web-uid` in the helper's `run`, and a raised `revision`. The service is not in any board's
`BOARD_SERVICES`; the helper starts it. Each board's root filesystem budget is checked with
`tests/size-check.sh`; raise a budget only with the measured number, as was done for the GUI.
`docs/ARCHITECTURE.md` gains a "Web GUI" decision entry and the layout entries for both crates.

## Security note for the owner

Until now the device passphrase could only be tried by someone who already had an SSH key on the
device. With the web GUI on, anyone on the home network can try it at the login page. The backoff
(up to 60 s per wrong guess, about 1,400 guesses a day) and the 8-character minimum are what stand
in the way, and the web GUI can add and remove SSH keys like the TUI can, so the passphrase alone
then leads to a shell. This follows from the decisions above and is why the web GUI is off by
default; it is stated here so it is a choice and not a surprise.

## Left to the builder

- Module layout inside `nodo-ui-core::flows`, and the exact shape of `Step`, `Dest` and `Question`
  once the first two features have been extracted.
- How a web session shares helper backends between a long job and page loads.
- The mechanism the helper uses to start and stop the `nodo-web-gui` service, following how it
  starts `nodo-caddy`.
- The exact htmx 4, axum, tokio and maud versions, checked against the Rust version the pinned
  void-packages revision provides and against the workspace `rust-version`.
- Fragment boundaries and CSS.

## Calls to check

1. Approach 2 for the library.
2. `nodo-tui` re-exports nothing; all users move to `nodo_ui_core::` paths.
3. The GUI gains rules it skipped, and sends approval-needing updates to the TUI.
4. Web login also unlocks a locked device.
5. A separate failure counter for web login.
6. The web GUI can do everything the TUI can, SSH keys included.
7. Address `https://nodo.local` through Caddy with the local CA; process on `127.0.0.1:10082`.
8. Sessions: 15 minutes idle, 12 hours at most, 4 at once.
9. Look: dark with the amber accent, mockups before the build.
10. Names: `nodo-ui-core`, `nodo-web-gui`, `_nodo_webgui`, `/run/nodo/webgui.sock`.

## Review resolutions

A reviewer read this document against the code. These settle what it found.

**Names.** `Request::WebStatus` already exists and means site addresses. The new requests are
`WebGuiLogin`, `WebGuiLogout`, `WebGuiStatus` and `SetWebGui`. The `--ctl` form follows the
existing flags: `nodo-tui --ctl web-gui 0|1`.

**Status reporting.** `DeviceOverview` is a struct and cannot grow without a protocol bump. The
choice and address are reported by the new `WebGuiStatus` request, which the main socket, the
screen socket (as read-only) and the web socket (even while the gate is closed, answering only
on/off) accept.

**Gate and sessions.**

- The web process owns sessions and their idle clock. A request counts as activity unless it is
  one of the polling routes (the job fragment, Home refresh, followed logs, network polling).
- The helper's gate opens on `WebGuiLogin`. It closes on `WebGuiLogout`, which the web process
  sends when its last session ends or expires; 12 hours after the last successful `WebGuiLogin`;
  15 minutes after the last request finished with none in flight; when the web GUI is turned off;
  and when the helper restarts. A request in flight keeps it open, so a long backup cannot close
  it under itself.
- One session's logout ends only that session.
- If the helper refuses a request because the gate is closed while sessions exist (a helper
  restart), the web process drops every session and sends the browser to the login page. Every
  page load starts with `ListServices` through `Backend::call`, which keeps the typed error.

**Errors through `Outcome`.** `ScreenData` keeps its text errors; this work does not widen it.
The touchscreen's "ask for the PIN and run again" is kept by the mechanism the GUI step chooses
(for example a `take_refusal()` on the backend that reports the last typed `ApiError::Locked`),
without changing TUI wording or snapshots.

**`Step`.** A result can call for more helper calls, so `Step` has a variant carrying effects
(`Step::Run(Vec<Effect>)`), and a frontend loops until a step carries none.

**Backends are per session.** `HelperBackend` keeps state between calls (site ids, the drive check
id, update status). Each web session owns one backend behind a lock. While a long job holds it,
pages show the job as running instead of fresh data, as the TUI does with its `working` flag.
This replaces "pages must keep answering with data while a long job runs".

**Login then unlock.** `WebGuiLogin` verifies under its own counter, then takes the unlock lock
only for the mount step, without verifying a second time under it.

**Sandbox and loopback.** `nodo-web-gui` runs under `chpst -u _nodo_webgui` with the confinement
the other device services use (`nodo-confine`). The firewall lets only Caddy's user and root
connect to `127.0.0.1:10082`, so an installed app cannot reach the login page around Caddy.

**Caddy.** With the web GUI on and no `.local` site online, all of these must turn on for it: the
local HTTPS server, the internal CA policy and PKI app, the trust page, the local firewall block,
the mDNS name `nodo.local`, and the `nodo-localnet` service.

**Parity.** `/system/restore` is dropped: `ScreenId::Restore` is first-boot only, when the web GUI
cannot be on. Restore on a set-up device is part of the Backups page. Exceptions gain `Restore`
(first boot), `Unlock` (login does it; a locked device after a helper restart sends the browser
to login) and `Setup`'s passphrase steps. `Setup`'s SSH-keys step is not in `ScreenId::ALL`, so
`/system/keys` is covered by its own test.

**Revisions.** A change to `crates/nodo-web` also raises the revisions of `nodo-ddns` and
`nodo-localnet`.

**Step 1.** `fixtures.rs` stays in the TUI.

**htmx.** htmx 4.0.2 (npm tag `next`) is the version vendored.

## As built

Where what was built differs from the text above. Each item names what stands now.

**The process and Caddy**

- **Unix socket upstream.** `nodo-web-gui` listens on `/run/nodo-web-gui/http.sock`, not on
  `127.0.0.1:10082`, and Caddy dials that. A loopback port could be taken by any app before the web
  GUI started; the socket's directory (2750, `_nodo_webgui:_caddy`) lets only the web GUI's account
  and Caddy's group in. Port 10082 is not reserved, and the firewall rule that guarded it is gone:
  `_nodo_webgui` may send nothing on the network at all.
- **By name only.** The route answers `https://nodo.local` and not the bare local address: a
  browser sends no server name for an address, and the local listener's TLS policies answer only
  the names they know. The plain HTTP trust page still answers by the address.
- **Host check.** The process refuses a request whose `Host` is not `nodo.local` (421). The design
  only asked for the `Origin` check on requests that change something, which is there too and
  accepts a `Referer` of the same origin when a browser sends no `Origin`.
- **Referrer-Policy.** `same-origin`, not `no-referrer`, so that the `Referer` fallback of the
  origin check works; every other site still gets none.
- **`app.js`.** htmx is not the only script: a few compiled-in lines give a question's dialog the
  keyboard focus and Escape, and make the Copy buttons work. Nothing depends on scripts; every page
  works as plain links and forms.
- **Caddy after an upgrade.** Caddy's sandbox has `/run/nodo-web-gui` only if the directory existed
  when Caddy started, so a Caddy that was running before the upgrade must be restarted once.
- **hyper** is not a direct dependency; it comes with axum.

**The core**

- **`Step::All`.** Beside `Stay`, `Done`, `Failed`, `Ask`, `Go` and `Run`, a step can be several of
  them followed in order (a notice and a page to go to, a page and effects to run there).
- **Hints.** The core's words name no key and no button. A message that points at something to do
  carries a `Hint` (on a `Notice`, from a flow's `error_hint()`, on a description or a log's
  advice); the terminal turns it into its key (`nodo_tui::keys`), the web GUI into a link or the
  button (`nodo_web_gui::hints`), the touchscreen into its own words.
- **`take_refusals`.** The touchscreen's "ask for the PIN and run again" is a `take_refusals()` on
  the backend that gives every different typed refusal a batch met, each once, and
  `Finished::Batch` carries them, since one batch can be refused twice for different reasons.
- **`words`.** The short wordings every interface shows alike have a module of their own beside
  `flows` and `queries`, which the design did not name.
- **Long calls are the core's to name.** `Effect::is_long`, `Step::is_long` and `Confirm::is_long`
  say which calls take minutes; the web GUI starts a job for exactly those.
- **The landing rule** (first setup, the data drive's page, or Home) is `Snapshot::landing`.
- **The web GUI's state is in the snapshot.** `Snapshot::web_gui` is read with everything else
  (`WebGuiStatus`), instead of each interface asking for it.
- **`Dest::HostingWithProvider`** does not name its site: the provider pages are opened from
  several places and keep nothing about the one that opened them, so the interface that sent the
  owner there knows where to return. `Dest::LeaveHosting`, `Dest::WentPublic` and
  `Confirm::DiscardSettings` do name theirs.
- **A rollback the helper refused** gives no notice from `Rollbacks::apply`; its line on the page
  is `words::rollback_result`, shared by the three interfaces. A notice as well would have changed
  the terminal's behaviour.
- **The mock plays the web GUI's socket** (`knobs.web_socket`, `MockBackend::web_gui`), so the web
  GUI's tests and `--mock` log in, log out and ask for the status with the helper's own requests.
- **What `main` added to the terminal while this was built** was carried into the core when the
  branch was brought up to date, not back into the terminal: a public site gaining a name once
  the owner confirms it (`Hosting::save`, `Confirm::AddPublicNames`, which carries the draft it
  saves); asking every provider again as the list opens, a provider's details and the domains its
  key cannot change (`providers::open_list`, `ProviderStage::Details`, `providers::refreshed_notice`
  with `Hint::ProviderDetails`, and the wordings in `words`); an update refused before anything
  changed as its own outcome (`words::update_results`, `words::update_failure_said`); and the word
  for the step a busy button's work is on (`words::busy_step`). The terminal keeps the button
  that opens a new name's controls, the pending mark and its drawing. The web GUI's Hosting page
  has the same button and question, its provider cards show the key and the blocked domains (the
  cards are its details page), and its jobs already last for a whole chain of effects. Protocol
  version 6 is `main`'s; the web GUI's four requests still sit after every older one.

**The interfaces**

- **Snapshots.** The terminal has 40 screens, not 39: `ScreenId::WebGui` is the page with the web
  GUI's switch, and the System tab gained its row. Those are the only two snapshot files that
  changed or were added. The new screen has no design file under `docs/design/tui`.
- **The touchscreen's hosting confirmation** is a dialog over the Hosting page that starts on
  Cancel, with the address typed into it, and not a page of its own.
- **Turning the web GUI on or off on the web** is not offered, as the parity table says; the
  web GUI's System page shows the state and what the terminal's page explains, and the
  touchscreen's System page shows one row.
- **Web sessions.** A session whose job is running is not idle; a browser that logs in again keeps
  one place of the four; stopping the process waits five seconds for requests in flight.
- **URLs.** Pages that belong to one app or one offer sit under it (`/apps/<id>/hosting/check`,
  `/apps/get/<feed>/<package>/review`), sources' own pages under `/sources/of/<name>`, and the
  shared pages are `/confirm`, `/working`, `/document` and `/unlock`. `/system/restore` does not
  exist, as resolved.
- **Mockups** of the four screens were not shown to the owner before the build, since the owner
  asked for the work to proceed without questions; the look follows the touchscreen's tokens.

**Tests and packaging**

- **The end-to-end test** (`tests/web-gui-e2e.sh`) runs inside the dev VM like
  `tests/helper-e2e.sh`, by hand, and speaks to the web GUI on its Unix socket as Caddy does. It
  does not go through Caddy and mDNS; the generated route and firewall are covered by `nodo-web`'s
  tests.
- **Budgets.** The stripped `nodo-web-gui` is about 6.5 MiB on x86_64. No board's budget was
  raised; none was measured again with it.
