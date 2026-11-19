//! The Monokulo Nodo admin pages for a browser on the home network (docs/ARCHITECTURE.md,
//! "Web GUI"): the screens of the terminal interface, as HTML.
//!
//! Caddy terminates TLS at `https://nodo.local` and passes requests to this process on a Unix
//! socket, so what arrives here is plain HTTP that is treated as secure. Nothing is kept on disk
//! and nothing is fetched from the network: sessions live in memory, htmx and the stylesheet are
//! compiled in.
//!
//! How a request becomes a page, for whoever adds one:
//!
//! - [`ctx::Ctx`] is the extractor every page behind the login takes. It finds the session and
//!   offers [`ctx::Ctx::with`], which runs a closure on the blocking pool with the session's
//!   backend, the device's `Snapshot` and the step runner ([`ctx::Work`]).
//! - The closure calls a flow of `nodo_ui_core::flows`, follows the `Step` with
//!   [`ctx::Work::run`], and returns an [`ctx::Out`]: a page to draw, or a [`runner::Reply`] that
//!   `Ctx` turns into a redirect, a flashed notice or a confirmation dialog.
//! - [`dest::url`] is the one place a `Dest` of the core becomes a URL.
//! - A call that can take minutes goes through [`ctx::Ctx::job`]; the browser then polls
//!   `/working` until it is over.
//! - [`layout`] holds the page frame and the pieces pages are made of.
//!
//! No page holds a rule of its own. What an action does, what it asks first and how its result
//! is worded come from the core; what is missing there is in [`core_wishlist`], marked as such.

// A dropped result is how failures went unreported. Where dropping is right, an #[allow] says why.
// CI also enforces these for the tests (.github/workflows/crates.yml).
#![deny(clippy::let_underscore_must_use, unused_must_use)]

pub mod assets;
pub mod backends;
pub mod clock;
pub mod core_wishlist;
pub mod ctx;
pub mod dest;
pub mod hints;
pub mod layout;
pub mod pages;
pub mod router;
pub mod runner;
pub mod security;
pub mod session;
pub mod state;

pub use router::router;
pub use state::{AppState, Config};
