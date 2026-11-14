//! Package settings: how packages declare them, and how values flow between packages.
//!
//! A package describes its settings in a Lua file. [`load_settings`] runs that file in a sandbox
//! and produces a plain [`Schema`], which is data and can cross into privileged code safely. Values
//! may reference settings of other packages as `$(package.setting)`, and [`Installed::resolve`]
//! turns those into typed values along with the dependencies and network access they imply.
//!
//! # The settings file
//!
//! ```lua
//! return {
//!   api_version = 1,
//!   settings = {
//!     { name = "rpc_port", type = "int", label = "RPC port", default = 18081,
//!       min = 1024, max = 65535, public = "rpc-port", endpoint = "tcp" },
//!     { name = "payout", type = "monero-address", label = "Payout address", required = true },
//!     { name = "daemon", type = "url", label = "Daemon", default = "$(monero-daemon.rpc-url)" },
//!     { name = "api_key", type = "secret", label = "API key",
//!       validate = function(v) return #v >= 16, "must be at least 16 characters" end },
//!     { name = "db_key", type = "secret", label = "Database key", required = true,
//!       generate = { hex = 32 } },
//!   },
//! }
//! ```
//!
//! `generate` asks the device to create a secret's value from fresh random bytes when the package
//! is installed and no value is set. It is created once and never regenerated.
//!
//! `retired = { "old_name", ... }` lists settings an earlier version declared and this one removed
//! on purpose. A value a device stored for a retired setting is ignored by resolution and dropped
//! by the helper, with a log line, the next time it writes the package's settings. A stored value
//! for a setting that is neither declared nor retired is still refused, so an update that removes
//! a setting by accident fails its install check and rolls back instead of losing the choice.
//! A retired name must not also be declared.
//!
//! Lua's own pattern functions are not available because their backtracking cannot be interrupted
//! by the instruction limit. Validators use `nodo.matches(text, regex)`, which runs in linear time.
//!
//! # Reference syntax
//!
//! `$(name.setting)` names a package id or a role. `$$(` is a literal `$(`, and each further `$`
//! before the `(` is kept, so `$$$(` is a literal `$$(`. [`escape_literal`] applies this form.
//! Resolved values are never rescanned, so a value containing `$(` cannot trigger an expansion.

mod escape;
mod monero;
mod resolve;
mod sandbox;
mod schema;
mod template;
mod url;

pub use escape::{env_file, env_name, env_pairs, toml_file, toml_value, EnvError};
pub use monero::{check_monero_address, MoneroNetwork};
pub use resolve::{
    Access, CatalogAccess, CatalogEntry, Dependency, Grant, Installed, InstalledPackage, NetworkGrant, PortProtocol,
    Preview, ReferenceCheck, ReferenceState, Rendered, ResolveError, Resolved,
};
pub use sandbox::{load_settings, Limit, Limits, LoadedSettings, Sandbox, SettingsError};
/// The Lua binding the sandbox uses, so that code built on [`Sandbox`] shares its version.
pub use mlua;
pub use schema::{
    is_valid_setting_name, DefaultValue, Endpoint, Generate, Schema, SecretString, SettingDef, SettingType,
    SettingValue, Value, MAX_GENERATED_BYTES, MAX_TEXT_LEN, MIN_GENERATED_BYTES, SETTINGS_API_VERSION,
};
pub use template::{escape_literal, parse_template, Part, Reference, TemplateError};
pub use url::{check_url, ParsedUrl};
