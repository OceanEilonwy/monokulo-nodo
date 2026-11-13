# The screen: package panels

The NanoPC-T6N's built-in 1920x1080 touchscreen shows panels that packages offer: a node's sync
progress, a shop's payments, the device's temperature. The owner chooses which panels show, where,
and how big. The look is design A's tile grid made OLED dark (`docs/design/tui/ScreenTilesOled*.dc.html`):
thin rounded outlines on pure black, so that unlit OLED pixels stay off, with each tile's name in
small capitals, its value large and a status dot. The top bar has two buttons, ☰ Arrange and
⏻ Shut down.

This document is the contract between packages and the screen. Packages describe their panels in
their manifest and compute the values in a small sandboxed Lua function; the screen fetches the data,
runs the function, checks what it returns, and draws every pixel itself.

## Parts

| Part | Where | Runs as |
|---|---|---|
| Manifest `[[panels]]` | `nodo-manifest` | parsed by the helper and the screen |
| Fetching, Lua, output checks, history, layout | `nodo-panels` | inside `nodo-screen` |
| The screen program | `nodo-screen` (in `nodo-tools`) | `_nodo_screen`, never root |
| Service | `/etc/sv/nodo-screen` on tty1 | started on boards with a screen |
| Shutting down | `nodo-power` (in `nodo-tools`) | root, asked only by `_nodo_screen` |

`nodo-screen` is a terminal program. On the T6N the kernel's console on the panel uses Terminus
16x32 and is turned to the case (`docs/MAINLINE_KERNEL.md`), so tty1 is 120 columns by 33 rows. The
service loads the screen's own console font, `nodo-screen-16x32`, and the program sets the
console's colours to the TUI's. The same program runs in any terminal, which is how it is tested.

### The console font

Terminus's 512-glyph console font (`ter-v32n`) has straight box lines and full blocks but none of the
rounded or dashed corners, heavy lines, half and eighth blocks, dot or icons the screen draws.
`nodo-screen-font` (`crates/nodo-screen-font`) builds `nodo-screen-16x32` when the package is
built: it draws those 28 glyphs to match Terminus's line positions and puts them in place of Greek
and Cyrillic glyphs the screen never shows. Terminus is under the SIL Open Font License, which
requires a modified font to take another name; the licence ships as
`/usr/share/licenses/nodo-tools/OFL-nodo-screen-16x32.txt`. A test in `nodo-screen` fails if the
program draws a glyph that neither Terminus nor the script provides.

## A package declares panels

```toml
[[panels]]
id = "sync"                         # unique within the package
title = "Monero node"               # shown in the border, at most 24 characters
script = "panels/sync.lua"          # relative to the package directory
refresh_secs = 10                   # 2 to 3600
min_size = [2, 1]                   # [columns, rows] on the 8 x 4 grid
max_size = [4, 2]
default_size = [4, 2]               # optional, min_size when absent

[panels.source.jsonrpc]             # or [panels.source.http]
url = "http://127.0.0.1:18081/json_rpc"
method = "get_info"
```

A package may declare up to 8 panels.

### Sources

The screen fetches the data; the Lua function never does. A source is one of:

- `http = { url = "http://127.0.0.1:<port>/<path>" }`: a GET whose response is JSON.
- `jsonrpc = { url = "...", method = "<name>" }`: a JSON-RPC 2.0 POST without parameters; the
  function receives the `result`.

Rules, all checked when the manifest is validated:

- The URL is plain `http://` on a loopback address (`127.0.0.1`, `localhost` or `[::1]`) with an
  explicit port, the same rule as health checks.
- The port must be one the package itself declares: an `network.inbound` port, the port of a `web`
  upstream, or its health check's. A panel cannot read another package's service.
- The method name is 1 to 64 letters, digits, `_` or `.`.

Fetches have a 2 second timeout and a 256 KiB response limit, follow no redirects, and send no
cookies or credentials. A response that is not JSON, or whose JSON nests deeper than 32 levels,
fails the fetch.

The base system's own panels (the device's temperature, storage and memory) use a third source,
`device`, which only the screen provides: it reads `/sys` and `/proc` and gives the function
`{ temp_c, ssd_temp_c, ssd_used, ssd_total, mem_used, mem_total, uptime_secs }`, where `temp_c`
is the hottest of the SoC's sensors and `ssd_temp_c` the NVMe drive's own (docs/THERMAL.md).
Each is left out when it cannot be read. Feed packages cannot use it.

## The function

The script is a Lua 5.4 file that returns a table with a `panel` function:

```lua
return {
  panel = function(data, history)
    local pct = math.floor(data.height / data.target_height * 100)
    return {
      big = pct .. "%", unit = "synced",
      bar = pct / 100,
      rows = { { "height", data.height .. " / " .. data.target_height },
               { "peers",  data.outgoing_connections_count .. " out" } },
      sample = pct,
      series = history,
      status = data.synchronized and "ok" or "busy",
    }
  end,
}
```

- `data` is the source's JSON as Lua values: objects become tables, arrays become sequences, `null`
  becomes `nil`, numbers stay numbers.
- `history` is the panel's earlier `sample` values, oldest first, at most 120.
- It runs in the same sandbox as package settings (`nodo-settings`): only `assert`, `error`,
  `ipairs`, `next`, `pairs`, `select`, `tonumber`, `tostring`, `type`, and parts of `string`,
  `table` and `math`; no `io`, `os`, `require`, `load` or networking; text chunks only, never
  bytecode; 4 MiB of memory, 1,000,000 instructions and 200 ms of wall time per run; never as root.
- Every run gets a new Lua state, so nothing a function stores survives to its next run. A file
  that fails to load, or a `panel` that errors or exceeds a limit, shows the panel as failed with a
  one-line reason; the rest of the screen is unaffected.

### What it returns

Every field is optional. Anything not listed here, a wrong type or an out of range value fails
the run, so a mistake is visible instead of drawn wrongly.

| Field | Type | Shown as |
|---|---|---|
| `big` | text, up to 16 characters | the panel's large value |
| `unit` | text, up to 16 | small text after `big` |
| `bar` | number 0 to 1 | a progress bar of block characters |
| `rows` | up to 6 pairs `{ label, value }`, 16 and 40 characters | label on the left, value on the right |
| `series` | up to 120 numbers | a sparkline of block characters, scaled to its own range |
| `sample` | number | appended to `history` for the next run |
| `status` | `"ok"`, `"busy"`, `"warn"`, `"error"` or `"idle"` | the colour of `big` and the border |
| `note` | text, up to 60 | one dim line at the bottom |

Text is one line of printable characters; control characters and escape sequences are refused.
Numbers must be finite.

A panel draws what fits its size: at 2 x 1, `big`, `unit` and `note`; at 2 x 2, also `bar` and
`rows`; from 4 x 1, also `series`. So one function serves every size.

## Freshness

- Until the first successful run the panel says `waiting for <package>`.
- When a fetch fails, the panel keeps its last output, dimmed, with `stale · <age>` in place of the
  note, and the screen retries at the refresh interval.
- A package that is stopped, being updated or removed has its panels dimmed or, once removed,
  taken off the screen. Its place on the layout is kept, so a reinstall puts the panel back.

History lives only in memory: a restart of the screen begins it again.

## The layout

The screen is an 8 x 4 grid. The layout is stored by the screen in
`/var/lib/nodo-screen/layout.json`:

```json
{ "version": 1, "panels": [
  { "package": "cuprated", "panel": "sync", "x": 0, "y": 0, "w": 4, "h": 2 },
  { "package": "nodo", "panel": "device", "x": 4, "y": 2, "w": 2, "h": 2 }
] }
```

Panels never overlap and stay inside the grid; sizes stay within each panel's `min_size` and
`max_size`. A layout file that breaks these rules, or names a panel that no longer exists, is
repaired on load: overlapping or invalid entries are dropped and logged. When there is no layout,
the screen places every available panel in order, each at its default size, wherever it first fits.

### Arranging

Tap ☰ Arrange in the top bar, hold anywhere for one second, or press `e`. The top bar then shows
Hide, + Add panel (with how many panels are hidden) and a blue Save; free spaces are grey dashed
boxes, and only the selected tile is orange.

| Touch or mouse | Keys | Action |
|---|---|---|
| tap a tile | `Tab`, arrows | select it |
| drag a tile | `h` `j` `k` `l` | move it; it snaps to the grid, and a dashed orange outline shows where it lands |
| drag the ⇲ grip in the selected tile's corner | `H` `J` `K` `L` | resize it within its `min_size` and `max_size` |
| Hide | `x` | hide the selected tile; Undo takes Hide's place for 8 seconds (`u`) |
| tap a free space, then + Add panel | `a` | the list of hidden panels by package, three rows each with a picture of its size; + Add puts it over the tapped space, or in the first free one |
| Save | `Enter`, `Esc`, `s` | save and return |

A drop that would overlap another tile or leave the grid is refused: the tile goes back and the top
bar says why. Every touch target is at least 6 columns by 3 rows, about 96 pixels square.

The layout is saved atomically (written, synced, then renamed over the old file). The copy on tty1
reads it again within a second when it changes, unless someone is arranging on it, so a second copy
started over SSH can arrange too:

```sh
chpst -u _nodo_screen:_nodo_screen nodo-screen --no-touch
```

Over SSH the mouse works as touch does (click, drag), in any terminal that reports the mouse. Run it
as `_nodo_screen` so that the layout file stays owned by the screen's account.

## Shutting down

Unplugging a node while it writes its database can corrupt it, as happened to cuprated's during its
first sync. ⏻ Shut down (or `p`) asks "Shut down the Nodo?" full screen, with large Cancel and
Shut down buttons, so that no single tap turns the device off. Then the screen shows each service
stopping, saving files to disk and turning off, and finally "Safe to unplug", which stays on the
panel after the device is off.

`nodo-screen` never runs as root, so it asks `nodo-power`, a root runit service that can do exactly
one thing:

- It listens on `/run/nodo-power/socket`, in a directory that is root:_nodo_screen 0750; the socket
  is root:_nodo_screen 0660, and the service also checks the caller's uid (root or `_nodo_screen`).
- The only request is the line `poweroff`. Anything else is answered with an error and ignored:
  there are no arguments, service names or other commands.
- It stops every enabled service at once except itself and the screen, each with `sv -w <seconds>
  stop`: 300 seconds for cuprated (it is never killed while it saves), 90 for the rest. A service
  that has not stopped by then is left to runit's own shutdown. Then it syncs, creates
  `/run/nodo-power/off`, and runs `poweroff`.
- It writes one line per step back: `services N`, `stopping NAME`, `stopped NAME MS`,
  `timeout NAME MS`, `syncing`, `synced MS`, `off`, or `error TEXT`. The protocol and the sequence
  are in `crates/nodo-power`, with tests.

The screen's finish script clears tty1 when the program stops, except when `/run/nodo-power/off`
exists, so that "Safe to unplug" stays. If `nodo-power` cannot be reached, the screen says so and
offers a way back to the panels.

## The screen service

`nodo-screen` runs as `_nodo_screen`, in the `input` group (to read the touchscreen) and nowhere
else. The service's run script, as root, opens tty1, loads the console font (`setfont`), lowers the
kernel's console log level so that kernel messages do not draw over the panels, and then executes
the program with `chpst`. It reads manifests from `/usr/share/nodo/manifests` and
`/var/lib/nodo/manifests`, scripts from the package directories, and writes only its layout. It
reaches the network only to fetch the declared loopback sources, and talks to one local socket,
`nodo-power`'s, only after the owner confirms a shutdown.

The touchscreen is the first input device whose name starts with `Goodix`. Its coordinates are
turned to match the console with `/etc/nodo/screen.toml` (which also sets the backlight, below):

```toml
[touch]
swap_xy = true
invert_x = true
invert_y = false
```

## Backlight

After two minutes with no touch, click or key, the screen dims its backlight (the first device under
`/sys/class/backlight`, `fde30000.dsi.0` on the T6N, maximum 255) to 20% of its maximum. Any input
brings it back to full, and that input only wakes the screen: a tap on a dim screen presses nothing,
and the rest of that touch is ignored until the finger lifts. The screen never dims while someone
arranges, adds a panel, or during the shutdown screens, and it sets full brightness when it stops.

```toml
[backlight]
dim_after_secs = 120   # 0 never dims
dim_percent = 20       # never quite off: 0 means the lowest step, 1 of 255
```

The program still runs without root: the service's run script, as root, gives `_nodo_screen`'s
group write access to that one `brightness` file (`chgrp` and `chmod g+w`) before it starts the
program. A copy over SSH (`--no-touch`) leaves the backlight alone.

## What the owner approves

A package's panels are listed in its install review ("shows 2 panels on the screen, reading its own
service"). They are not a separate permission: a panel can only read what the package's own
service already serves on loopback, and it can only produce the plain fields above.

## Not in this version

- Panels that read another package's service, which would need a grant like `$(package.setting)`.
- Credentials for a source. Monokulo's order totals need a store key (`sk_...`), so its payments
  panel waits for an unauthenticated summary endpoint or for a way to grant a panel a secret.
- History that survives a restart.
- Tapping a panel to open its package in the TUI.
