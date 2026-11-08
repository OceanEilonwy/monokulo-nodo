//! The real settings policy: schemas and validators from package Lua, run by the unprivileged
//! worker; references and rendering by `nodo-settings` in this process, which runs no Lua.
//!
//! Each package's values live in a root-only store, kept apart from the generated file so a
//! secret never has to be read back out of a config format. The package's own file is either
//! its template filled in, such as cuprated's `Cuprated.toml`, or a flat table of values.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nodo_api::{
    ApiError, SettingChange, SettingCurrent, SettingField, SettingInput, SettingKind,
    SettingLiteral, SettingsForm,
};
use nodo_gen::service_user;
use nodo_manifest::is_valid_package_path;
use nodo_settings::{
    is_valid_setting_name, parse_template, toml_file, DefaultValue, Installed, InstalledPackage,
    Part, Resolved, Schema, SettingDef, SettingType, SettingValue, Value,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::catalog::Package;
use crate::settings::{PlannedFile, SettingsContext, SettingsPolicy};
use crate::worker::{WorkerCheck, WorkerPackage, WorkerRequest, WorkerResponse, MAX_WORKER_IO};

/// Settings code is small. A larger file is refused before it reaches the worker.
const MAX_SOURCE: usize = 256 * 1024;
const MAX_TEMPLATE: usize = 256 * 1024;
const FLAT_FILE: &str = "settings.toml";

/// A stored value. Literals are never scanned for references, which is what keeps a typed
/// `$(` from turning into one.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Stored {
    Literal(String),
    Template(String),
}

impl std::fmt::Debug for Stored {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stored::Literal(_) => f.write_str("Literal(..)"),
            Stored::Template(text) => write!(f, "Template({text:?})"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    values: BTreeMap<String, Stored>,
}

/// The settings policy used on the device.
#[derive(Debug, Clone, Copy, Default)]
pub struct NodoSettings;

fn failed(message: impl Into<String>) -> ApiError {
    ApiError::Failed(message.into())
}

fn store_path(ctx: &SettingsContext, id: &str) -> PathBuf {
    ctx.paths.settings_store.join(format!("{id}.toml"))
}

fn read_text(ctx: &SettingsContext, path: &Path, limit: usize) -> Result<String, ApiError> {
    let bytes = ctx
        .sys
        .read_file(path)?
        .ok_or_else(|| failed(format!("{} is missing", path.display())))?;
    if bytes.len() > limit {
        return Err(failed(format!("{} is too large", path.display())));
    }
    String::from_utf8(bytes).map_err(|_| failed(format!("{} is not UTF-8", path.display())))
}

fn package_file(
    ctx: &SettingsContext,
    package: &Package,
    relative: &str,
) -> Result<PathBuf, ApiError> {
    // System manifests skip validation, so the path is checked here for both kinds.
    if !is_valid_package_path(relative) {
        return Err(failed(format!("{}: invalid package path", package.id())));
    }
    Ok(package
        .package_dir(&ctx.paths.system_packages)?
        .join(relative))
}

fn load_store(ctx: &SettingsContext, id: &str) -> Result<Store, ApiError> {
    match ctx.sys.read_file(&store_path(ctx, id))? {
        None => Ok(Store::default()),
        Some(bytes) => {
            let bytes = Zeroizing::new(bytes);
            std::str::from_utf8(&bytes)
                .ok()
                .and_then(|text| toml::from_str(text).ok())
                .ok_or_else(|| failed(format!("the stored settings of {id} are corrupt")))
        }
    }
}

fn ask_worker(ctx: &SettingsContext, request: &WorkerRequest) -> Result<WorkerResponse, ApiError> {
    let input = Zeroizing::new(serde_json::to_vec(request).map_err(|e| failed(e.to_string()))?);
    if input.len() > MAX_WORKER_IO {
        return Err(failed("settings request too large"));
    }
    let output = Zeroizing::new(ctx.sys.settings_worker(&input)?);
    serde_json::from_slice(&output)
        .map_err(|_| failed("the settings worker gave a malformed answer"))
}

/// The literal text of a value, as `SettingType::parse_literal` reads it back.
fn value_text(value: &Value) -> Zeroizing<String> {
    Zeroizing::new(match value {
        Value::String(text) | Value::Enum(text) | Value::MoneroAddress(text) | Value::Url(text) => {
            text.clone()
        }
        Value::Int(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Secret(secret) => secret.expose().to_owned(),
    })
}

fn kind(ty: &SettingType) -> SettingKind {
    match ty {
        SettingType::String => SettingKind::Text,
        SettingType::Int { min, max } => SettingKind::Int {
            min: *min,
            max: *max,
        },
        SettingType::Bool => SettingKind::Bool,
        SettingType::Enum { choices } => SettingKind::Choice(choices.clone()),
        SettingType::MoneroAddress => SettingKind::MoneroAddress,
        SettingType::Url => SettingKind::Url,
        SettingType::Secret => SettingKind::Secret,
    }
}

fn literal_for(ty: &SettingType, value: &Value) -> Option<SettingLiteral> {
    match (ty, value) {
        (_, Value::Secret(_)) => None,
        (_, Value::Int(n)) => Some(SettingLiteral::Int(*n)),
        (_, Value::Bool(b)) => Some(SettingLiteral::Bool(*b)),
        (_, other) => Some(SettingLiteral::Text(value_text(other).to_string())),
    }
}

/// The single reference a stored template holds, which is the only form this API writes.
fn single_reference(text: &str) -> Option<(String, String)> {
    match parse_template(text).ok()?.as_slice() {
        [Part::Reference(reference)] => Some((reference.target.clone(), reference.setting.clone())),
        _ => None,
    }
}

fn to_setting_value(def: &SettingDef, stored: &Stored) -> Result<SettingValue, String> {
    match stored {
        Stored::Literal(text) => def.ty.parse_literal(text).map(SettingValue::Literal),
        Stored::Template(text) => Ok(SettingValue::Template(text.clone())),
    }
}

/// Every installed package with settings, loaded and checked in one worker run.
struct World {
    installed: Installed,
    schemas: BTreeMap<String, Result<Schema, String>>,
}

impl World {
    fn load(ctx: &SettingsContext) -> Result<World, ApiError> {
        let mut request = WorkerRequest::default();
        for package in ctx.packages {
            if let Some(relative) = &package.manifest.settings {
                let path = package_file(ctx, package, relative)?;
                let source = read_text(ctx, &path, MAX_SOURCE).unwrap_or_default();
                request.packages.push(WorkerPackage {
                    id: package.id().to_owned(),
                    source,
                });
            }
        }
        let response = ask_worker(ctx, &request)?;
        let mut installed = Installed::default();
        for package in ctx.packages {
            let schema = match response.schemas.get(package.id()) {
                Some(Ok(schema)) => schema.clone(),
                // Its own form reports the problem. Others see it as having no settings.
                Some(Err(_)) => continue,
                None => Schema::empty(),
            };
            let store = load_store(ctx, package.id())?;
            let values = store
                .values
                .iter()
                .filter_map(|(name, stored)| {
                    let def = schema.setting(name)?;
                    to_setting_value(def, stored)
                        .ok()
                        .map(|v| (name.clone(), v))
                })
                .collect();
            installed.packages.insert(
                package.id().to_owned(),
                InstalledPackage {
                    manifest: package.manifest.clone(),
                    schema,
                    values,
                },
            );
        }
        Ok(World {
            installed,
            schemas: response.schemas,
        })
    }

    fn schema(&self, id: &str) -> Result<&Schema, ApiError> {
        match self.schemas.get(id) {
            Some(Ok(schema)) => Ok(schema),
            Some(Err(problem)) => Err(failed(format!("{id}'s settings file failed: {problem}"))),
            None => Err(failed(format!("{id} has no settings file"))),
        }
    }
}

fn join_errors<E: std::fmt::Display>(errors: &[E]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

fn default_text(def: &SettingDef) -> Option<String> {
    if matches!(def.ty, SettingType::Secret) {
        return None;
    }
    def.default.as_ref().map(|default| match default {
        DefaultValue::Text(text) => text.clone(),
        DefaultValue::Int(n) => n.to_string(),
        DefaultValue::Bool(b) => b.to_string(),
    })
}

fn current(
    def: &SettingDef,
    stored: Option<&Stored>,
    resolved: Option<&Resolved>,
) -> SettingCurrent {
    let secret = matches!(def.ty, SettingType::Secret);
    let resolved_text = || {
        resolved
            .and_then(|r| r.values.get(&def.name))
            .filter(|_| !secret)
            .map(|value| value_text(value).to_string())
    };
    match stored {
        None if secret => SettingCurrent::SecretUnset,
        None => SettingCurrent::Default,
        Some(Stored::Literal(_)) if secret => SettingCurrent::SecretSet,
        Some(Stored::Literal(text)) => match def.ty.parse_literal(text) {
            Ok(value) => literal_for(&def.ty, &value)
                .map_or(SettingCurrent::SecretSet, SettingCurrent::Literal),
            Err(_) => SettingCurrent::Default,
        },
        Some(Stored::Template(text)) => match single_reference(text) {
            Some((target, setting)) => SettingCurrent::Reference {
                target,
                setting,
                resolved: resolved_text(),
            },
            None => SettingCurrent::Literal(SettingLiteral::Text(text.clone())),
        },
    }
}

/// Turns one typed change into what is stored. Every literal is parsed by its type here, so
/// nothing malformed is ever stored.
fn stored_for(def: &SettingDef, input: &SettingInput) -> Result<Option<Stored>, String> {
    let secret = matches!(def.ty, SettingType::Secret);
    match input {
        SettingInput::Reset => Ok(None),
        SettingInput::Secret(value) => {
            if !secret {
                return Err(format!("{} is not a secret", def.name));
            }
            def.ty
                .parse_literal(value.expose())
                .map_err(|m| format!("{} {m}", def.name))?;
            Ok(Some(Stored::Literal(value.expose().to_owned())))
        }
        SettingInput::Literal(_) if secret => Err(format!(
            "{} is a secret, so it must be sent as one",
            def.name
        )),
        SettingInput::Literal(literal) => {
            let text = match literal {
                SettingLiteral::Text(text) => text.clone(),
                SettingLiteral::Int(n) => n.to_string(),
                SettingLiteral::Bool(b) => b.to_string(),
            };
            def.ty
                .parse_literal(&text)
                .map_err(|m| format!("{} {m}", def.name))?;
            Ok(Some(Stored::Literal(text)))
        }
        SettingInput::Reference { target, setting } => {
            if !nodo_manifest::is_valid_name(target) || !is_valid_setting_name(setting) {
                return Err(format!("{}: invalid reference", def.name));
            }
            Ok(Some(Stored::Template(format!("$({target}.{setting})"))))
        }
    }
}

impl SettingsPolicy for NodoSettings {
    fn form(&self, ctx: &SettingsContext, package: &Package) -> Result<SettingsForm, ApiError> {
        let world = World::load(ctx)?;
        let schema = world.schema(package.id())?;
        let store = load_store(ctx, package.id())?;
        let resolved = world.installed.resolve(package.id()).ok();
        let fields = schema
            .settings
            .iter()
            .map(|def| SettingField {
                name: def.name.clone(),
                label: def.label.clone(),
                help: def.help.clone().unwrap_or_default(),
                required: def.required,
                kind: kind(&def.ty),
                current: current(def, store.values.get(&def.name), resolved.as_ref()),
                default: default_text(def),
                public_name: def.public.clone(),
            })
            .collect();
        Ok(SettingsForm {
            package: package.id().to_owned(),
            fields,
        })
    }

    fn plan(
        &self,
        ctx: &SettingsContext,
        package: &Package,
        changes: &[SettingChange],
    ) -> Result<Vec<PlannedFile>, ApiError> {
        let id = package.id();
        let mut world = World::load(ctx)?;
        let schema = world.schema(id)?.clone();
        let mut store = load_store(ctx, id)?;
        let mut seen = std::collections::BTreeSet::new();
        for change in changes {
            if !seen.insert(change.name.as_str()) {
                return Err(failed(format!("setting given twice: {}", change.name)));
            }
            let def = schema.setting(&change.name).ok_or_else(|| {
                failed(format!(
                    "{id} has no setting named {}",
                    change.name.escape_debug()
                ))
            })?;
            match stored_for(def, &change.value).map_err(failed)? {
                Some(stored) => store.values.insert(change.name.clone(), stored),
                None => store.values.remove(&change.name),
            };
        }
        // Stored values that no longer fit the schema, such as after an update, are dropped
        // rather than blocking every future change.
        store
            .values
            .retain(|name, _| schema.setting(name).is_some());

        let mut values = BTreeMap::new();
        for (name, stored) in &store.values {
            let def = schema.setting(name).expect("retained above");
            let value = to_setting_value(def, stored).map_err(|m| failed(format!("{name} {m}")))?;
            values.insert(name.clone(), value);
        }
        if let Some(entry) = world.installed.packages.get_mut(id) {
            entry.values = values;
        }
        let resolved = world
            .installed
            .resolve(id)
            .map_err(|errors| failed(join_errors(&errors)))?;

        // Validators see final values, so a reference is checked by what it resolves to.
        let checks: Vec<WorkerCheck> = resolved
            .values
            .iter()
            .filter(|(name, _)| schema.setting(name).is_some_and(|d| d.has_validator))
            .map(|(name, value)| WorkerCheck {
                package: id.to_owned(),
                setting: name.clone(),
                text: value_text(value).to_string(),
            })
            .collect();
        if !checks.is_empty() {
            let mut request = WorkerRequest {
                checks,
                ..WorkerRequest::default()
            };
            let relative = package.manifest.settings.as_deref().unwrap_or_default();
            request.packages.push(WorkerPackage {
                id: id.to_owned(),
                source: read_text(ctx, &package_file(ctx, package, relative)?, MAX_SOURCE)?,
            });
            let response = ask_worker(ctx, &request)?;
            if !response.rejections.is_empty() {
                return Err(failed(response.rejections.join("; ")));
            }
        }

        let layout = package.layout()?;
        let (name, rendered) = match &package.manifest.config {
            Some(config) => {
                let template = read_text(
                    ctx,
                    &package_file(ctx, package, &config.template)?,
                    MAX_TEMPLATE,
                )?;
                let rendered = world
                    .installed
                    .render_toml(id, &template)
                    .map_err(|errors| failed(join_errors(&errors)))?;
                (config.name.clone(), rendered.text)
            }
            None => (
                FLAT_FILE.to_owned(),
                toml_file(&resolved.values).map_err(|e| failed(e.to_string()))?,
            ),
        };
        let store_text = toml::to_string(&store).map_err(|e| failed(e.to_string()))?;
        let user = service_user(id).map_err(|e| failed(e.to_string()))?;
        Ok(vec![
            PlannedFile {
                path: store_path(ctx, id),
                bytes: Zeroizing::new(store_text.into_bytes()),
                mode: 0o600,
                group: None,
                parent_mode: 0o700,
            },
            PlannedFile {
                path: Path::new(&layout.config_dir).join(name),
                bytes: Zeroizing::new(rendered.into_bytes()),
                // Readable by the service through its group, and by nobody else, since it can
                // hold secrets.
                mode: 0o640,
                group: Some(user),
                parent_mode: 0o755,
            },
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{FakeSystem, TEST_PATHS};
    use crate::helper::{AllowedUids, Helper};
    use crate::NoCaddyYet;
    use nodo_api::{Request, Response, Secret};

    const CUPRATED: &str = r#"api_version = 1
id = "cuprated"
version = "0.1.0"
exec = ["/usr/bin/cuprated"]
provides = ["monero-daemon"]
settings = "settings.lua"

[config]
template = "Cuprated.toml.in"
name = "Cuprated.toml"

[health.tcp]
port = 18081
"#;

    const LUA: &str = r#"return { api_version = 1, settings = {
        { name = "outbound_peers", type = "int", label = "Outbound peers", default = 32, min = 1, max = 128 },
        { name = "inbound", type = "bool", label = "Accept inbound peers", default = true },
        { name = "log_level", type = "enum", label = "Log level", default = "info",
          choices = { "error", "warn", "info", "debug" } },
        { name = "rpc_port", type = "int", label = "RPC port", default = 18081, public = "rpc-port" },
        { name = "note", type = "string", label = "Note",
          validate = function(v) return v ~= "bad", "must not be bad" end },
        { name = "token", type = "secret", label = "Token" },
    } }"#;

    const TEMPLATE: &str = r#"network = "Mainnet"

[tracing.file]
level = "$(cuprated.log_level)"

[p2p.clear_net]
enable_inbound = "$(cuprated.inbound)"
outbound_connections = "$(cuprated.outbound_peers)"
listen_on = "0.0.0.0"
"#;

    const SHOP: &str = r#"api_version = 1
id = "shop"
version = "1"
exec = ["bin/shop"]
settings = "settings.lua"
"#;

    const SHOP_LUA: &str = r#"return { api_version = 1, settings = {
        { name = "daemon_port", type = "int", label = "Daemon port", default = "$(monero-daemon.rpc-port)" },
        { name = "label", type = "string", label = "Label" },
    } }"#;

    const OUT: &str = "/etc/cuprated/Cuprated.toml";

    fn setup() -> Helper<FakeSystem> {
        let sys = FakeSystem::default();
        sys.put("/m/sys/cuprated.toml", CUPRATED.as_bytes());
        sys.put("/p/sys/cuprated/settings.lua", LUA.as_bytes());
        sys.put("/p/sys/cuprated/Cuprated.toml.in", TEMPLATE.as_bytes());
        sys.add_manifest(SHOP);
        sys.put("/opt/nodo/apps/shop/settings.lua", SHOP_LUA.as_bytes());
        Helper::new(
            sys,
            AllowedUids::new([1000]),
            Box::new(NodoSettings),
            Box::new(NoCaddyYet),
        )
        .with_paths(TEST_PATHS())
    }

    fn change(name: &str, value: SettingInput) -> SettingChange {
        SettingChange {
            name: name.into(),
            value,
        }
    }

    fn int(n: i64) -> SettingInput {
        SettingInput::Literal(SettingLiteral::Int(n))
    }

    fn text(t: &str) -> SettingInput {
        SettingInput::Literal(SettingLiteral::Text(t.into()))
    }

    fn apply(
        h: &Helper<FakeSystem>,
        package: &str,
        changes: Vec<SettingChange>,
    ) -> Result<Response, ApiError> {
        h.handle(
            1000,
            Request::ApplySettings {
                package: package.into(),
                changes,
            },
        )
    }

    fn form(h: &Helper<FakeSystem>, package: &str) -> SettingsForm {
        match h.handle(
            1000,
            Request::SettingsForm {
                package: package.into(),
            },
        ) {
            Ok(Response::SettingsForm(form)) => form,
            other => panic!("{other:?}"),
        }
    }

    fn output(h: &Helper<FakeSystem>) -> toml::Table {
        toml::from_str(std::str::from_utf8(&h.sys.get(OUT).unwrap()).unwrap()).unwrap()
    }

    #[test]
    fn cuprated_gets_its_real_config_file_with_typed_values() {
        let h = setup();
        apply(
            &h,
            "cuprated",
            vec![
                change("outbound_peers", int(64)),
                change("log_level", text("debug")),
            ],
        )
        .unwrap();
        let out = output(&h);
        assert_eq!(out["network"].as_str(), Some("Mainnet"));
        assert_eq!(
            out["p2p"]["clear_net"]["outbound_connections"].as_integer(),
            Some(64)
        );
        assert_eq!(
            out["p2p"]["clear_net"]["enable_inbound"].as_bool(),
            Some(true)
        );
        assert_eq!(out["tracing"]["file"]["level"].as_str(), Some("debug"));
        assert!(h
            .sys
            .calls()
            .contains(&format!("write {OUT} mode=640 group=_cuprated")));
        assert!(h
            .sys
            .calls()
            .contains(&"sv force-restart cuprated -w120".to_owned()));
    }

    #[test]
    fn values_are_checked_by_type_before_anything_is_written() {
        let h = setup();
        for changes in [
            vec![change("outbound_peers", int(500))],
            vec![change("outbound_peers", text("many"))],
            vec![change("log_level", text("loud"))],
            vec![change("ghost", int(1))],
            vec![change("token", text("plain"))],
            vec![change("note", SettingInput::Secret(Secret::new("x")))],
            vec![change("note", text("bad"))],
            vec![
                change("outbound_peers", int(5)),
                change("outbound_peers", int(6)),
            ],
        ] {
            assert!(
                matches!(
                    apply(&h, "cuprated", changes.clone()),
                    Err(ApiError::Failed(_))
                ),
                "{changes:?}"
            );
        }
        assert!(
            !h.sys.calls().iter().any(|c| c.starts_with("write")),
            "{:?}",
            h.sys.calls()
        );
    }

    #[test]
    fn a_typed_reference_lookalike_stays_literal_text() {
        let h = setup();
        apply(
            &h,
            "shop",
            vec![change("label", text("$(cuprated.rpc_port)"))],
        )
        .unwrap();
        let out: toml::Table = toml::from_str(
            std::str::from_utf8(&h.sys.get("/etc/apps/shop/settings.toml").unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(out["label"].as_str(), Some("$(cuprated.rpc_port)"));
        let field = form(&h, "shop")
            .fields
            .into_iter()
            .find(|f| f.name == "label")
            .unwrap();
        assert_eq!(
            field.current,
            SettingCurrent::Literal(SettingLiteral::Text("$(cuprated.rpc_port)".into()))
        );
    }

    #[test]
    fn references_resolve_across_packages_and_follow_the_provider() {
        let h = setup();
        apply(&h, "shop", vec![change("label", text("x"))]).unwrap();
        let shop = |h: &Helper<FakeSystem>| -> toml::Table {
            toml::from_str(
                std::str::from_utf8(&h.sys.get("/etc/apps/shop/settings.toml").unwrap()).unwrap(),
            )
            .unwrap()
        };
        assert_eq!(shop(&h)["daemon_port"].as_integer(), Some(18081));
        let field = form(&h, "shop")
            .fields
            .into_iter()
            .find(|f| f.name == "daemon_port")
            .unwrap();
        assert_eq!(field.current, SettingCurrent::Default);
        assert_eq!(field.default.as_deref(), Some("$(monero-daemon.rpc-port)"));

        let reference = SettingInput::Reference {
            target: "cuprated".into(),
            setting: "rpc-port".into(),
        };
        apply(&h, "shop", vec![change("daemon_port", reference)]).unwrap();
        let field = form(&h, "shop")
            .fields
            .into_iter()
            .find(|f| f.name == "daemon_port")
            .unwrap();
        assert_eq!(
            field.current,
            SettingCurrent::Reference {
                target: "cuprated".into(),
                setting: "rpc-port".into(),
                resolved: Some("18081".into()),
            }
        );
    }

    #[test]
    fn unresolvable_reference_is_refused() {
        let h = setup();
        let reference = SettingInput::Reference {
            target: "cuprated".into(),
            setting: "token".into(),
        };
        let Err(ApiError::Failed(message)) =
            apply(&h, "shop", vec![change("daemon_port", reference)])
        else {
            panic!()
        };
        assert!(
            message.contains("approval") || message.contains("no setting"),
            "{message}"
        );
    }

    #[test]
    fn secrets_are_stored_root_only_and_reported_only_as_set() {
        let h = setup();
        let field = |h: &Helper<FakeSystem>| {
            form(h, "cuprated")
                .fields
                .into_iter()
                .find(|f| f.name == "token")
                .unwrap()
        };
        assert_eq!(field(&h).current, SettingCurrent::SecretUnset);
        apply(
            &h,
            "cuprated",
            vec![change(
                "token",
                SettingInput::Secret(Secret::new("s3cret-value")),
            )],
        )
        .unwrap();
        assert_eq!(field(&h).current, SettingCurrent::SecretSet);
        assert!(h
            .sys
            .calls()
            .contains(&"write /s/settings/cuprated.toml mode=600 group=-".to_owned()));
        let shown = format!("{:?}", form(&h, "cuprated"));
        assert!(!shown.contains("s3cret"), "{shown}");
        apply(&h, "cuprated", vec![change("token", SettingInput::Reset)]).unwrap();
        assert_eq!(field(&h).current, SettingCurrent::SecretUnset);
    }

    #[test]
    fn form_describes_every_setting_for_the_tui() {
        let h = setup();
        let form = form(&h, "cuprated");
        let names: Vec<&str> = form.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "outbound_peers",
                "inbound",
                "log_level",
                "rpc_port",
                "note",
                "token"
            ]
        );
        let level = &form.fields[2];
        assert_eq!(
            level.kind,
            SettingKind::Choice(vec![
                "error".into(),
                "warn".into(),
                "info".into(),
                "debug".into()
            ])
        );
        assert_eq!(level.default.as_deref(), Some("info"));
        assert_eq!(form.fields[3].public_name.as_deref(), Some("rpc-port"));
        assert_eq!(form.fields[5].default, None);
    }

    #[test]
    fn lua_runs_only_through_the_worker() {
        let h = setup();
        form(&h, "cuprated");
        apply(&h, "cuprated", vec![change("note", text("fine"))]).unwrap();
        let workers = h
            .sys
            .calls()
            .iter()
            .filter(|c| *c == "settings-worker")
            .count();
        assert_eq!(
            workers, 3,
            "one load for the form, one load and one validation for the change"
        );
    }

    #[test]
    fn broken_settings_code_is_reported() {
        let h = setup();
        h.sys
            .put("/p/sys/cuprated/settings.lua", b"while true do end");
        let result = h.handle(
            1000,
            Request::SettingsForm {
                package: "cuprated".into(),
            },
        );
        assert!(matches!(result, Err(ApiError::Failed(_))), "{result:?}");
    }

    #[test]
    fn hostile_template_path_is_refused() {
        let h = setup();
        h.sys.put(
            "/m/sys/cuprated.toml",
            CUPRATED
                .replace("Cuprated.toml.in", "../../etc/shadow")
                .as_bytes(),
        );
        assert!(apply(&h, "cuprated", vec![change("outbound_peers", int(8))]).is_err());
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("write")));
    }
}
