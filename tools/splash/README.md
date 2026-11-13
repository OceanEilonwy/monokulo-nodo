# The boot splash's logo

`logo.svg` is the Nodo mark from the branding canvas: the Ember band with the orange hex core
("Nodo: Ember band, hex core status", the needs-attention variant on dark). nodo-splash
(`crates/nodo-splash`) draws it on the built-in screen's text console in half blocks, from the
56 x 56 palette grid in `crates/nodo-splash/logo.grid`, which it embeds at build time.

`splash-grid` here makes that grid. After changing the SVG:

```sh
cargo run --release --manifest-path tools/splash/Cargo.toml -- \
    tools/splash/logo.svg crates/nodo-splash/logo.grid
```

Add `--check` to compare instead of writing. How it renders: the knot's outlines are drawn
thicker (stroke width 1.21 to 2.0), so they survive at 56 pixels; the logo is rendered at 448 x 448
over black; every pixel is matched to the nearest of the eight colours; and each 8 x 8 block
becomes one grid pixel: the outline when at least 30% of it is outline, black when at least half is
black, otherwise its most common orange.

This crate is its own workspace, so its SVG renderer never reaches the device's build. To see the
result in a desktop terminal with 24-bit colour, run `cargo run -p nodo-splash -- --preview`: the
logo at rest, then with the glint across its middle.
