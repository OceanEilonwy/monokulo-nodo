//! The real settings policy: schemas and validators from package Lua, run by the unprivileged
//! worker; references and rendering by `nodo-settings` in this process, which runs no Lua.
//!
//! Each package's values live in a root-only store, kept apart from the generated file so a
//! secret never has to be read back out of a config format. Secrets in the store are sealed
//! under the device key (see [`crate::secrets`]). The package's own file is either its template
//! filled in, such as cuprated's `Cuprated.toml`, or a flat table of values. Resolved values
//! also go to a root-only environment file, which the run script hands to the service.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use nodo_api::{
    ApiError, ImpliedAccess, ReferenceAccess, ReferenceCheck, ReferenceOption, ReferenceStatus,
    SettingChange, SettingCurrent, SettingField, SettingInput, SettingKind, SettingLiteral,
    SettingPreview, SettingsForm, TemplatePart,
};
use nodo_gen::{service_user, Profile, ResolvedGrants};
use nodo_manifest::is_valid_package_path;
use nodo_settings::{
    env_file, escape_literal, is_valid_setting_name, parse_template, toml_file, Access,
    CatalogAccess, DefaultValue, Grant, ReferenceState,
    Installed, InstalledPackage, NetworkGrant, Part, PortProtocol, ResolveError, Resolved, Schema,
    SettingDef,
    SettingType, SettingValue, Value,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::catalog::Package;
use crate::secrets::{self, SecretsKey};
use crate::settings::{Plan, PlannedFile, ServiceInputs, SettingsContext, SettingsPolicy, Target};
use crate::system::WriteOptions;
use crate::worker::{WorkerCheck, WorkerPackage, WorkerRequest, WorkerResponse, MAX_WORKER_IO};

/// Settings code is small. A larger file is refused before it reaches the worker.
const MAX_SOURCE: usize = 256 * 1024;
const MAX_TEMPLATE: usize = 256 * 1024;
const FLAT_FILE: &str = "settings.toml";
const NONCE_LEN: usize = 24;

/// A stored value. Literals are never scanned for references, which is what keeps a typed
/// `$(` from turning into one.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Stored {
    Literal(String),
    Template(String),
    /// A secret sealed under the device key.
    Sealed(String),
}

impl std::fmt::Debug for Stored {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Stored::Literal(_) => f.write_str("Literal(..)"),
            Stored::Template(text) => write!(f, "Template({text:?})"),
            Stored::Sealed(_) => f.write_str("Sealed(..)"),
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

/// Why a secret cannot be read or written right now.
fn no_key(ctx: &SettingsContext, id: &str) -> ApiError {
    if ctx.set_up {
        ApiError::Locked(format!(
            "the secrets of {id} are sealed until the device is unlocked"
        ))
    } else {
        ApiError::Locked(
            "secrets are sealed under the device passphrase, so set it up first".to_owned(),
        )
    }
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

fn store_file(ctx: &SettingsContext, id: &str, store: &Store) -> Result<PlannedFile, ApiError> {
    let text = Zeroizing::new(toml::to_string(store).map_err(|e| failed(e.to_string()))?);
    Ok(PlannedFile {
        path: store_path(ctx, id),
        bytes: Zeroizing::new(text.as_bytes().to_vec()),
        mode: 0o600,
        group: None,
        parent_mode: 0o700,
    })
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

fn is_secret(def: &SettingDef) -> bool {
    matches!(def.ty, SettingType::Secret)
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

/// A stored template as pieces, for clients that edit text and references separately.
fn template_parts(text: &str) -> Option<Vec<TemplatePart>> {
    let parts = parse_template(text).ok()?;
    Some(
        parts
            .into_iter()
            .map(|part| match part {
                Part::Literal(text) => TemplatePart::Text(text),
                Part::Reference(r) => TemplatePart::Reference {
                    target: r.target,
                    setting: r.setting,
                },
            })
            .collect(),
    )
}

/// The stored form of pieces: text escaped so a literal `$(` stays text, references written as
/// `$(target.setting)`. Nothing the user typed can become a reference.
fn template_text(def: &SettingDef, parts: &[TemplatePart]) -> Result<String, String> {
    let mut text = String::new();
    for part in parts {
        match part {
            TemplatePart::Text(literal) => text.push_str(&escape_literal(literal)),
            TemplatePart::Reference { target, setting } => {
                if !nodo_manifest::is_valid_name(target) || !is_valid_setting_name(setting) {
                    return Err(format!("{}: invalid reference", def.name));
                }
                text.push_str(&format!("$({target}.{setting})"));
            }
        }
    }
    if parse_template(&text).is_err() {
        return Err(format!("{}: invalid value", def.name));
    }
    Ok(text)
}

/// The value a stored entry stands for. `None` when it is sealed and the device is locked.
fn stored_value(
    key: Option<&SecretsKey>,
    id: &str,
    def: &SettingDef,
    stored: &Stored,
) -> Result<Option<SettingValue>, String> {
    match stored {
        Stored::Literal(text) => def
            .ty
            .parse_literal(text)
            .map(|v| Some(SettingValue::Literal(v))),
        Stored::Template(text) => Ok(Some(SettingValue::Template(text.clone()))),
        Stored::Sealed(sealed) => {
            let Some(key) = key else { return Ok(None) };
            let plain = secrets::open(key, id, &def.name, sealed)?;
            def.ty
                .parse_literal(&plain)
                .map(|v| Some(SettingValue::Literal(v)))
        }
    }
}

fn seal(
    ctx: &SettingsContext,
    key: &SecretsKey,
    id: &str,
    name: &str,
    value: &str,
) -> Result<Stored, ApiError> {
    let mut nonce = [0u8; NONCE_LEN];
    ctx.sys.random(&mut nonce)?;
    secrets::seal(key, id, name, value, &nonce)
        .map(Stored::Sealed)
        .map_err(failed)
}

/// Seals every secret of one store still held in clear. Returns how many.
fn seal_clear_secrets(
    ctx: &SettingsContext,
    key: &SecretsKey,
    id: &str,
    schema: &Schema,
    store: &mut Store,
) -> Result<usize, ApiError> {
    let mut sealed = 0;
    for (name, stored) in store.values.iter_mut() {
        let Some(def) = schema.setting(name) else {
            continue;
        };
        if let (true, Stored::Literal(text)) = (is_secret(def), &*stored) {
            let text = Zeroizing::new(text.clone());
            *stored = seal(ctx, key, id, name, &text)?;
            sealed += 1;
        }
    }
    Ok(sealed)
}

/// Firewall and sandbox access that references imply. Only ever loopback ports a provider
/// declares and sockets in its own directory, which the resolver has already checked.
fn grants_for<'a>(network: impl Iterator<Item = &'a NetworkGrant>) -> ResolvedGrants {
    let mut tcp = BTreeSet::new();
    let mut udp = BTreeSet::new();
    let mut sockets = BTreeSet::new();
    for grant in network {
        match &grant.access {
            Access::Port {
                protocol: PortProtocol::Tcp,
                port,
            } => {
                tcp.insert(*port);
            }
            Access::Port {
                protocol: PortProtocol::Udp,
                port,
            } => {
                udp.insert(*port);
            }
            Access::UnixSocket { .. } => {
                sockets.insert(grant.provider.clone());
            }
        }
    }
    ResolvedGrants {
        loopback_tcp_ports: tcp.into_iter().collect(),
        loopback_udp_ports: udp.into_iter().collect(),
        socket_deps: sockets.into_iter().collect(),
        ..ResolvedGrants::default()
    }
}

/// Every installed package with settings, loaded and checked in one worker run.
struct World {
    installed: Installed,
    schemas: BTreeMap<String, Result<Schema, String>>,
    /// Packages with sealed values that could not be opened, because the device is locked.
    locked: BTreeSet<String>,
    /// Stored values for settings a package neither declares nor retired, by package.
    unknown: BTreeMap<String, Vec<String>>,
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
        let mut installed = Installed {
            role_bindings: ctx.bindings.roles.clone(),
            grants: ctx
                .bindings
                .grants
                .iter()
                .map(|g| Grant {
                    consumer: g.consumer.clone(),
                    provider: g.provider.clone(),
                    setting: g.setting.clone(),
                })
                .collect(),
            ..Installed::default()
        };
        let mut locked = BTreeSet::new();
        let mut unknown: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for package in ctx.packages {
            let schema = match response.schemas.get(package.id()) {
                Some(Ok(schema)) => schema.clone(),
                // Its own form reports the problem. Others see it as having no settings.
                Some(Err(_)) => continue,
                None => Schema::empty(),
            };
            let store = load_store(ctx, package.id())?;
            let mut values = BTreeMap::new();
            for (name, stored) in &store.values {
                let Some(def) = schema.setting(name) else {
                    // A retired setting's value is left for the next write of the store to drop.
                    if !schema.is_retired(name) {
                        unknown.entry(package.id().to_owned()).or_default().push(name.clone());
                    }
                    continue;
                };
                match stored_value(ctx.secrets, package.id(), def, stored) {
                    Ok(Some(value)) => {
                        values.insert(name.clone(), value);
                    }
                    Ok(None) => {
                        locked.insert(package.id().to_owned());
                    }
                    Err(_) => {}
                }
            }
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
            locked,
            unknown,
        })
    }

    fn schema(&self, id: &str) -> Result<&Schema, ApiError> {
        match self.schemas.get(id) {
            Some(Ok(schema)) => Ok(schema),
            Some(Err(problem)) => Err(failed(format!("{id}'s settings file failed: {problem}"))),
            None => Err(failed(format!("{id} has no settings file"))),
        }
    }

    /// Resolution, with a locked device reported as such rather than as missing values.
    fn resolve(&self, ctx: &SettingsContext, id: &str) -> Result<Resolved, ApiError> {
        if let Some(names) = self.unknown.get(id) {
            return Err(failed(unknown_settings(id, names)));
        }
        match self.installed.resolve(id) {
            Ok(resolved) => {
                if let Some(grant) = resolved
                    .grants_used
                    .iter()
                    .find(|g| self.locked.contains(&g.provider))
                {
                    return Err(no_key(ctx, &grant.provider));
                }
                Ok(resolved)
            }
            Err(errors) => match self.locked.iter().next() {
                Some(locked) => Err(no_key(ctx, locked)),
                None => Err(failed(join_errors(&errors))),
            },
        }
    }
}

/// Why stored values for settings a package neither declares nor retired are refused.
fn unknown_settings(id: &str, names: &[String]) -> String {
    let errors: Vec<ResolveError> = names
        .iter()
        .map(|name| ResolveError::UnknownSetting {
            package: id.to_owned(),
            setting: name.escape_debug().to_string(),
        })
        .collect();
    format!(
        "{}. This version of {id} neither declares nor retires it",
        join_errors(&errors)
    )
}

fn join_errors<E: std::fmt::Display>(errors: &[E]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

fn default_text(def: &SettingDef) -> Option<String> {
    if is_secret(def) {
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
    let secret = is_secret(def);
    let resolved_text = || {
        resolved
            .and_then(|r| r.values.get(&def.name))
            .filter(|_| !secret)
            .map(|value| value_text(value).to_string())
    };
    match stored {
        None if secret => SettingCurrent::SecretUnset,
        None => SettingCurrent::Default,
        Some(Stored::Sealed(_)) => SettingCurrent::SecretSet,
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
            None => match template_parts(text) {
                Some(parts) => SettingCurrent::Template {
                    parts,
                    resolved: resolved_text(),
                },
                None => SettingCurrent::Literal(SettingLiteral::Text(text.clone())),
            },
        },
    }
}

/// Turns one typed change into what is stored. Every literal is parsed by its type here, so
/// nothing malformed is ever stored. A secret comes back in clear and is sealed by the caller.
fn stored_for(def: &SettingDef, input: &SettingInput) -> Result<Option<Stored>, String> {
    let secret = is_secret(def);
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
        SettingInput::Template(_) if secret => Err(format!(
            "{} is a secret, so it must be sent as one",
            def.name
        )),
        SettingInput::Template(parts) => {
            let text = template_text(def, parts)?;
            // A template without references is a plain value, checked like one.
            match template_parts(&text) {
                Some(p) if p.iter().all(|p| matches!(p, TemplatePart::Text(_))) => {
                    let literal: String = p
                        .into_iter()
                        .map(|p| match p {
                            TemplatePart::Text(t) => t,
                            TemplatePart::Reference { .. } => unreachable!(),
                        })
                        .collect();
                    def.ty
                        .parse_literal(&literal)
                        .map_err(|m| format!("{} {m}", def.name))?;
                    Ok(Some(Stored::Literal(literal)))
                }
                _ => Ok(Some(Stored::Template(text))),
            }
        }
    }
}

/// The rendered configuration file of a package, with what its references imply.
fn render(
    ctx: &SettingsContext,
    world: &World,
    package: &Package,
    resolved: &Resolved,
) -> Result<(String, String, BTreeSet<NetworkGrant>, BTreeSet<String>), ApiError> {
    let id = package.id();
    match &package.manifest.config {
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
            let providers = rendered
                .dependencies
                .into_iter()
                .map(|d| d.package)
                .collect();
            Ok((
                config.name.clone(),
                rendered.text,
                rendered.network,
                providers,
            ))
        }
        None => Ok((
            FLAT_FILE.to_owned(),
            toml_file(&resolved.values).map_err(|e| failed(e.to_string()))?,
            BTreeSet::new(),
            BTreeSet::new(),
        )),
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
    ) -> Result<Plan, ApiError> {
        let id = package.id();
        let mut world = World::load(ctx)?;
        let schema = world.schema(id)?.clone();
        let mut store = load_store(ctx, id)?;
        let mut seen = BTreeSet::new();
        let mut new_secret = false;
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
            new_secret |= matches!(change.value, SettingInput::Secret(_));
            match stored_for(def, &change.value).map_err(failed)? {
                Some(stored) => store.values.insert(change.name.clone(), stored),
                None => store.values.remove(&change.name),
            };
        }
        // A value for a setting this version retired, such as after an update, is dropped and
        // the store written without it. A value for a setting it neither declares nor retired
        // is refused, so an update cannot silently lose a choice the owner made.
        let stale: Vec<String> = store
            .values
            .keys()
            .filter(|name| schema.setting(name).is_none())
            .cloned()
            .collect();
        let unknown: Vec<String> = stale
            .iter()
            .filter(|name| !schema.is_retired(name))
            .cloned()
            .collect();
        if !unknown.is_empty() {
            return Err(failed(unknown_settings(id, &unknown)));
        }
        for name in stale {
            store.values.remove(&name);
            eprintln!("nodo-helper: {id}: dropping the stored value of retired setting {name}");
        }
        world.unknown.remove(id);
        // A secret the package asks the device to generate is made once, the first time its
        // settings are written, and then kept like any other value. Resetting it makes a new one.
        let to_generate: Vec<&SettingDef> = schema
            .settings
            .iter()
            .filter(|def| def.generate.is_some() && !store.values.contains_key(&def.name))
            .collect();
        let has_sealed = store
            .values
            .values()
            .any(|s| matches!(s, Stored::Sealed(_)));
        let key = match ctx.secrets {
            Some(key) => Some(key),
            None if new_secret || has_sealed || !to_generate.is_empty() => {
                return Err(no_key(ctx, id))
            }
            None => None,
        };
        for def in to_generate {
            let generate = def.generate.expect("filtered above");
            let mut random = Zeroizing::new(vec![0u8; generate.byte_len()]);
            ctx.sys.random(&mut random)?;
            let secret = generate.encode(&random).map_err(failed)?;
            store.values.insert(
                def.name.clone(),
                Stored::Literal(secret.expose().to_owned()),
            );
        }
        // New secrets, and any left in clear from before sealing existed, are stored sealed.
        if let Some(key) = key {
            seal_clear_secrets(ctx, key, id, &schema, &mut store)?;
        }

        let mut values = BTreeMap::new();
        for (name, stored) in &store.values {
            let def = schema.setting(name).expect("retained above");
            let value = stored_value(key, id, def, stored)
                .map_err(|m| failed(format!("{name} {m}")))?
                .ok_or_else(|| no_key(ctx, id))?;
            values.insert(name.clone(), value);
        }
        world.locked.remove(id);
        if let Some(entry) = world.installed.packages.get_mut(id) {
            entry.values = values;
        }
        let resolved = world.resolve(ctx, id)?;

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

        // A base system service ships its own run script, which reads its config from a fixed
        // path, so only apps move theirs to memory. Their stored secrets are sealed either way.
        let sealed = package.profile == Profile::App && schema.settings.iter().any(is_secret);
        let mut layout = package.layout()?;
        if sealed {
            layout = layout
                .with_sealed_settings(id)
                .map_err(|e| failed(e.to_string()))?;
        }
        let (name, rendered, template_network, _) = render(ctx, &world, package, &resolved)?;
        let env = Zeroizing::new(env_file(&resolved.values).map_err(|e| failed(e.to_string()))?);
        let user = service_user(id).map_err(|e| failed(e.to_string()))?;
        let env_options = WriteOptions {
            mode: 0o600,
            group: None,
        };
        let files = vec![
            store_file(ctx, id, &store)?,
            PlannedFile {
                path: Path::new(&layout.config_dir).join(name),
                bytes: Zeroizing::new(rendered.into_bytes()),
                // Readable by the service through its group, and by nobody else, since it can
                // hold secrets.
                mode: 0o640,
                group: Some(user),
                parent_mode: 0o755,
            },
            // Last, because a service with in-memory settings starts once this file exists.
            PlannedFile {
                path: PathBuf::from(&layout.env_file),
                bytes: Zeroizing::new(env.as_bytes().to_vec()),
                mode: env_options.mode,
                group: None,
                parent_mode: if layout.settings_in_memory {
                    0o711
                } else {
                    0o700
                },
            },
        ];
        Ok(Plan {
            files,
            service: ServiceInputs {
                exec: Some(resolved.exec.clone()),
                grants: grants_for(resolved.network.iter().chain(&template_network)),
                sealed,
            },
        })
    }

    fn service(&self, ctx: &SettingsContext, package: &Package) -> Result<ServiceInputs, ApiError> {
        let world = World::load(ctx)?;
        let resolved = world.resolve(ctx, package.id())?;
        Ok(ServiceInputs {
            exec: Some(resolved.exec.clone()),
            grants: grants_for(resolved.network.iter()),
            sealed: false,
        })
    }

    fn dependents(&self, ctx: &SettingsContext, target: &Target) -> Result<Vec<String>, ApiError> {
        let world = World::load(ctx)?;
        let mut found = Vec::new();
        for package in ctx.packages {
            let id = package.id();
            if package.manifest.settings.is_none() || Target::Package(id.to_owned()) == *target {
                continue;
            }
            // A package that does not resolve now cannot be told apart, so it is re-rendered,
            // and if it still fails the change is refused rather than leaving it stale.
            let Ok(resolved) = world.installed.resolve(id) else {
                found.push(id.to_owned());
                continue;
            };
            let mut providers: BTreeSet<String> = resolved
                .dependencies
                .iter()
                .map(|d| d.package.clone())
                .collect();
            let mut roles: BTreeSet<String> = resolved
                .dependencies
                .iter()
                .filter_map(|d| d.role.clone())
                .collect();
            if package.manifest.config.is_some() {
                if let Ok((_, _, _, from_template)) = render(ctx, &world, package, &resolved) {
                    providers.extend(from_template);
                }
                // Roles in a template are not reported by name, so a role binding change
                // re-renders every templated package that depends on any provider of it.
                if let Target::Role(role) = target {
                    let provides = |p: &String| {
                        world
                            .installed
                            .packages
                            .get(p)
                            .is_some_and(|pkg| pkg.manifest.provides.contains(role))
                    };
                    if providers.iter().any(provides) {
                        roles.insert(role.clone());
                    }
                }
            }
            let depends = match target {
                Target::Package(provider) => providers.contains(provider),
                Target::Role(role) => roles.contains(role),
            };
            if depends {
                found.push(id.to_owned());
            }
        }
        Ok(found)
    }

    fn seal_stored(&self, ctx: &SettingsContext) -> Result<usize, ApiError> {
        let Some(key) = ctx.secrets else {
            return Ok(0);
        };
        let world = World::load(ctx)?;
        let mut total = 0;
        for package in ctx.packages {
            let Ok(schema) = world.schema(package.id()) else {
                continue;
            };
            let mut store = load_store(ctx, package.id())?;
            let sealed = seal_clear_secrets(ctx, key, package.id(), schema, &mut store)?;
            if sealed > 0 {
                let file = store_file(ctx, package.id(), &store)?;
                ctx.sys.atomic_write(
                    &file.path,
                    &file.bytes,
                    WriteOptions {
                        mode: file.mode,
                        group: None,
                    },
                )?;
                total += sealed;
            }
        }
        Ok(total)
    }

    fn references(
        &self,
        ctx: &SettingsContext,
        package: &Package,
    ) -> Result<Vec<ReferenceOption>, ApiError> {
        let world = World::load(ctx)?;
        Ok(world
            .installed
            .catalog(package.id())
            .into_iter()
            .map(|entry| ReferenceOption {
                package: entry.package,
                reference: entry.reference,
                label: entry.label,
                help: entry.help.unwrap_or_default(),
                kind: kind(&entry.ty),
                access: match entry.access {
                    CatalogAccess::Public { value } => ReferenceAccess::Public { value },
                    CatalogAccess::Grant { secret, granted } => {
                        ReferenceAccess::NeedsGrant { secret, granted }
                    }
                },
            })
            .collect())
    }

    fn preview(
        &self,
        ctx: &SettingsContext,
        package: &Package,
        change: &SettingChange,
    ) -> Result<SettingPreview, ApiError> {
        let id = package.id();
        let world = World::load(ctx)?;
        let def = world.schema(id)?.setting(&change.name).cloned().ok_or_else(|| {
            failed(format!("{id} has no setting named {}", change.name.escape_debug()))
        })?;
        if matches!(change.value, SettingInput::Secret(_)) {
            return Err(failed("a secret is not previewed".to_owned()));
        }
        let value = match stored_for(&def, &change.value).map_err(failed)? {
            Some(stored) => stored_value(None, id, &def, &stored)
                .map_err(|m| failed(format!("{} {m}", def.name)))?
                .ok_or_else(|| failed("a secret is not previewed".to_owned()))?,
            None => match &def.default {
                Some(default) => SettingValue::from(default),
                None => {
                    return Ok(SettingPreview {
                        resolved: None,
                        problem: Some(format!("{} has no default", def.name)),
                        references: Vec::new(),
                        new_dependencies: Vec::new(),
                        new_access: Vec::new(),
                    })
                }
            },
        };
        let before = world.installed.resolve(id).ok();
        let preview = world
            .installed
            .preview(id, &change.name, value)
            .map_err(|e| failed(e.to_string()))?;
        let had_dependency = |p: &str| {
            before
                .as_ref()
                .is_some_and(|b| b.dependencies.iter().any(|d| d.package == p))
        };
        let had_access =
            |g: &NetworkGrant| before.as_ref().is_some_and(|b| b.network.contains(g));
        let mut new_dependencies: Vec<String> = preview
            .dependencies
            .iter()
            .filter(|d| !had_dependency(&d.package))
            .map(|d| d.package.clone())
            .collect();
        new_dependencies.dedup();
        let new_access = preview
            .network
            .iter()
            .filter(|g| !had_access(g))
            .map(|g| ImpliedAccess {
                provider: g.provider.clone(),
                what: match &g.access {
                    Access::Port { protocol, port } => format!(
                        "port {port}/{}",
                        match protocol {
                            PortProtocol::Tcp => "tcp",
                            PortProtocol::Udp => "udp",
                        }
                    ),
                    Access::UnixSocket { path } => format!("socket {path}"),
                },
            })
            .collect();
        let references = preview
            .references
            .into_iter()
            .map(|check| ReferenceCheck {
                target: check.reference.target,
                setting: check.reference.setting,
                status: match check.state {
                    ReferenceState::Resolves { provider, value } => {
                        ReferenceStatus::Ok { provider, value }
                    }
                    ReferenceState::NotInstalled => ReferenceStatus::NotInstalled,
                    ReferenceState::NoSuchSetting { provider } => {
                        ReferenceStatus::NoSuchSetting { provider }
                    }
                    ReferenceState::NeedsGrant {
                        provider,
                        setting,
                        secret,
                    } => ReferenceStatus::NeedsGrant {
                        provider,
                        setting,
                        secret,
                    },
                    ReferenceState::TypeMismatch { expected, found } => {
                        ReferenceStatus::TypeMismatch { expected, found }
                    }
                    ReferenceState::Problem(problem) => ReferenceStatus::Problem(problem),
                },
            })
            .collect();
        let (resolved, problem) = match preview.value {
            Ok(value) => (value, None),
            Err(problem) => (None, Some(problem)),
        };
        Ok(SettingPreview {
            resolved,
            problem,
            references,
            new_dependencies,
            new_access,
        })
    }

    fn public_value(
        &self,
        ctx: &SettingsContext,
        package: &Package,
        public_name: &str,
    ) -> Result<Option<String>, ApiError> {
        let world = World::load(ctx)?;
        let Ok(schema) = world.schema(package.id()) else {
            return Ok(None);
        };
        let Some(def) = schema.exported(public_name).filter(|d| !is_secret(d)) else {
            return Ok(None);
        };
        let resolved = world.resolve(ctx, package.id())?;
        Ok(resolved
            .values
            .get(&def.name)
            .map(|value| value_text(value).to_string()))
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

    const LUA: &str = r#"return { api_version = 1, retired = { "restricted_rpc" }, settings = {
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
        // Installed by the helper, so its settings follow cuprated and its service is generated.
        sys.put("/s/installed/shop", b"");
        Helper::new(
            sys,
            AllowedUids::new([1000]),
            Box::new(NodoSettings),
            Box::new(NoCaddyYet),
        )
        .with_paths(TEST_PATHS())
        .with_kdf(crate::keys::KdfParams {
            memory_kib: 64,
            iterations: 1,
            lanes: 1,
        })
    }

    const PASS: &str = "correct horse battery";

    /// Set up and unlocked, as a device is right after first-boot setup.
    fn unlocked() -> Helper<FakeSystem> {
        let h = setup();
        h.init_device(&Secret::new(PASS)).unwrap();
        h
    }

    /// The same machine after a reboot: a new helper with nothing in memory.
    fn rebooted(h: Helper<FakeSystem>) -> Helper<FakeSystem> {
        let fresh = setup();
        h.sys.reboot();
        Helper {
            sys: h.sys,
            ..fresh
        }
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

    fn tpl(parts: &[(&str, &str)]) -> SettingInput {
        // ("", text) is text; (target, setting) is a reference.
        SettingInput::Template(
            parts
                .iter()
                .map(|(target, rest)| {
                    if target.is_empty() {
                        TemplatePart::Text((*rest).into())
                    } else {
                        TemplatePart::Reference {
                            target: (*target).into(),
                            setting: (*rest).into(),
                        }
                    }
                })
                .collect(),
        )
    }

    fn preview(h: &Helper<FakeSystem>, package: &str, name: &str, value: SettingInput) -> SettingPreview {
        match h.handle(
            1000,
            Request::PreviewSetting {
                package: package.into(),
                change: change(name, value),
            },
        ) {
            Ok(Response::SettingPreview(p)) => p,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn references_list_public_values_and_lock_the_rest() {
        let h = setup();
        let Ok(Response::References(options)) = h.handle(
            1000,
            Request::ListReferences {
                package: "shop".into(),
            },
        ) else {
            panic!()
        };
        assert!(options.iter().all(|o| o.package == "cuprated"));
        let port = options.iter().find(|o| o.reference == "rpc-port").unwrap();
        assert_eq!(
            port.access,
            ReferenceAccess::Public {
                value: Some("18081".into())
            }
        );
        let token = options.iter().find(|o| o.reference == "token").unwrap();
        assert_eq!(
            token.access,
            ReferenceAccess::NeedsGrant {
                secret: true,
                granted: false
            }
        );
        let peers = options
            .iter()
            .find(|o| o.reference == "outbound_peers")
            .unwrap();
        assert!(matches!(
            peers.access,
            ReferenceAccess::NeedsGrant { secret: false, .. }
        ));
    }

    #[test]
    fn text_and_references_together_are_stored_and_reported_as_pieces() {
        let h = setup();
        let value = tpl(&[("", "port "), ("cuprated", "rpc-port"), ("", " $(not.a.ref)")]);
        apply(&h, "shop", vec![change("label", value)]).unwrap();
        let out: toml::Table = toml::from_str(
            std::str::from_utf8(&h.sys.get("/etc/apps/shop/settings.toml").unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(out["label"].as_str(), Some("port 18081 $(not.a.ref)"));
        let field = form(&h, "shop")
            .fields
            .into_iter()
            .find(|f| f.name == "label")
            .unwrap();
        assert_eq!(
            field.current,
            SettingCurrent::Template {
                parts: vec![
                    TemplatePart::Text("port ".into()),
                    TemplatePart::Reference {
                        target: "cuprated".into(),
                        setting: "rpc-port".into()
                    },
                    TemplatePart::Text(" $(not.a.ref)".into()),
                ],
                resolved: Some("port 18081 $(not.a.ref)".into()),
            }
        );
    }

    #[test]
    fn a_template_of_only_text_is_a_plain_value_and_secrets_refuse_templates() {
        let h = setup();
        apply(&h, "shop", vec![change("label", tpl(&[("", "$(x.y) plain")]))]).unwrap();
        let field = form(&h, "shop")
            .fields
            .into_iter()
            .find(|f| f.name == "label")
            .unwrap();
        assert_eq!(
            field.current,
            SettingCurrent::Literal(SettingLiteral::Text("$(x.y) plain".into()))
        );
        let h = unlocked();
        assert!(apply(&h, "cuprated", vec![change("token", tpl(&[("", "abc")]))]).is_err());
    }

    #[test]
    fn a_preview_reports_each_reference_and_writes_nothing() {
        let h = setup();
        let writes = |h: &Helper<FakeSystem>| {
            h.sys
                .calls()
                .iter()
                .filter(|c| c.starts_with("write"))
                .count()
        };
        let before = writes(&h);
        let p = preview(
            &h,
            "shop",
            "label",
            tpl(&[
                ("", "http://127.0.0.1:"),
                ("cuprated", "rpc-port"),
                ("", "/"),
                ("old-shop", "url"),
                ("cuprated", "token"),
            ]),
        );
        assert_eq!(
            p.references[0].status,
            ReferenceStatus::Ok {
                provider: "cuprated".into(),
                value: Some("18081".into())
            }
        );
        assert_eq!(p.references[1].status, ReferenceStatus::NotInstalled);
        assert!(matches!(
            p.references[2].status,
            ReferenceStatus::NeedsGrant { secret: true, .. }
        ));
        assert!(p.problem.is_some() && p.resolved.is_none());
        let ok = preview(
            &h,
            "shop",
            "label",
            tpl(&[("", "http://127.0.0.1:"), ("cuprated", "rpc-port")]),
        );
        assert_eq!(ok.resolved.as_deref(), Some("http://127.0.0.1:18081"));
        assert_eq!(writes(&h), before, "{:?}", h.sys.calls());
    }

    #[test]
    fn a_preview_says_what_a_new_reference_would_open() {
        let h = setup();
        apply(&h, "shop", vec![change("daemon_port", int(18081))]).unwrap();
        let p = preview(
            &h,
            "shop",
            "daemon_port",
            SettingInput::Reference {
                target: "cuprated".into(),
                setting: "rpc-port".into(),
            },
        );
        assert_eq!(p.resolved.as_deref(), Some("18081"));
        assert_eq!(p.new_dependencies, ["cuprated"]);
        let mixed = preview(&h, "shop", "daemon_port", tpl(&[("", "1"), ("cuprated", "rpc-port")]));
        assert!(matches!(
            mixed.references[0].status,
            ReferenceStatus::TypeMismatch { .. }
        ));
    }

    #[test]
    fn a_secret_is_never_previewed() {
        let h = unlocked();
        assert!(h
            .handle(
                1000,
                Request::PreviewSetting {
                    package: "cuprated".into(),
                    change: change("token", SettingInput::Secret(Secret::new("hunter2"))),
                },
            )
            .is_err());
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
        let h = unlocked();
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

    const CUPRATED_STORE: &str = "/s/settings/cuprated.toml";

    #[test]
    fn a_stored_value_for_a_retired_setting_is_dropped_at_the_next_write() {
        let h = setup();
        h.sys.put(
            CUPRATED_STORE,
            b"[values]\nrestricted_rpc = { literal = \"true\" }\noutbound_peers = { literal = \"40\" }\n",
        );
        // Read paths ignore it: the form and the node's public values still resolve.
        assert_eq!(
            form(&h, "cuprated")
                .fields
                .iter()
                .find(|f| f.name == "outbound_peers")
                .map(|f| f.current.clone()),
            Some(SettingCurrent::Literal(SettingLiteral::Int(40)))
        );
        apply(&h, "cuprated", vec![change("log_level", text("warn"))]).unwrap();
        let store = stored(&h, "cuprated");
        assert!(!store.contains("restricted_rpc"), "{store}");
        assert!(store.contains("outbound_peers"), "other values stay: {store}");
        assert_eq!(output(&h)["p2p"]["clear_net"]["outbound_connections"].as_integer(), Some(40));
    }

    #[test]
    fn a_stored_value_for_a_setting_neither_declared_nor_retired_is_refused() {
        let h = setup();
        let before = b"[values]\nghost = { literal = \"1\" }\n".to_vec();
        h.sys.put(CUPRATED_STORE, &before);
        let Err(ApiError::Failed(message)) =
            apply(&h, "cuprated", vec![change("log_level", text("warn"))])
        else {
            panic!("an unknown stored value must be refused")
        };
        assert!(message.contains("unknown setting 'ghost'"), "{message}");
        assert!(message.contains("neither declares nor retires"), "{message}");
        assert_eq!(h.sys.get(CUPRATED_STORE).unwrap(), before, "nothing written");
        assert_eq!(h.sys.get(OUT), None);
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
            workers, 5,
            "one load for the form, one load and one validation for the change, one load to \
             find dependents and one to re-plan shop, which follows cuprated's RPC port"
        );
    }

    #[test]
    fn a_generated_secret_is_made_once_and_kept() {
        let h = unlocked();
        h.sys.put(
            "/opt/nodo/apps/shop/settings.lua",
            br#"return { api_version = 1, settings = {
                { name = "label", type = "string", label = "Label" },
                { name = "db_key", type = "secret", label = "Database key", generate = { hex = 16 } },
            } }"#,
        );
        apply(&h, "shop", vec![change("label", text("a"))]).unwrap();
        let read = |h: &Helper<FakeSystem>| -> toml::Table {
            toml::from_str(
                std::str::from_utf8(
                    &h.sys
                        .get("/run/nodo/secrets/shop/config/settings.toml")
                        .unwrap(),
                )
                .unwrap(),
            )
            .unwrap()
        };
        let first = read(&h)["db_key"].as_str().unwrap().to_owned();
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
        apply(&h, "shop", vec![change("label", text("b"))]).unwrap();
        assert_eq!(
            read(&h)["db_key"].as_str().unwrap(),
            first,
            "never regenerated"
        );
        let field = form(&h, "shop")
            .fields
            .into_iter()
            .find(|f| f.name == "db_key")
            .unwrap();
        assert_eq!(field.current, SettingCurrent::SecretSet);
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

    fn secret(value: &str) -> SettingInput {
        SettingInput::Secret(Secret::new(value))
    }

    const VAULTED_SHOP_LUA: &str = r#"return { api_version = 1, settings = {
        { name = "daemon_port", type = "int", label = "Daemon port", default = "$(monero-daemon.rpc-port)" },
        { name = "label", type = "string", label = "Label" },
        { name = "token", type = "secret", label = "Token" },
    } }"#;

    fn shop_with_token() -> Helper<FakeSystem> {
        let h = unlocked();
        h.sys.put(
            "/opt/nodo/apps/shop/settings.lua",
            VAULTED_SHOP_LUA.as_bytes(),
        );
        h
    }

    fn stored(h: &Helper<FakeSystem>, id: &str) -> String {
        String::from_utf8(h.sys.get(&format!("/s/settings/{id}.toml")).unwrap()).unwrap()
    }

    #[test]
    fn a_secret_is_stored_sealed_and_rendered_only_to_memory() {
        let h = shop_with_token();
        apply(&h, "shop", vec![change("token", secret("s3cret-token"))]).unwrap();
        let store = stored(&h, "shop");
        assert!(!store.contains("s3cret"), "{store}");
        assert!(store.contains("sealed = \"v1."), "{store}");
        let config = h
            .sys
            .get("/run/nodo/secrets/shop/config/settings.toml")
            .unwrap();
        assert!(String::from_utf8(config)
            .unwrap()
            .contains("token = \"s3cret-token\""));
        let env = String::from_utf8(h.sys.get("/run/nodo/secrets/shop/env").unwrap()).unwrap();
        assert!(env.contains("NODO_TOKEN='s3cret-token'\n"), "{env}");
        assert!(env.contains("NODO_DAEMON_PORT='18081'\n"), "{env}");
        let calls = h.sys.calls();
        assert!(calls.contains(&"write /run/nodo/secrets/shop/env mode=600 group=-".to_owned()));
        assert!(calls.contains(&"mkdir /run/nodo/secrets 711 -".to_owned()));
        // Nothing of the package's settings reaches the system disk in clear.
        assert_eq!(h.sys.get("/etc/apps/shop/settings.toml"), None);
        assert_eq!(h.sys.get("/var/lib/nodo/env/shop.env"), None);
    }

    #[test]
    fn a_sealed_secret_is_unreadable_while_locked() {
        let h = shop_with_token();
        apply(&h, "shop", vec![change("token", secret("s3cret-token"))]).unwrap();
        let h = rebooted(h);
        assert_eq!(
            h.sys.get("/run/nodo/secrets/shop/env"),
            None,
            "memory is empty after a reboot"
        );
        // The form still works and says only that the secret is set.
        let field = form(&h, "shop")
            .fields
            .into_iter()
            .find(|f| f.name == "token")
            .unwrap();
        assert_eq!(field.current, SettingCurrent::SecretSet);
        h.sys.clear_calls();
        for changes in [
            vec![change("label", text("x"))],
            vec![change("token", secret("another"))],
        ] {
            assert!(
                matches!(apply(&h, "shop", changes), Err(ApiError::Locked(_))),
                "nothing that needs the secret works while locked"
            );
        }
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("write")));
        // Unlocking renders it again, which is what the waiting service starts on.
        h.handle(
            1000,
            Request::Unlock {
                passphrase: Secret::new(PASS),
            },
        )
        .unwrap();
        let env = String::from_utf8(h.sys.get("/run/nodo/secrets/shop/env").unwrap()).unwrap();
        assert!(env.contains("NODO_TOKEN='s3cret-token'"));
    }

    #[test]
    fn another_device_key_cannot_open_a_sealed_store() {
        let h = shop_with_token();
        apply(&h, "shop", vec![change("token", secret("s3cret-token"))]).unwrap();
        let store = h.sys.get("/s/settings/shop.toml").unwrap();
        // The fake's randomness is a counter, so draw once to get a different device key.
        let other = setup();
        crate::system::System::random(&other.sys, &mut [0u8; 1]).unwrap();
        other.init_device(&Secret::new(PASS)).unwrap();
        other.sys.put(
            "/opt/nodo/apps/shop/settings.lua",
            VAULTED_SHOP_LUA.as_bytes(),
        );
        other.sys.put("/s/settings/shop.toml", &store);
        assert!(apply(&other, "shop", vec![change("label", text("x"))]).is_err());
    }

    #[test]
    fn secrets_need_the_device_passphrase_set_up_first() {
        let h = setup();
        h.sys.put(
            "/opt/nodo/apps/shop/settings.lua",
            VAULTED_SHOP_LUA.as_bytes(),
        );
        let Err(ApiError::Locked(message)) =
            apply(&h, "shop", vec![change("token", secret("s3cret"))])
        else {
            panic!()
        };
        assert!(message.contains("set it up first"), "{message}");
        // Settings without a secret value still work, and render to memory.
        apply(&h, "shop", vec![change("label", text("x"))]).unwrap();
        assert!(h.sys.get("/run/nodo/secrets/shop/env").is_some());
    }

    #[test]
    fn secrets_stored_in_clear_are_sealed_at_the_first_unlock() {
        let h = setup();
        h.sys.put(
            "/opt/nodo/apps/shop/settings.lua",
            VAULTED_SHOP_LUA.as_bytes(),
        );
        // As an older helper stored it, before sealing existed.
        h.sys.put(
            "/s/settings/shop.toml",
            b"[values.token]\nliteral = \"old-clear-token\"\n\n[values.label]\nliteral = \"kept\"\n",
        );
        h.init_device(&Secret::new(PASS)).unwrap();
        let store = stored(&h, "shop");
        assert!(!store.contains("old-clear-token"), "{store}");
        assert!(
            store.contains("literal = \"kept\""),
            "only secrets are sealed: {store}"
        );
        let h = rebooted(h);
        h.handle(
            1000,
            Request::Unlock {
                passphrase: Secret::new(PASS),
            },
        )
        .unwrap();
        let env = String::from_utf8(h.sys.get("/run/nodo/secrets/shop/env").unwrap()).unwrap();
        assert!(env.contains("NODO_TOKEN='old-clear-token'"), "{env}");
    }

    const ENDPOINT_LUA: &str = r#"return { api_version = 1, settings = {
        { name = "outbound_peers", type = "int", label = "Outbound peers", default = 32, min = 1, max = 128 },
        { name = "rpc_port", type = "int", label = "RPC port", default = 18081, public = "rpc-port", endpoint = "tcp" },
    } }"#;

    /// cuprated declares its RPC port as an endpoint, and shop is an installed app.
    fn with_endpoint() -> Helper<FakeSystem> {
        let h = setup();
        h.sys.put(
            "/m/sys/cuprated.toml",
            format!("{CUPRATED}\n[[network.inbound]]\nport = 18081\nprotocol = \"tcp\"\nscope = \"local\"\n\n[[network.inbound]]\nport = 18089\nprotocol = \"tcp\"\nscope = \"lan\"\n").as_bytes(),
        );
        h.sys
            .put("/p/sys/cuprated/settings.lua", ENDPOINT_LUA.as_bytes());
        h.sys.put(
            "/p/sys/cuprated/Cuprated.toml.in",
            b"[p2p.clear_net]\noutbound_connections = \"$(cuprated.outbound_peers)\"\n",
        );
        h.sys.put("/s/installed/shop", b"");
        h
    }

    fn fragment(h: &Helper<FakeSystem>) -> String {
        String::from_utf8(h.sys.get("/nft/shop.nft").unwrap()).unwrap()
    }

    #[test]
    fn a_reference_opens_its_port_and_removing_it_closes_it_again() {
        let h = with_endpoint();
        apply(&h, "shop", vec![change("label", text("a"))]).unwrap();
        let rule = "meta skuid \"_shop\" oifname \"lo\" tcp dport 18081 accept";
        assert!(fragment(&h).contains(rule), "{}", fragment(&h));
        assert!(h
            .sys
            .calls()
            .contains(&"nft check /nft/shop.nft.pending".to_owned()));
        assert!(h
            .sys
            .calls()
            .contains(&"nft load /nft/main.conf".to_owned()));
        // The sandbox runs the resolved command line and reads the settings as environment.
        let run = String::from_utf8(h.sys.get("/sv/shop/run").unwrap()).unwrap();
        assert!(run.contains(". /var/lib/nodo/env/shop.env"), "{run}");

        // A literal port implies nothing, so the access goes with the reference.
        h.sys.clear_calls();
        apply(&h, "shop", vec![change("daemon_port", int(18081))]).unwrap();
        assert!(!fragment(&h).contains("18081"), "{}", fragment(&h));
        assert!(h
            .sys
            .calls()
            .contains(&"nft load /nft/main.conf".to_owned()));

        // And comes back with it.
        let reference = SettingInput::Reference {
            target: "monero-daemon".into(),
            setting: "rpc-port".into(),
        };
        apply(&h, "shop", vec![change("daemon_port", reference)]).unwrap();
        assert!(fragment(&h).contains(rule));
    }

    #[test]
    fn a_rejected_fragment_leaves_the_settings_and_firewall_alone() {
        let h = with_endpoint();
        apply(&h, "shop", vec![change("label", text("a"))]).unwrap();
        let before = (
            h.sys.get("/nft/shop.nft"),
            h.sys.get("/s/settings/shop.toml"),
        );
        h.sys.fail("nft check");
        assert!(apply(&h, "shop", vec![change("daemon_port", int(18081))]).is_err());
        assert_eq!(
            before,
            (
                h.sys.get("/nft/shop.nft"),
                h.sys.get("/s/settings/shop.toml")
            )
        );
    }

    #[test]
    fn a_public_setting_change_re_renders_and_restarts_its_dependents() {
        let h = with_endpoint();
        h.sys.put(
            "/p/sys/cuprated/settings.lua",
            ENDPOINT_LUA
                .replace("max = 18081", "")
                .replace(
                    "default = 18081, public",
                    "default = 18081, min = 1024, max = 65535, public",
                )
                .as_bytes(),
        );
        apply(&h, "shop", vec![change("label", text("a"))]).unwrap();
        h.sys.clear_calls();
        // Not declared inbound, so the reference cannot follow it and the change is refused
        // before anything is written.
        assert!(apply(&h, "cuprated", vec![change("rpc_port", int(18082))]).is_err());
        assert!(!h.sys.calls().iter().any(|c| c.starts_with("write")));
        // A declared port works: shop follows it, in its settings and its firewall.
        apply(&h, "cuprated", vec![change("rpc_port", int(18089))]).unwrap();
        let shop: toml::Table = toml::from_str(
            std::str::from_utf8(&h.sys.get("/etc/apps/shop/settings.toml").unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(shop["daemon_port"].as_integer(), Some(18089));
        assert!(fragment(&h).contains("tcp dport 18089 accept"));
        assert!(!fragment(&h).contains("18081"));
        let restarts: Vec<String> = h
            .sys
            .calls()
            .into_iter()
            .filter(|c| c.starts_with("sv force-restart"))
            .collect();
        assert_eq!(
            restarts,
            [
                "sv force-restart cuprated -w120",
                "sv force-restart shop -w120"
            ]
        );
        // A change no other package references restarts only its own package.
        h.sys.clear_calls();
        apply(&h, "cuprated", vec![change("outbound_peers", int(9))]).unwrap();
        assert!(!h.sys.calls().iter().any(|c| c.contains("restart shop")));
    }

    #[test]
    fn a_failing_dependent_rolls_back_the_whole_change() {
        let h = with_endpoint().with_timing(crate::helper::Timing {
            health_attempts: 3,
            ..crate::helper::Timing::default()
        });
        h.sys.put(
            "/p/sys/cuprated/settings.lua",
            ENDPOINT_LUA
                .replace(
                    "default = 18081, public",
                    "default = 18081, min = 1024, max = 65535, public",
                )
                .as_bytes(),
        );
        apply(&h, "shop", vec![change("label", text("a"))]).unwrap();
        let cuprated_before = (h.sys.get(OUT), h.sys.get("/s/settings/cuprated.toml"));
        let shop_before = (
            h.sys.get("/etc/apps/shop/settings.toml"),
            h.sys.get("/var/lib/nodo/env/shop.env"),
            h.sys.get("/nft/shop.nft"),
            h.sys.get("/sv/shop/run"),
        );
        h.sys.clear_calls();
        // cuprated comes back healthy on its new port, then shop never stays up on it.
        h.sys
            .script_status("shop", &["down: shop: 1s, normally up, want up\n"; 3]);
        let rollback = match apply(&h, "cuprated", vec![change("rpc_port", int(18089))]) {
            Ok(Response::RolledBack(rollback)) => rollback,
            other => panic!("{other:?}"),
        };
        assert!(
            rollback.reason.contains("shop depends on this change"),
            "{rollback:?}"
        );
        assert!(rollback.recovered, "{rollback:?}");
        assert_eq!(
            cuprated_before,
            (h.sys.get(OUT), h.sys.get("/s/settings/cuprated.toml"))
        );
        assert_eq!(
            shop_before,
            (
                h.sys.get("/etc/apps/shop/settings.toml"),
                h.sys.get("/var/lib/nodo/env/shop.env"),
                h.sys.get("/nft/shop.nft"),
                h.sys.get("/sv/shop/run"),
            )
        );
        let calls = h.sys.calls();
        let restarts: Vec<&String> = calls
            .iter()
            .filter(|c| c.starts_with("sv force-restart"))
            .collect();
        assert_eq!(
            restarts.len(),
            4,
            "both on the new files, then both on the old: {restarts:?}"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|c| *c == "nft load /nft/main.conf")
                .count(),
            2,
            "the old firewall is loaded again"
        );
    }

    #[test]
    fn a_dependent_that_no_longer_resolves_refuses_the_change() {
        let h = with_endpoint();
        apply(&h, "shop", vec![change("label", text("a"))]).unwrap();
        h.sys.put(
            "/opt/nodo/apps/shop/settings.lua",
            br#"return { api_version = 1, settings = {
                { name = "daemon_port", type = "int", label = "Daemon port", default = "$(monero-daemon.rpc-port)",
                  validate = function(v) return v ~= 18089, "not that port" end },
                { name = "label", type = "string", label = "Label" },
            } }"#,
        );
        h.sys.put(
            "/p/sys/cuprated/settings.lua",
            ENDPOINT_LUA
                .replace(
                    "default = 18081, public",
                    "default = 18081, min = 1024, max = 65535, public",
                )
                .as_bytes(),
        );
        let before = h.sys.get("/s/settings/cuprated.toml");
        h.sys.clear_calls();
        let Err(ApiError::Failed(message)) =
            apply(&h, "cuprated", vec![change("rpc_port", int(18089))])
        else {
            panic!()
        };
        assert!(
            message.contains("shop") && message.contains("not that port"),
            "{message}"
        );
        assert_eq!(h.sys.get("/s/settings/cuprated.toml"), before, "put back");
        assert!(!h
            .sys
            .calls()
            .iter()
            .any(|c| c.starts_with("sv force-restart")));
    }

    #[test]
    fn approving_a_grant_lets_a_package_read_a_secret_and_revoking_it_in_use_is_refused() {
        let h = unlocked();
        h.sys.put(
            "/opt/nodo/apps/shop/settings.lua",
            br#"return { api_version = 1, settings = {
                { name = "upstream_token", type = "secret", label = "Upstream token" },
            } }"#,
        );
        apply(&h, "cuprated", vec![change("token", secret("node-secret"))]).unwrap();
        let reference = || SettingInput::Reference {
            target: "cuprated".into(),
            setting: "token".into(),
        };
        assert!(apply(&h, "shop", vec![change("upstream_token", reference())]).is_err());
        let grant = nodo_api::SettingGrant {
            consumer: "shop".into(),
            provider: "cuprated".into(),
            setting: "token".into(),
        };
        h.handle(
            1000,
            Request::SetGrant {
                grant: grant.clone(),
                approved: true,
            },
        )
        .unwrap();
        apply(&h, "shop", vec![change("upstream_token", reference())]).unwrap();
        let env = String::from_utf8(h.sys.get("/run/nodo/secrets/shop/env").unwrap()).unwrap();
        assert!(env.contains("NODO_UPSTREAM_TOKEN='node-secret'"), "{env}");
        let revoke = h.handle(
            1000,
            Request::SetGrant {
                grant,
                approved: false,
            },
        );
        assert!(revoke.is_err(), "{revoke:?}");
        assert_eq!(
            h.load_bindings().unwrap().grants.len(),
            1,
            "the grant stays"
        );
    }

    #[test]
    fn a_placed_but_uninstalled_package_never_blocks_a_change() {
        let h = unlocked();
        // Its settings file is broken, which only matters once it is installed.
        h.sys.add_manifest(
            "api_version = 1\nid = \"ghost\"\nversion = \"1\"\nexec = [\"bin/g\"]\nsettings = \"s.lua\"\n",
        );
        h.sys.put("/opt/nodo/apps/ghost/s.lua", b"return nil");
        apply(&h, "cuprated", vec![change("outbound_peers", int(9))]).unwrap();
        h.handle(
            1000,
            Request::Unlock {
                passphrase: Secret::new(PASS),
            },
        )
        .unwrap();
        assert!(!h.sys.calls().iter().any(|c| c.contains("ghost")));
    }

    #[test]
    fn binding_a_role_re_renders_the_packages_that_use_it() {
        let h = with_endpoint();
        apply(&h, "shop", vec![change("label", text("a"))]).unwrap();
        h.sys.add_manifest(
            "api_version = 1\nid = \"othernode\"\nversion = \"1\"\nexec = [\"bin/node\"]\nprovides = [\"monero-daemon\"]\nsettings = \"s.lua\"\n\n[[network.inbound]]\nport = 28081\nprotocol = \"tcp\"\nscope = \"local\"\n",
        );
        h.sys.put(
            "/opt/nodo/apps/othernode/s.lua",
            br#"return { api_version = 1, settings = {
                { name = "rpc", type = "int", label = "RPC", default = 28081, public = "rpc-port", endpoint = "tcp" },
            } }"#,
        );
        h.handle(
            1000,
            Request::SetRoleBinding {
                role: "monero-daemon".into(),
                package: Some("othernode".into()),
            },
        )
        .unwrap();
        assert!(
            fragment(&h).contains("tcp dport 28081 accept"),
            "{}",
            fragment(&h)
        );
        assert!(!fragment(&h).contains("18081"));
    }

    /// The shipped cuprated settings export the restricted RPC port, the only port the node's
    /// onion address may ever reach. RPC is always restricted, so there is no switch for it.
    #[test]
    fn shipped_cuprated_exports_its_restricted_rpc_port() {
        let h = setup();
        h.sys.put(
            "/m/sys/cuprated.toml",
            include_str!("../../../packages/nodo-tools/files/cuprated/manifest.toml").as_bytes(),
        );
        h.sys.put(
            "/p/sys/cuprated/settings.lua",
            include_str!("../../../packages/nodo-tools/files/cuprated/settings.lua").as_bytes(),
        );
        let (packages, _) = h.packages().unwrap();
        let cuprated = packages.iter().find(|p| p.id() == "cuprated").unwrap();
        let read = |name: &str| {
            h.with_settings(&packages, |ctx| h.settings.public_value(ctx, cuprated, name))
                .unwrap()
        };
        assert_eq!(read("restricted-rpc").as_deref(), None);
        assert_eq!(read("restricted-rpc-port").as_deref(), Some("18081"));
        assert_eq!(read("rpc-port").as_deref(), Some("18081"));
    }
}
