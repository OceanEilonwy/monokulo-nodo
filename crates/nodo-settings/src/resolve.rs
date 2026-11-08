//! Resolves `$(package.setting)` references across installed packages.
//!
//! Resolution is pure: it reads an [`Installed`] snapshot and returns values plus what they imply.
//! It runs no Lua, so root code may use it. Package validation functions are a separate step, see
//! [`crate::LoadedSettings::validate_values`].
//!
//! Access rules, all enforced per reference by the package holding the reference:
//!
//! - A public setting is open to any package, under its public name.
//! - A non-public setting or a secret needs a [`Grant`] naming the concrete provider, so a role
//!   moving to another package does not carry the user's approval along with it.
//! - A secret can only flow into a secret. Since secrets are never public, one package holding a
//!   grant cannot launder the secret to a third package through a public setting of its own.

use std::collections::{BTreeMap, BTreeSet};

use nodo_manifest::{is_valid_package_path, Manifest, Protocol};
use thiserror::Error;

use crate::escape::toml_value;
use crate::schema::{Endpoint, Schema, SecretString, SettingDef, SettingType, SettingValue, Value, MAX_TEXT_LEN};
use crate::template::{literal_text, parse_template, Part, Reference, TemplateError};
use crate::url::check_url;

/// Chains longer than this are not plausible configuration.
const MAX_REFERENCE_DEPTH: usize = 32;
const MAX_TOML_OUTPUT: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResolveError {
    #[error("package '{0}' is not installed")]
    UnknownPackage(String),
    #[error("{package}: value given for unknown setting '{setting}'")]
    UnknownSetting { package: String, setting: String },
    #[error("{package}.{setting}: {error}")]
    Template { package: String, setting: String, error: TemplateError },
    #[error("{consumer} needs '{name}', but no installed package provides it")]
    MissingProvider { consumer: String, name: String },
    #[error("several packages provide '{role}': {}. Choose one", .candidates.join(", "))]
    AmbiguousProvider { role: String, candidates: Vec<String> },
    #[error("'{name}' is both a package and a role provided by {provider}")]
    AmbiguousName { name: String, provider: String },
    #[error("role '{role}' is bound to '{package}', which does not provide it")]
    InvalidBinding { role: String, package: String },
    #[error("{provider} has no setting that can be referenced as '{setting}'")]
    MissingSetting { provider: String, setting: String },
    #[error("{consumer} needs approval to read {provider}.{setting}{}", if *.secret { " (a secret)" } else { " (not public)" })]
    GrantRequired { consumer: String, provider: String, setting: String, secret: bool },
    #[error("{package}.{setting} is {expected}, so it cannot take {found} from {reference}")]
    TypeMismatch { package: String, setting: String, reference: String, expected: String, found: String },
    #[error("{package}.{setting} {problem}")]
    InvalidValue { package: String, setting: String, problem: String },
    #[error("{package}.{setting} has no value")]
    MissingValue { package: String, setting: String },
    #[error("settings form a cycle: {}", .path.join(" -> "))]
    Cycle { path: Vec<String> },
    #[error("packages depend on each other: {}", .path.join(" -> "))]
    PackageCycle { path: Vec<String> },
    #[error("references starting at {package}.{setting} are nested too deeply")]
    TooDeep { package: String, setting: String },
    #[error("{package}.{setting} names port {port}, which the package does not declare as inbound")]
    UndeclaredEndpoint { package: String, setting: String, port: i64 },
    #[error("{package}.{setting} is an endpoint, but its value is not an address inside this package")]
    InvalidEndpoint { package: String, setting: String },
    #[error("exec argument {index} of {package} cannot hold a secret, because command lines are visible to other processes")]
    SecretInArgument { package: String, index: usize },
    #[error("exec argument {index} of {package} would start with '-' after substitution, which could inject an option")]
    OptionInjection { package: String, index: usize },
    #[error("the configuration template is invalid: {0}")]
    InvalidToml(String),
    #[error("the configuration template has a reference in a key: {0}")]
    ReferenceInKey(String),
    #[error("the generated configuration is too large")]
    OutputTooLarge,
}

/// User approval for one package to read one otherwise inaccessible setting of another.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Grant {
    pub consumer: String,
    /// A concrete package id, never a role.
    pub provider: String,
    /// The setting's own name, not a public name.
    pub setting: String,
}

/// A package another one depends on, directly or through a chain of references.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dependency {
    pub package: String,
    /// The role this was reached through, when it was referenced by role.
    pub role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    Port { protocol: PortProtocol, port: u16 },
    UnixSocket { path: String },
}

/// `nodo_manifest::Protocol` has no ordering, and grants are kept in sorted sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PortProtocol {
    Tcp,
    Udp,
}

/// Firewall and sandbox access that the generators must open for a resolved reference.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NetworkGrant {
    pub consumer: String,
    pub provider: String,
    pub access: Access,
}

/// One package as the resolver sees it.
#[derive(Debug, Clone)]
pub struct InstalledPackage {
    pub manifest: Manifest,
    pub schema: Schema,
    /// Values the user has set. Anything absent uses the schema's default.
    pub values: BTreeMap<String, SettingValue>,
}

/// A snapshot of everything resolution depends on.
#[derive(Debug, Clone, Default)]
pub struct Installed {
    pub packages: BTreeMap<String, InstalledPackage>,
    /// The user's choice of provider where several packages provide one role.
    pub role_bindings: BTreeMap<String, String>,
    pub grants: BTreeSet<Grant>,
}

/// Everything a reference implies, gathered as evaluation walks the graph.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Trace {
    dependencies: BTreeSet<Dependency>,
    /// Provider and access, before the consumer is known.
    access: BTreeSet<(String, Access)>,
    grants: BTreeSet<Grant>,
}

impl Trace {
    fn merge(&mut self, other: &Trace) {
        self.dependencies.extend(other.dependencies.iter().cloned());
        self.access.extend(other.access.iter().cloned());
        self.grants.extend(other.grants.iter().cloned());
    }

    fn finish(mut self, consumer: &str) -> Implied {
        self.dependencies.retain(|d| d.package != consumer);
        self.access.retain(|(provider, _)| provider != consumer);
        Implied {
            dependencies: self.dependencies,
            network: self
                .access
                .into_iter()
                .map(|(provider, access)| NetworkGrant { consumer: consumer.to_string(), provider, access })
                .collect(),
            grants_used: self.grants,
        }
    }
}

struct Implied {
    dependencies: BTreeSet<Dependency>,
    network: BTreeSet<NetworkGrant>,
    grants_used: BTreeSet<Grant>,
}

/// A package's fully resolved settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub package: String,
    /// Set values only. An optional setting with no value is absent.
    pub values: BTreeMap<String, Value>,
    /// `exec` with references substituted. Pass directly to `execve`, never to a shell.
    pub exec: Vec<String>,
    pub dependencies: BTreeSet<Dependency>,
    pub network: BTreeSet<NetworkGrant>,
    /// The grants this resolution relied on, including those along chains of references.
    pub grants_used: BTreeSet<Grant>,
}

/// Generated output and what its references imply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    pub dependencies: BTreeSet<Dependency>,
    pub network: BTreeSet<NetworkGrant>,
    pub grants_used: BTreeSet<Grant>,
}

#[derive(Debug, Clone)]
struct Evaluated {
    value: Value,
    trace: Trace,
}

impl InstalledPackage {
    fn raw_value(&self, def: &SettingDef) -> Option<SettingValue> {
        self.values.get(&def.name).cloned().or_else(|| def.default.as_ref().map(SettingValue::from))
    }

    fn templates(&self) -> impl Iterator<Item = String> + '_ {
        let settings = self.schema.settings.iter().filter_map(|def| match self.raw_value(def) {
            Some(SettingValue::Template(text)) => Some(text),
            _ => None,
        });
        settings.chain(self.manifest.exec.iter().skip(1).cloned())
    }
}

impl Installed {
    /// Which package a reference target means, as seen from `from`. A package is not its own
    /// provider, so one that both needs and provides a role resolves to some other provider.
    fn target(&self, from: &str, name: &str) -> Result<(String, Option<String>), ResolveError> {
        let providers: Vec<&String> = self
            .packages
            .iter()
            .filter(|(id, package)| id.as_str() != from && package.manifest.provides.iter().any(|role| role == name))
            .map(|(id, _)| id)
            .collect();

        if self.packages.contains_key(name) {
            // A package named like a role must not silently capture references to that role.
            return match providers.iter().find(|provider| provider.as_str() != name) {
                Some(provider) => Err(ResolveError::AmbiguousName {
                    name: name.to_string(),
                    provider: provider.to_string(),
                }),
                None => Ok((name.to_string(), None)),
            };
        }

        if let Some(bound) = self.role_bindings.get(name) {
            if providers.contains(&bound) {
                return Ok((bound.clone(), Some(name.to_string())));
            }
            return Err(ResolveError::InvalidBinding { role: name.to_string(), package: bound.clone() });
        }

        match providers.as_slice() {
            [] => Err(ResolveError::MissingProvider { consumer: from.to_string(), name: name.to_string() }),
            [only] => Ok((only.to_string(), Some(name.to_string()))),
            many => Err(ResolveError::AmbiguousProvider {
                role: name.to_string(),
                candidates: many.iter().map(|id| id.to_string()).collect(),
            }),
        }
    }

    fn package(&self, id: &str) -> Result<&InstalledPackage, ResolveError> {
        self.packages.get(id).ok_or_else(|| ResolveError::UnknownPackage(id.to_string()))
    }

    /// Resolves all of a package's settings and its command line, reporting every problem found.
    pub fn resolve(&self, consumer: &str) -> Result<Resolved, Vec<ResolveError>> {
        let package = self.package(consumer).map_err(|e| vec![e])?;
        let mut eval = Evaluator::new(self);
        let mut errors = Vec::new();
        let mut trace = Trace::default();
        let mut values = BTreeMap::new();

        // A value for a setting this version retired is ignored. Anything else unknown is refused.
        for setting in package.values.keys() {
            if package.schema.setting(setting).is_none() && !package.schema.is_retired(setting) {
                errors.push(ResolveError::UnknownSetting { package: consumer.to_string(), setting: setting.clone() });
            }
        }

        for def in &package.schema.settings {
            if package.raw_value(def).is_none() && !def.required {
                continue;
            }
            match eval.setting(consumer, &def.name) {
                Ok(evaluated) => {
                    trace.merge(&evaluated.trace);
                    values.insert(def.name.clone(), evaluated.value);
                }
                Err(error) => errors.push(error),
            }
        }

        for need in &package.manifest.needs {
            match self.target(consumer, need) {
                Ok((provider, role)) => {
                    trace.dependencies.insert(Dependency { package: provider, role });
                }
                Err(error) => errors.push(error),
            }
        }

        let mut exec = Vec::with_capacity(package.manifest.exec.len());
        for (index, arg) in package.manifest.exec.iter().enumerate() {
            if index == 0 {
                exec.push(arg.clone());
                continue;
            }
            match eval.argument(consumer, index, arg) {
                Ok((text, arg_trace)) => {
                    trace.merge(&arg_trace);
                    exec.push(text);
                }
                Err(error) => errors.push(error),
            }
        }

        if let Some(path) = self.find_package_cycle(consumer) {
            errors.push(ResolveError::PackageCycle { path });
        }

        if !errors.is_empty() {
            errors.dedup();
            return Err(errors);
        }
        let implied = trace.finish(consumer);
        Ok(Resolved {
            package: consumer.to_string(),
            values,
            exec,
            dependencies: implied.dependencies,
            network: implied.network,
            grants_used: implied.grants_used,
        })
    }

    /// Fills references inside a TOML template on behalf of a package.
    ///
    /// The template is parsed before anything is substituted and substitution happens in the
    /// parsed tree, so a value can never change the document's structure, whatever it contains.
    /// A string that is exactly one reference takes the referenced value's type, which lets
    /// `port = "$(monero-daemon.rpc-port)"` produce an integer.
    pub fn render_toml(&self, consumer: &str, template: &str) -> Result<Rendered, Vec<ResolveError>> {
        self.package(consumer).map_err(|e| vec![e])?;
        let mut table: toml::Table =
            toml::from_str(template).map_err(|e| vec![ResolveError::InvalidToml(e.message().to_string())])?;
        let mut eval = Evaluator::new(self);
        let mut trace = Trace::default();
        let mut errors = Vec::new();
        for (key, value) in table.iter_mut() {
            eval.toml_node(consumer, key, value, &mut trace, &mut errors);
        }
        if !errors.is_empty() {
            errors.dedup();
            return Err(errors);
        }
        let text = toml::to_string(&table).map_err(|e| vec![ResolveError::InvalidToml(e.to_string())])?;
        if text.len() > MAX_TOML_OUTPUT {
            return Err(vec![ResolveError::OutputTooLarge]);
        }
        let implied = trace.finish(consumer);
        Ok(Rendered { text, dependencies: implied.dependencies, network: implied.network, grants_used: implied.grants_used })
    }

    /// Finds a dependency loop reachable from a package. This looks at every setting of every
    /// reached package, not only those evaluated, because the supervisor starts whole packages
    /// and a loop between them can never come up even when no single setting forms a cycle.
    fn find_package_cycle(&self, start: &str) -> Option<Vec<String>> {
        let mut done = BTreeSet::new();
        let mut path = Vec::new();
        self.walk_packages(start, &mut done, &mut path)
    }

    fn walk_packages(
        &self,
        id: &str,
        done: &mut BTreeSet<String>,
        path: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        if let Some(at) = path.iter().position(|p| p == id) {
            let mut cycle = path[at..].to_vec();
            cycle.push(id.to_string());
            return Some(cycle);
        }
        if !done.insert(id.to_string()) {
            return None;
        }
        path.push(id.to_string());
        for next in self.package_edges(id) {
            if let Some(cycle) = self.walk_packages(&next, done, path) {
                return Some(cycle);
            }
        }
        path.pop();
        None
    }

    /// Unresolvable references are skipped: they are reported when the setting is evaluated, or
    /// when that other package is resolved.
    fn package_edges(&self, id: &str) -> BTreeSet<String> {
        let mut edges = BTreeSet::new();
        let Some(package) = self.packages.get(id) else { return edges };
        let mut targets: Vec<String> = package.manifest.needs.clone();
        for text in package.templates() {
            for part in parse_template(&text).unwrap_or_default() {
                if let Part::Reference(reference) = part {
                    targets.push(reference.target);
                }
            }
        }
        for target in targets {
            if let Ok((provider, _)) = self.target(id, &target) {
                if provider != id {
                    edges.insert(provider);
                }
            }
        }
        edges
    }
}

struct Evaluator<'a> {
    installed: &'a Installed,
    memo: BTreeMap<(String, String), Result<Evaluated, ResolveError>>,
    stack: Vec<(String, String)>,
}

impl<'a> Evaluator<'a> {
    fn new(installed: &'a Installed) -> Self {
        Evaluator { installed, memo: BTreeMap::new(), stack: Vec::new() }
    }

    fn setting(&mut self, package: &str, setting: &str) -> Result<Evaluated, ResolveError> {
        let key = (package.to_string(), setting.to_string());
        if let Some(found) = self.memo.get(&key) {
            return found.clone();
        }
        if let Some(at) = self.stack.iter().position(|entry| *entry == key) {
            let mut path: Vec<String> = self.stack[at..].iter().map(|(p, s)| format!("{p}.{s}")).collect();
            path.push(format!("{package}.{setting}"));
            return Err(ResolveError::Cycle { path });
        }
        if self.stack.len() >= MAX_REFERENCE_DEPTH {
            return Err(ResolveError::TooDeep { package: package.to_string(), setting: setting.to_string() });
        }
        self.stack.push(key.clone());
        let result = self.compute(package, setting);
        self.stack.pop();
        self.memo.insert(key, result.clone());
        result
    }

    fn compute(&mut self, package_id: &str, setting: &str) -> Result<Evaluated, ResolveError> {
        let package = self.installed.package(package_id)?;
        let def = package.schema.setting(setting).ok_or_else(|| ResolveError::MissingSetting {
            provider: package_id.to_string(),
            setting: setting.to_string(),
        })?;
        let raw = package.raw_value(def).ok_or_else(|| ResolveError::MissingValue {
            package: package_id.to_string(),
            setting: setting.to_string(),
        })?;
        let invalid = |problem: String| ResolveError::InvalidValue {
            package: package_id.to_string(),
            setting: setting.to_string(),
            problem,
        };

        let mut evaluated = match raw {
            SettingValue::Literal(value) => {
                def.ty.check(&value).map_err(invalid)?;
                Evaluated { value, trace: Trace::default() }
            }
            SettingValue::Template(text) => {
                let parts = parse_template(&text).map_err(|error| ResolveError::Template {
                    package: package_id.to_string(),
                    setting: setting.to_string(),
                    error,
                })?;
                self.template(package_id, def, &parts)?
            }
        };

        if let Some(endpoint) = def.endpoint {
            let access = endpoint_access(package_id, package, def, endpoint, &evaluated.value)?;
            evaluated.trace.access.insert((package_id.to_string(), access));
        }
        Ok(evaluated)
    }

    fn template(&mut self, package: &str, def: &SettingDef, parts: &[Part]) -> Result<Evaluated, ResolveError> {
        let invalid = |problem: String| ResolveError::InvalidValue {
            package: package.to_string(),
            setting: def.name.clone(),
            problem,
        };

        if let Some(literal) = literal_text(parts) {
            let value = def.ty.parse_literal(&literal).map_err(invalid)?;
            return Ok(Evaluated { value, trace: Trace::default() });
        }

        let mismatch = |reference: &Reference, found: &str| ResolveError::TypeMismatch {
            package: package.to_string(),
            setting: def.name.clone(),
            reference: reference.to_string(),
            expected: def.ty.name().to_string(),
            found: found.to_string(),
        };

        if let [Part::Reference(reference)] = parts {
            let source = self.reference(package, reference)?;
            let found = source.value.type_name();
            let value = convert(source.value, &def.ty).ok_or_else(|| mismatch(reference, found))?;
            def.ty.check(&value).map_err(invalid)?;
            return Ok(Evaluated { value, trace: source.trace });
        }

        // Text assembled from several pieces. Types that are not free text cannot be assembled.
        if !matches!(def.ty, SettingType::String | SettingType::Url | SettingType::Secret) {
            let reference = parts.iter().find_map(|part| match part {
                Part::Reference(reference) => Some(reference),
                Part::Literal(_) => None,
            });
            return Err(mismatch(reference.expect("parts without references were handled above"), "assembled text"));
        }
        let mut text = String::new();
        let mut trace = Trace::default();
        for part in parts {
            match part {
                Part::Literal(literal) => text.push_str(literal),
                Part::Reference(reference) => {
                    let source = self.reference(package, reference)?;
                    if matches!(source.value, Value::Secret(_)) && !matches!(def.ty, SettingType::Secret) {
                        return Err(mismatch(reference, "secret"));
                    }
                    text.push_str(&source.value.text());
                    trace.merge(&source.trace);
                }
            }
            // Checked as it grows: settings that each repeat the previous one twice would
            // otherwise double in size at every level of a chain.
            if text.len() > MAX_TEXT_LEN {
                return Err(invalid(format!("would be longer than {MAX_TEXT_LEN} bytes")));
            }
        }
        let value = match def.ty {
            SettingType::Url => Value::Url(text),
            SettingType::Secret => Value::Secret(SecretString::new(text)),
            _ => Value::String(text),
        };
        def.ty.check(&value).map_err(invalid)?;
        Ok(Evaluated { value, trace })
    }

    /// Follows one reference, enforcing access rules before the value is ever read.
    fn reference(&mut self, from: &str, reference: &Reference) -> Result<Evaluated, ResolveError> {
        let (provider_id, role) = self.installed.target(from, &reference.target)?;
        let provider = self.installed.package(&provider_id)?;
        let missing = || ResolveError::MissingSetting {
            provider: provider_id.clone(),
            setting: reference.setting.clone(),
        };

        let mut trace = Trace::default();
        let def = if provider_id == from {
            provider
                .schema
                .exported(&reference.setting)
                .or_else(|| provider.schema.setting(&reference.setting))
                .ok_or_else(missing)?
        } else if let Some(def) = provider.schema.exported(&reference.setting) {
            // Public names cannot belong to a secret, as loading enforces, but a schema may also
            // arrive deserialized, so this stays checked here.
            if matches!(def.ty, SettingType::Secret) {
                self.require_grant(from, &provider_id, def, &mut trace)?;
            }
            def
        } else {
            let def = provider.schema.setting(&reference.setting).ok_or_else(missing)?;
            self.require_grant(from, &provider_id, def, &mut trace)?;
            def
        };

        let source = self.setting(&provider_id, &def.name)?;
        trace.merge(&source.trace);
        if provider_id != from {
            trace.dependencies.insert(Dependency { package: provider_id, role });
        }
        Ok(Evaluated { value: source.value, trace })
    }

    fn require_grant(
        &self,
        from: &str,
        provider: &str,
        def: &SettingDef,
        trace: &mut Trace,
    ) -> Result<(), ResolveError> {
        let grant = Grant {
            consumer: from.to_string(),
            provider: provider.to_string(),
            setting: def.name.clone(),
        };
        if !self.installed.grants.contains(&grant) {
            return Err(ResolveError::GrantRequired {
                consumer: grant.consumer,
                provider: grant.provider,
                setting: grant.setting,
                secret: matches!(def.ty, SettingType::Secret),
            });
        }
        trace.grants.insert(grant);
        Ok(())
    }

    fn argument(&mut self, package: &str, index: usize, arg: &str) -> Result<(String, Trace), ResolveError> {
        let parts = parse_template(arg).map_err(|error| ResolveError::Template {
            package: package.to_string(),
            setting: format!("exec[{index}]"),
            error,
        })?;
        let mut text = String::new();
        let mut trace = Trace::default();
        for (position, part) in parts.iter().enumerate() {
            match part {
                Part::Literal(literal) => text.push_str(literal),
                Part::Reference(reference) => {
                    let source = self.reference(package, reference)?;
                    if matches!(source.value, Value::Secret(_)) {
                        return Err(ResolveError::SecretInArgument { package: package.to_string(), index });
                    }
                    let value = source.value.text();
                    if position == 0 && value.starts_with('-') {
                        return Err(ResolveError::OptionInjection { package: package.to_string(), index });
                    }
                    text.push_str(&value);
                    trace.merge(&source.trace);
                }
            }
        }
        Ok((text, trace))
    }

    fn toml_node(
        &mut self,
        consumer: &str,
        key: &str,
        node: &mut toml::Value,
        trace: &mut Trace,
        errors: &mut Vec<ResolveError>,
    ) {
        if key.contains("$(") {
            errors.push(ResolveError::ReferenceInKey(key.chars().filter(|c| !c.is_control()).take(64).collect()));
        }
        match node {
            toml::Value::String(text) => match self.toml_string(consumer, key, text) {
                Ok((value, value_trace)) => {
                    *node = value;
                    trace.merge(&value_trace);
                }
                Err(error) => errors.push(error),
            },
            toml::Value::Array(items) => {
                for item in items {
                    self.toml_node(consumer, key, item, trace, errors);
                }
            }
            toml::Value::Table(table) => {
                for (child_key, child) in table.iter_mut() {
                    self.toml_node(consumer, child_key, child, trace, errors);
                }
            }
            _ => {}
        }
    }

    fn toml_string(&mut self, consumer: &str, key: &str, text: &str) -> Result<(toml::Value, Trace), ResolveError> {
        let parts = parse_template(text).map_err(|error| ResolveError::Template {
            package: consumer.to_string(),
            setting: key.chars().filter(|c| !c.is_control()).take(64).collect(),
            error,
        })?;
        if let [Part::Reference(reference)] = parts.as_slice() {
            let source = self.reference(consumer, reference)?;
            return Ok((toml_value(&source.value), source.trace));
        }
        let mut out = String::new();
        let mut trace = Trace::default();
        for part in &parts {
            match part {
                Part::Literal(literal) => out.push_str(literal),
                Part::Reference(reference) => {
                    let source = self.reference(consumer, reference)?;
                    out.push_str(&source.value.text());
                    trace.merge(&source.trace);
                }
            }
        }
        if out.len() > MAX_TOML_OUTPUT {
            return Err(ResolveError::OutputTooLarge);
        }
        Ok((toml::Value::String(out), trace))
    }
}

/// Exact references keep their type. Anything can widen into text, except that a secret can only
/// ever be held by another secret.
fn convert(value: Value, target: &SettingType) -> Option<Value> {
    match (target, &value) {
        (SettingType::Secret, Value::Secret(_)) => Some(value),
        (SettingType::Secret, _) => Some(Value::Secret(SecretString::new(value.text()))),
        (_, Value::Secret(_)) => None,
        (SettingType::String, _) => Some(Value::String(value.text())),
        (SettingType::Int { .. }, Value::Int(_))
        | (SettingType::Bool, Value::Bool(_))
        | (SettingType::Enum { .. }, Value::Enum(_))
        | (SettingType::MoneroAddress, Value::MoneroAddress(_))
        | (SettingType::Url, Value::Url(_)) => Some(value),
        _ => None,
    }
}

/// Works out what a reference to an endpoint setting opens. The destination is always the package
/// that declared the setting and only what its manifest declares, so a package marking a setting
/// as an endpoint cannot cause access to anything but itself.
fn endpoint_access(
    id: &str,
    package: &InstalledPackage,
    def: &SettingDef,
    endpoint: Endpoint,
    value: &Value,
) -> Result<Access, ResolveError> {
    let invalid = || ResolveError::InvalidEndpoint { package: id.to_string(), setting: def.name.clone() };
    match endpoint {
        Endpoint::Unix => {
            let text = match value {
                Value::String(text) => text,
                _ => return Err(invalid()),
            };
            let inner = text.strip_prefix(&format!("unix:/run/apps/{id}/")).ok_or_else(invalid)?;
            if !is_valid_package_path(inner) || !inner.ends_with(".sock") {
                return Err(invalid());
            }
            Ok(Access::UnixSocket { path: format!("/run/apps/{id}/{inner}") })
        }
        Endpoint::Tcp | Endpoint::Udp => {
            let port = match value {
                Value::Int(number) => *number,
                Value::Url(url) => {
                    let parsed = check_url(url).map_err(|_| invalid())?;
                    if !parsed.is_loopback() {
                        return Err(invalid());
                    }
                    i64::from(parsed.effective_port())
                }
                _ => return Err(invalid()),
            };
            let wanted = if endpoint == Endpoint::Tcp { Protocol::Tcp } else { Protocol::Udp };
            let declared = u16::try_from(port)
                .ok()
                .filter(|port| {
                    package.manifest.network.inbound.iter().any(|i| i.port == *port && i.protocol == wanted)
                })
                .ok_or_else(|| ResolveError::UndeclaredEndpoint {
                    package: id.to_string(),
                    setting: def.name.clone(),
                    port,
                })?;
            let protocol = if endpoint == Endpoint::Tcp { PortProtocol::Tcp } else { PortProtocol::Udp };
            Ok(Access::Port { protocol, port: declared })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monero::encode_address;
    use crate::schema::DefaultValue;
    use crate::template::escape_literal;

    fn setting(name: &str, ty: SettingType) -> SettingDef {
        SettingDef {
            name: name.into(),
            ty,
            label: name.into(),
            help: None,
            required: false,
            default: None,
            public: None,
            endpoint: None,
            has_validator: false,
            generate: None,
        }
    }

    const INT: SettingType = SettingType::Int { min: None, max: None };

    fn text_default(mut def: SettingDef, text: &str) -> SettingDef {
        def.default = Some(DefaultValue::Text(text.into()));
        def
    }

    fn int_default(mut def: SettingDef, number: i64) -> SettingDef {
        def.default = Some(DefaultValue::Int(number));
        def
    }

    fn public(mut def: SettingDef, name: &str) -> SettingDef {
        def.public = Some(name.into());
        def
    }

    fn endpoint(mut def: SettingDef, kind: Endpoint) -> SettingDef {
        def.endpoint = Some(kind);
        def
    }

    fn package(id: &str, provides: &[&str], settings: Vec<SettingDef>) -> InstalledPackage {
        // Fixtures use system ids such as cuprated, which feed manifests may not claim.
        let manifest = Manifest::parse_system(&format!(
            "api_version = 1\nid = \"{id}\"\nversion = \"1.0\"\nexec = [\"bin/run\"]\n"
        ))
        .unwrap();
        let mut package = InstalledPackage {
            manifest,
            schema: Schema { api_version: 1, settings, retired: Vec::new() },
            values: BTreeMap::new(),
        };
        package.manifest.provides = provides.iter().map(|s| s.to_string()).collect();
        package
    }

    fn with_inbound(mut package: InstalledPackage, port: u16) -> InstalledPackage {
        package.manifest.network.inbound.push(nodo_manifest::Inbound {
            port,
            protocol: Protocol::Tcp,
            scope: nodo_manifest::Scope::Local,
        });
        package
    }

    fn world(packages: Vec<InstalledPackage>) -> Installed {
        Installed {
            packages: packages.into_iter().map(|p| (p.manifest.id.clone(), p)).collect(),
            ..Installed::default()
        }
    }

    fn cuprated() -> InstalledPackage {
        with_inbound(
            package(
                "cuprated",
                &["monero-daemon"],
                vec![
                    endpoint(public(int_default(setting("rpc_port", INT), 18081), "rpc-port"), Endpoint::Tcp),
                    endpoint(
                        public(text_default(setting("url", SettingType::Url), "http://127.0.0.1:18081/"), "rpc-url"),
                        Endpoint::Tcp,
                    ),
                    public(text_default(setting("name", SettingType::String), "cuprate"), "name"),
                    text_default(setting("hidden", SettingType::String), "private"),
                    setting("login", SettingType::Secret),
                ],
            ),
            18081,
        )
    }

    fn consumer(default: &str, ty: SettingType) -> InstalledPackage {
        package("shop", &[], vec![text_default(setting("daemon", ty), default)])
    }

    fn secret_value(text: &str) -> SettingValue {
        SettingValue::Literal(Value::Secret(SecretString::new(text)))
    }

    fn only_error(installed: &Installed, id: &str) -> ResolveError {
        let mut errors = installed.resolve(id).unwrap_err();
        assert_eq!(errors.len(), 1, "{errors:?}");
        errors.remove(0)
    }

    fn grant(consumer: &str, provider: &str, setting: &str) -> Grant {
        Grant { consumer: consumer.into(), provider: provider.into(), setting: setting.into() }
    }

    #[test]
    fn resolves_through_a_role() {
        let installed = world(vec![cuprated(), consumer("$(monero-daemon.rpc-url)", SettingType::Url)]);
        let resolved = installed.resolve("shop").unwrap();
        assert_eq!(resolved.values["daemon"], Value::Url("http://127.0.0.1:18081/".into()));
        assert_eq!(
            resolved.dependencies.into_iter().collect::<Vec<_>>(),
            [Dependency { package: "cuprated".into(), role: Some("monero-daemon".into()) }]
        );
        assert_eq!(
            resolved.network.into_iter().collect::<Vec<_>>(),
            [NetworkGrant {
                consumer: "shop".into(),
                provider: "cuprated".into(),
                access: Access::Port { protocol: PortProtocol::Tcp, port: 18081 },
            }]
        );
    }

    #[test]
    fn the_role_follows_whichever_package_provides_it() {
        let mut monerod = cuprated();
        monerod.manifest.id = "monerod".into();
        monerod.values.insert("name".into(), SettingValue::Literal(Value::String("monerod".into())));
        let installed = world(vec![monerod, consumer("$(monero-daemon.name)", SettingType::String)]);
        let resolved = installed.resolve("shop").unwrap();
        assert_eq!(resolved.values["daemon"], Value::String("monerod".into()));
    }

    #[test]
    fn references_a_package_directly_by_id() {
        let installed = world(vec![cuprated(), consumer("$(cuprated.name)", SettingType::String)]);
        let resolved = installed.resolve("shop").unwrap();
        assert_eq!(resolved.values["daemon"], Value::String("cuprate".into()));
        assert_eq!(
            resolved.dependencies.into_iter().collect::<Vec<_>>(),
            [Dependency { package: "cuprated".into(), role: None }]
        );
        assert!(resolved.network.is_empty(), "a non endpoint setting opens no access");
    }

    #[test]
    fn missing_provider_is_reported() {
        let installed = world(vec![consumer("$(monero-daemon.rpc-url)", SettingType::Url)]);
        assert_eq!(
            only_error(&installed, "shop"),
            ResolveError::MissingProvider { consumer: "shop".into(), name: "monero-daemon".into() }
        );
    }

    #[test]
    fn manifest_needs_require_a_provider_and_imply_a_dependency() {
        let mut shop = package("shop", &[], vec![]);
        shop.manifest.needs = vec!["monero-daemon".into()];
        let installed = world(vec![shop.clone()]);
        assert!(matches!(only_error(&installed, "shop"), ResolveError::MissingProvider { .. }));
        let installed = world(vec![shop, cuprated()]);
        let resolved = installed.resolve("shop").unwrap();
        assert_eq!(resolved.dependencies.len(), 1);
    }

    #[test]
    fn several_providers_need_an_explicit_choice() {
        let mut monerod = cuprated();
        monerod.manifest.id = "monerod".into();
        monerod.values.insert("name".into(), SettingValue::Literal(Value::String("monerod".into())));
        let mut installed = world(vec![cuprated(), monerod, consumer("$(monero-daemon.name)", SettingType::String)]);
        assert_eq!(
            only_error(&installed, "shop"),
            ResolveError::AmbiguousProvider {
                role: "monero-daemon".into(),
                candidates: vec!["cuprated".into(), "monerod".into()],
            }
        );
        installed.role_bindings.insert("monero-daemon".into(), "monerod".into());
        assert_eq!(installed.resolve("shop").unwrap().values["daemon"], Value::String("monerod".into()));
        installed.role_bindings.insert("monero-daemon".into(), "shop".into());
        assert!(matches!(only_error(&installed, "shop"), ResolveError::InvalidBinding { .. }));
    }

    #[test]
    fn a_package_named_like_a_role_cannot_capture_it() {
        let mut impostor = package("impostor", &[], vec![public(text_default(setting("name", SettingType::String), "evil"), "name")]);
        impostor.manifest.id = "monero-daemon".into();
        let installed = world(vec![cuprated(), impostor, consumer("$(monero-daemon.name)", SettingType::String)]);
        assert!(matches!(only_error(&installed, "shop"), ResolveError::AmbiguousName { .. }));
    }

    #[test]
    fn a_package_that_proxies_its_own_role_resolves_to_another_provider() {
        let mut proxy = package(
            "proxy",
            &["monero-daemon"],
            vec![public(text_default(setting("name", SettingType::String), "$(monero-daemon.name)"), "name")],
        );
        proxy.manifest.needs = vec!["monero-daemon".into()];
        let installed = world(vec![cuprated(), proxy.clone()]);
        let resolved = installed.resolve("proxy").unwrap();
        assert_eq!(resolved.values["name"], Value::String("cuprate".into()));
        let installed = world(vec![proxy]);
        assert!(installed.resolve("proxy").is_err(), "it is not its own provider");
    }

    #[test]
    fn refuses_unknown_settings_on_the_provider() {
        let installed = world(vec![cuprated(), consumer("$(cuprated.nope)", SettingType::String)]);
        assert_eq!(
            only_error(&installed, "shop"),
            ResolveError::MissingSetting { provider: "cuprated".into(), setting: "nope".into() }
        );
    }

    #[test]
    fn refuses_values_for_unknown_settings() {
        let mut shop = package("shop", &[], vec![]);
        shop.values.insert("ghost".into(), SettingValue::Literal(Value::Int(1)));
        assert!(matches!(only_error(&world(vec![shop]), "shop"), ResolveError::UnknownSetting { .. }));
    }

    #[test]
    fn values_for_retired_settings_are_ignored_and_others_still_refused() {
        let mut shop = package("shop", &[], vec![int_default(setting("port", INT), 1)]);
        shop.schema.retired = vec!["old_switch".into()];
        shop.values.insert("old_switch".into(), SettingValue::Literal(Value::Bool(true)));
        let resolved = world(vec![shop.clone()]).resolve("shop").unwrap();
        assert!(!resolved.values.contains_key("old_switch"));
        assert_eq!(resolved.values["port"], Value::Int(1));
        shop.values.insert("ghost".into(), SettingValue::Literal(Value::Int(1)));
        assert_eq!(
            only_error(&world(vec![shop]), "shop"),
            ResolveError::UnknownSetting { package: "shop".into(), setting: "ghost".into() }
        );
    }

    #[test]
    fn non_public_settings_need_a_grant() {
        let mut installed = world(vec![cuprated(), consumer("$(cuprated.hidden)", SettingType::String)]);
        assert_eq!(
            only_error(&installed, "shop"),
            ResolveError::GrantRequired {
                consumer: "shop".into(),
                provider: "cuprated".into(),
                setting: "hidden".into(),
                secret: false,
            }
        );
        installed.grants.insert(grant("shop", "cuprated", "hidden"));
        let resolved = installed.resolve("shop").unwrap();
        assert_eq!(resolved.values["daemon"], Value::String("private".into()));
        assert!(resolved.grants_used.contains(&grant("shop", "cuprated", "hidden")));
    }

    #[test]
    fn a_public_settings_internal_name_is_still_private() {
        let installed = world(vec![cuprated(), consumer("$(cuprated.rpc_port)", SettingType::Secret)]);
        assert!(matches!(only_error(&installed, "shop"), ResolveError::GrantRequired { .. }));
    }

    #[test]
    fn secrets_need_a_grant() {
        let mut daemon = cuprated();
        daemon.values.insert("login".into(), secret_value("hunter2"));
        let mut installed = world(vec![daemon, consumer("$(cuprated.login)", SettingType::Secret)]);
        assert!(matches!(
            only_error(&installed, "shop"),
            ResolveError::GrantRequired { secret: true, .. }
        ));
        installed.grants.insert(grant("shop", "cuprated", "login"));
        let resolved = installed.resolve("shop").unwrap();
        assert_eq!(resolved.values["daemon"], Value::Secret(SecretString::new("hunter2")));
    }

    #[test]
    fn grants_are_exact() {
        let mut daemon = cuprated();
        daemon.values.insert("login".into(), secret_value("hunter2"));
        for wrong in [
            grant("other", "cuprated", "login"),
            grant("shop", "other", "login"),
            grant("shop", "cuprated", "hidden"),
            grant("shop", "monero-daemon", "login"),
        ] {
            let mut installed = world(vec![daemon.clone(), consumer("$(monero-daemon.login)", SettingType::Secret)]);
            installed.grants.insert(wrong.clone());
            assert!(
                matches!(only_error(&installed, "shop"), ResolveError::GrantRequired { .. }),
                "{wrong:?} must not satisfy the reference"
            );
        }
    }

    #[test]
    fn a_secret_grant_does_not_follow_the_role_to_another_provider() {
        let mut replacement = cuprated();
        replacement.manifest.id = "monerod".into();
        replacement.values.insert("login".into(), secret_value("other"));
        let mut installed = world(vec![replacement, consumer("$(monero-daemon.login)", SettingType::Secret)]);
        installed.grants.insert(grant("shop", "cuprated", "login"));
        assert!(matches!(only_error(&installed, "shop"), ResolveError::GrantRequired { .. }));
    }

    #[test]
    fn a_deserialized_schema_with_a_public_secret_still_needs_a_grant() {
        let mut daemon = package("noded", &[], vec![public(setting("key", SettingType::Secret), "key")]);
        daemon.values.insert("key".into(), secret_value("hunter2"));
        let installed = world(vec![daemon, consumer("$(noded.key)", SettingType::Secret)]);
        assert!(matches!(only_error(&installed, "shop"), ResolveError::GrantRequired { secret: true, .. }));
    }

    #[test]
    fn secrets_cannot_flow_into_other_types() {
        let mut daemon = cuprated();
        daemon.values.insert("login".into(), secret_value("hunter2"));
        for (template, ty) in [
            ("$(cuprated.login)", SettingType::String),
            ("password=$(cuprated.login)", SettingType::String),
            ("http://$(cuprated.login)/", SettingType::Url),
            ("$(cuprated.login)", SettingType::Url),
        ] {
            let mut installed = world(vec![daemon.clone(), consumer(template, ty)]);
            installed.grants.insert(grant("shop", "cuprated", "login"));
            let error = only_error(&installed, "shop");
            assert!(matches!(error, ResolveError::TypeMismatch { .. }), "{template}: {error:?}");
            assert!(!error.to_string().contains("hunter2"));
        }
    }

    #[test]
    fn a_granted_secret_cannot_be_laundered_through_a_public_setting() {
        let mut daemon = cuprated();
        daemon.values.insert("login".into(), secret_value("hunter2"));
        let mut middle = package(
            "middle",
            &[],
            vec![public(text_default(setting("relay", SettingType::String), "$(cuprated.login)"), "relay")],
        );
        middle.values.clear();
        let third = package("third", &[], vec![text_default(setting("x", SettingType::String), "$(middle.relay)")]);
        let mut installed = world(vec![daemon, middle, third]);
        installed.grants.insert(grant("middle", "cuprated", "login"));
        let errors = installed.resolve("third").unwrap_err();
        assert!(errors.iter().any(|e| matches!(e, ResolveError::TypeMismatch { .. })), "{errors:?}");
    }

    #[test]
    fn grants_apply_to_the_package_holding_the_reference() {
        // third may read middle.relay, but middle has no approval to read the hidden setting.
        let middle = package(
            "middle",
            &[],
            vec![public(text_default(setting("relay", SettingType::String), "$(cuprated.hidden)"), "relay")],
        );
        let third = package("third", &[], vec![text_default(setting("x", SettingType::String), "$(middle.relay)")]);
        let mut installed = world(vec![cuprated(), middle, third]);
        installed.grants.insert(grant("third", "cuprated", "hidden"));
        assert!(matches!(
            only_error(&installed, "third"),
            ResolveError::GrantRequired { consumer, .. } if consumer == "middle"
        ));
    }

    #[test]
    fn a_package_reads_its_own_settings_without_a_grant() {
        let mut shop = package(
            "shop",
            &[],
            vec![setting("key", SettingType::Secret), text_default(setting("copy", SettingType::Secret), "$(shop.key)")],
        );
        shop.values.insert("key".into(), secret_value("hunter2"));
        let resolved = world(vec![shop]).resolve("shop").unwrap();
        assert_eq!(resolved.values["copy"], Value::Secret(SecretString::new("hunter2")));
        assert!(resolved.dependencies.is_empty());
    }

    #[test]
    fn type_mismatches_are_refused() {
        let address = encode_address(18, 64);
        let mut daemon = cuprated();
        daemon.schema.settings.push(public(text_default(setting("addr", SettingType::MoneroAddress), &address), "addr"));
        for (template, ty) in [
            ("$(cuprated.name)", INT),
            ("$(cuprated.name)", SettingType::Bool),
            ("$(cuprated.name)", SettingType::Url),
            ("$(cuprated.name)", SettingType::MoneroAddress),
            ("$(cuprated.name)", SettingType::Enum { choices: vec!["cuprate".into()] }),
            ("$(cuprated.rpc-port)", SettingType::Url),
            ("$(cuprated.rpc-url)", INT),
            ("$(cuprated.addr)", SettingType::Url),
            ("1$(cuprated.rpc-port)", INT),
            ("$(cuprated.rpc-port)$(cuprated.rpc-port)", INT),
            ("x$(cuprated.name)", SettingType::Bool),
        ] {
            let installed = world(vec![daemon.clone(), consumer(template, ty.clone())]);
            let error = only_error(&installed, "shop");
            assert!(matches!(error, ResolveError::TypeMismatch { .. }), "{template} into {}: {error:?}", ty.name());
        }
    }

    #[test]
    fn compatible_references_convert() {
        for (template, ty, expected) in [
            ("$(cuprated.rpc-port)", INT, Value::Int(18081)),
            ("$(cuprated.rpc-port)", SettingType::String, Value::String("18081".into())),
            ("$(cuprated.rpc-url)", SettingType::String, Value::String("http://127.0.0.1:18081/".into())),
            ("$(cuprated.name)", SettingType::Secret, Value::Secret(SecretString::new("cuprate"))),
            ("http://127.0.0.1:$(cuprated.rpc-port)/x", SettingType::Url, Value::Url("http://127.0.0.1:18081/x".into())),
            ("<$(cuprated.name)>", SettingType::String, Value::String("<cuprate>".into())),
        ] {
            let installed = world(vec![cuprated(), consumer(template, ty)]);
            assert_eq!(installed.resolve("shop").unwrap().values["daemon"], expected, "{template}");
        }
    }

    #[test]
    fn the_result_must_satisfy_the_target_type() {
        for (template, ty) in [
            ("$(cuprated.rpc-port)", SettingType::Int { min: Some(20000), max: None }),
            ("not a url $(cuprated.name)", SettingType::Url),
        ] {
            let installed = world(vec![cuprated(), consumer(template, ty)]);
            let error = only_error(&installed, "shop");
            assert!(matches!(error, ResolveError::InvalidValue { .. }), "{template}: {error:?}");
        }
    }

    #[test]
    fn detects_setting_cycles() {
        let a = package("alpha", &[], vec![public(text_default(setting("x", SettingType::String), "$(beta.y)"), "x")]);
        let b = package("beta", &[], vec![public(text_default(setting("y", SettingType::String), "$(alpha.x)"), "y")]);
        let errors = world(vec![a, b]).resolve("alpha").unwrap_err();
        let cycle = errors.iter().find_map(|e| match e {
            ResolveError::Cycle { path } => Some(path),
            _ => None,
        });
        assert_eq!(cycle.unwrap(), &["alpha.x", "beta.y", "alpha.x"]);
    }

    #[test]
    fn detects_self_reference() {
        let a = package("alpha", &[], vec![text_default(setting("x", SettingType::String), "$(alpha.x)")]);
        assert!(matches!(only_error(&world(vec![a]), "alpha"), ResolveError::Cycle { .. }));
    }

    #[test]
    fn detects_package_cycles_without_setting_cycles() {
        // No setting depends on itself, but neither package could ever start first.
        let a = package(
            "alpha",
            &[],
            vec![
                public(text_default(setting("a1", SettingType::String), "one"), "a1"),
                text_default(setting("a2", SettingType::String), "$(beta.b1)"),
            ],
        );
        let b = package(
            "beta",
            &[],
            vec![
                public(text_default(setting("b1", SettingType::String), "two"), "b1"),
                text_default(setting("b2", SettingType::String), "$(alpha.a1)"),
            ],
        );
        let installed = world(vec![a, b]);
        assert_eq!(
            only_error(&installed, "alpha"),
            ResolveError::PackageCycle { path: vec!["alpha".into(), "beta".into(), "alpha".into()] }
        );
    }

    #[test]
    fn detects_package_cycles_through_needs() {
        let mut a = package("alpha", &["role-a"], vec![]);
        a.manifest.needs = vec!["role-b".into()];
        let mut b = package("beta", &["role-b"], vec![]);
        b.manifest.needs = vec!["role-a".into()];
        assert!(matches!(only_error(&world(vec![a, b]), "alpha"), ResolveError::PackageCycle { .. }));
    }

    #[test]
    fn a_shared_dependency_is_not_a_cycle() {
        // A diamond visits the base twice, which must not look like a loop.
        let base = package("base", &[], vec![public(text_default(setting("v", SettingType::String), "x"), "v")]);
        let left = package("left", &[], vec![public(text_default(setting("v", SettingType::String), "$(base.v)"), "v")]);
        let right = package("right", &[], vec![public(text_default(setting("v", SettingType::String), "$(base.v)"), "v")]);
        let top = package("top", &[], vec![text_default(setting("v", SettingType::String), "$(left.v)$(right.v)")]);
        let resolved = world(vec![base, left, right, top]).resolve("top").unwrap();
        assert_eq!(resolved.values["v"], Value::String("xx".into()));
        assert_eq!(resolved.dependencies.len(), 3, "transitive dependencies are included");
    }

    #[test]
    fn long_chains_stop_at_the_depth_limit() {
        let count = MAX_REFERENCE_DEPTH + 8;
        let mut settings: Vec<SettingDef> = (0..count)
            .map(|i| text_default(setting(&format!("s{i}"), SettingType::String), &format!("$(deep.s{})", i + 1)))
            .collect();
        settings.push(text_default(setting(&format!("s{count}"), SettingType::String), "end"));
        let errors = world(vec![package("deep", &[], settings)]).resolve("deep").unwrap_err();
        assert!(errors.iter().any(|e| matches!(e, ResolveError::TooDeep { .. })), "{errors:?}");
    }

    #[test]
    fn exponential_expansion_is_stopped() {
        let levels = 30;
        let mut settings = vec![text_default(setting("s0", SettingType::String), "xxxxxxxxxxxxxxxx")];
        settings.extend((1..levels).map(|i| {
            let previous = format!("$(boom.s{})", i - 1);
            text_default(setting(&format!("s{i}"), SettingType::String), &format!("{previous}{previous}"))
        }));
        let errors = world(vec![package("boom", &[], settings)]).resolve("boom").unwrap_err();
        assert!(errors.iter().any(|e| matches!(e, ResolveError::InvalidValue { .. })), "{errors:?}");
    }

    #[test]
    fn missing_required_and_referenced_values_are_errors() {
        let mut required = setting("must", SettingType::String);
        required.required = true;
        let shop = package("shop", &[], vec![required, setting("optional", SettingType::String)]);
        let installed = world(vec![shop]);
        assert_eq!(
            only_error(&installed, "shop"),
            ResolveError::MissingValue { package: "shop".into(), setting: "must".into() }
        );

        let daemon = package("noded", &[], vec![public(setting("unset", SettingType::String), "unset")]);
        let installed = world(vec![daemon, consumer("$(noded.unset)", SettingType::String)]);
        assert_eq!(
            only_error(&installed, "shop"),
            ResolveError::MissingValue { package: "noded".into(), setting: "unset".into() }
        );
    }

    #[test]
    fn optional_unset_settings_are_absent() {
        let shop = package("shop", &[], vec![setting("optional", SettingType::String)]);
        assert!(world(vec![shop]).resolve("shop").unwrap().values.is_empty());
    }

    #[test]
    fn user_values_override_defaults_and_are_type_checked() {
        let mut shop = consumer("default", SettingType::String);
        shop.values.insert("daemon".into(), SettingValue::Literal(Value::String("chosen".into())));
        let resolved = world(vec![shop.clone()]).resolve("shop").unwrap();
        assert_eq!(resolved.values["daemon"], Value::String("chosen".into()));

        shop.values.insert("daemon".into(), SettingValue::Literal(Value::Int(5)));
        assert!(matches!(only_error(&world(vec![shop]), "shop"), ResolveError::InvalidValue { .. }));
    }

    #[test]
    fn reports_every_problem_at_once() {
        let shop = package(
            "shop",
            &[],
            vec![
                text_default(setting("a", SettingType::String), "$(nothing.x)"),
                text_default(setting("b", SettingType::String), "$(unterminated"),
                text_default(setting("c", SettingType::String), "$(alsomissing.y)"),
            ],
        );
        assert_eq!(world(vec![shop]).resolve("shop").unwrap_err().len(), 3);
    }

    #[test]
    fn literal_dollar_paren_passes_through_unexpanded() {
        let shop = consumer("price $$(not.a-reference)", SettingType::String);
        let resolved = world(vec![shop]).resolve("shop").unwrap();
        assert_eq!(resolved.values["daemon"], Value::String("price $(not.a-reference)".into()));
    }

    #[test]
    fn resolved_values_are_never_expanded_again() {
        // A provider controls this text. It must arrive as text, not read a secret on arrival.
        let mut daemon = cuprated();
        daemon.values.insert("login".into(), secret_value("hunter2"));
        daemon
            .values
            .insert("name".into(), SettingValue::Literal(Value::String("$(cuprated.login)".into())));
        for template in ["$(cuprated.name)", "x$(cuprated.name)"] {
            let mut installed = world(vec![daemon.clone(), consumer(template, SettingType::String)]);
            installed.grants.insert(grant("shop", "cuprated", "login"));
            let value = installed.resolve("shop").unwrap().values["daemon"].clone();
            assert!(!format!("{value:?}").contains("hunter2"), "{template}");
            assert!(format!("{value:?}").contains("$(cuprated.login)"));
        }
    }

    #[test]
    fn literal_values_are_not_scanned_for_references() {
        let mut shop = consumer("unused", SettingType::Secret);
        shop.values.insert("daemon".into(), secret_value("$(cuprated.login)"));
        let resolved = world(vec![shop, cuprated()]).resolve("shop").unwrap();
        assert_eq!(resolved.values["daemon"], Value::Secret(SecretString::new("$(cuprated.login)")));
        assert!(resolved.dependencies.is_empty());
    }

    #[test]
    fn escaped_user_text_round_trips() {
        let text = "$(a.b) and $$(c) and $";
        let shop = consumer(&escape_literal(text), SettingType::String);
        assert_eq!(world(vec![shop]).resolve("shop").unwrap().values["daemon"], Value::String(text.into()));
    }

    #[test]
    fn endpoint_ports_must_be_declared_by_the_provider() {
        let mut daemon = cuprated();
        daemon.values.insert("rpc_port".into(), SettingValue::Literal(Value::Int(22)));
        let installed = world(vec![daemon, consumer("$(cuprated.rpc-port)", INT)]);
        assert!(matches!(only_error(&installed, "shop"), ResolveError::UndeclaredEndpoint { port: 22, .. }));
    }

    #[test]
    fn endpoint_urls_must_be_loopback() {
        let mut daemon = cuprated();
        daemon.values.insert("url".into(), SettingValue::Literal(Value::Url("http://evil.example:18081/".into())));
        let installed = world(vec![daemon, consumer("$(cuprated.rpc-url)", SettingType::Url)]);
        assert!(matches!(only_error(&installed, "shop"), ResolveError::InvalidEndpoint { .. }));
    }

    #[test]
    fn unix_endpoints_must_stay_in_the_providers_run_directory() {
        let socket = |value: &str| {
            let provider = package(
                "noded",
                &[],
                vec![endpoint(public(text_default(setting("sock", SettingType::String), value), "sock"), Endpoint::Unix)],
            );
            world(vec![provider, consumer("$(noded.sock)", SettingType::String)])
        };
        for bad in [
            "unix:/var/run/docker.sock",
            "unix:/run/apps/other/a.sock",
            "unix:/run/apps/noded/../other/a.sock",
            "unix:/run/apps/noded/a.txt",
            "/run/apps/noded/a.sock",
        ] {
            assert!(matches!(only_error(&socket(bad), "shop"), ResolveError::InvalidEndpoint { .. }), "{bad}");
        }
        let resolved = socket("unix:/run/apps/noded/rpc.sock").resolve("shop").unwrap();
        assert_eq!(
            resolved.network.into_iter().next().unwrap().access,
            Access::UnixSocket { path: "/run/apps/noded/rpc.sock".into() }
        );
    }

    #[test]
    fn grants_propagate_through_chains() {
        let proxy = package(
            "proxy",
            &[],
            vec![public(text_default(setting("up", SettingType::Url), "$(cuprated.rpc-url)"), "up")],
        );
        let installed = world(vec![cuprated(), proxy, consumer("$(proxy.up)", SettingType::Url)]);
        let resolved = installed.resolve("shop").unwrap();
        assert_eq!(resolved.values["daemon"], Value::Url("http://127.0.0.1:18081/".into()));
        assert_eq!(resolved.dependencies.len(), 2);
        assert_eq!(
            resolved.network.into_iter().map(|g| g.provider).collect::<Vec<_>>(),
            ["cuprated"],
            "the consumer receives cuprated's address, so it is cuprated it must reach"
        );
    }

    #[test]
    fn own_endpoints_grant_nothing() {
        let daemon = cuprated();
        let resolved = world(vec![daemon]).resolve("cuprated").unwrap();
        assert!(resolved.network.is_empty());
    }

    #[test]
    fn exec_arguments_are_substituted_without_splitting() {
        let mut daemon = cuprated();
        daemon
            .values
            .insert("name".into(), SettingValue::Literal(Value::String("a b; rm -rf / $(x) `y`".into())));
        let mut shop = package("shop", &[], vec![]);
        shop.manifest.exec.extend(["--name=$(cuprated.name)".to_string(), "--port=$(monero-daemon.rpc-port)".to_string()]);
        let resolved = world(vec![daemon, shop]).resolve("shop").unwrap();
        assert_eq!(resolved.exec, ["bin/run", "--name=a b; rm -rf / $(x) `y`", "--port=18081"]);
        assert_eq!(resolved.dependencies.len(), 2);
        assert_eq!(resolved.network.len(), 1);
    }

    #[test]
    fn exec_arguments_refuse_secrets_and_injected_options() {
        let mut daemon = cuprated();
        daemon.values.insert("login".into(), secret_value("hunter2"));
        daemon.values.insert("name".into(), SettingValue::Literal(Value::String("--evil".into())));

        let mut shop = package("shop", &[], vec![]);
        shop.manifest.exec.push("--key=$(cuprated.login)".to_string());
        let mut installed = world(vec![daemon.clone(), shop]);
        installed.grants.insert(grant("shop", "cuprated", "login"));
        assert!(matches!(only_error(&installed, "shop"), ResolveError::SecretInArgument { index: 1, .. }));

        let mut shop = package("shop", &[], vec![]);
        shop.manifest.exec.push("$(cuprated.name)".to_string());
        assert!(matches!(
            only_error(&world(vec![daemon.clone(), shop]), "shop"),
            ResolveError::OptionInjection { index: 1, .. }
        ));

        let mut shop = package("shop", &[], vec![]);
        shop.manifest.exec.push("--name=$(cuprated.name)".to_string());
        assert_eq!(world(vec![daemon, shop]).resolve("shop").unwrap().exec[1], "--name=--evil");
    }

    #[test]
    fn exec_escape_form_works() {
        let mut shop = package("shop", &[], vec![]);
        shop.manifest.exec.push("$$(literal)".to_string());
        assert_eq!(world(vec![shop]).resolve("shop").unwrap().exec[1], "$(literal)");
    }

    #[test]
    fn exec_references_join_the_dependency_graph() {
        let mut a = package("alpha", &[], vec![public(text_default(setting("x", SettingType::String), "1"), "x")]);
        a.manifest.exec.push("$(beta.y)".to_string());
        let mut b = package("beta", &[], vec![public(text_default(setting("y", SettingType::String), "2"), "y")]);
        b.manifest.exec.push("$(alpha.x)".to_string());
        assert!(matches!(only_error(&world(vec![a, b]), "alpha"), ResolveError::PackageCycle { .. }));
    }

    const TOML_ATTACKS: &[&str] = &[
        "x\"\nadmin = true\ny = \"",
        "x'''\nadmin = true\ny = '''",
        "\\\"\nadmin = true #",
        "\n[evil]\nadmin = true\n",
        "$(cuprated.login)",
        "$$(cuprated.login)",
    ];

    #[test]
    fn toml_rendering_substitutes_with_types() {
        let installed = world(vec![cuprated(), package("shop", &[], vec![])]);
        let rendered = installed
            .render_toml(
                "shop",
                r#"
                literal = "keep me"
                price = "$$(not.a-reference)"
                count = 5
                list = ["$(monero-daemon.rpc-port)", "a$(cuprated.name)"]
                [rpc]
                port = "$(monero-daemon.rpc-port)"
                url = "$(monero-daemon.rpc-url)"
                label = "node: $(cuprated.name)"
                "#,
            )
            .unwrap();
        let parsed: toml::Table = toml::from_str(&rendered.text).unwrap();
        assert_eq!(parsed["literal"].as_str(), Some("keep me"));
        assert_eq!(parsed["price"].as_str(), Some("$(not.a-reference)"));
        assert_eq!(parsed["count"].as_integer(), Some(5));
        assert_eq!(parsed["list"][0].as_integer(), Some(18081));
        assert_eq!(parsed["list"][1].as_str(), Some("acuprate"));
        assert_eq!(parsed["rpc"]["port"].as_integer(), Some(18081));
        assert_eq!(parsed["rpc"]["url"].as_str(), Some("http://127.0.0.1:18081/"));
        assert_eq!(parsed["rpc"]["label"].as_str(), Some("node: cuprate"));
        assert_eq!(rendered.network.len(), 1);
        assert_eq!(rendered.dependencies.len(), 2);
    }

    #[test]
    fn toml_rendering_cannot_be_injected() {
        for attack in TOML_ATTACKS {
            let mut daemon = cuprated();
            daemon.values.insert("name".into(), SettingValue::Literal(Value::String(attack.to_string())));
            let mut login = daemon.clone();
            login.values.insert("login".into(), secret_value("hunter2"));
            let installed = world(vec![login, package("shop", &[], vec![])]);
            for template in ["a = \"$(cuprated.name)\"\n", "a = \"<$(cuprated.name)>\"\n"] {
                let rendered = installed.render_toml("shop", template).unwrap();
                assert!(!rendered.text.contains("hunter2"), "{attack:?} expanded a secret");
                let parsed: toml::Table = toml::from_str(&rendered.text).unwrap();
                assert_eq!(parsed.len(), 1, "{attack:?} changed the document: {}", rendered.text);
                let got = parsed["a"].as_str().unwrap();
                assert!(got.contains(attack), "{attack:?} was altered to {got:?}");
            }
        }
    }

    #[test]
    fn toml_rendering_enforces_the_same_access_rules() {
        let installed = world(vec![cuprated(), package("shop", &[], vec![])]);
        for template in ["a = \"$(cuprated.hidden)\"", "a = \"$(cuprated.login)\"", "a = [{ b = \"$(cuprated.hidden)\" }]"] {
            let errors = installed.render_toml("shop", template).unwrap_err();
            assert!(matches!(errors[0], ResolveError::GrantRequired { .. }), "{template}: {errors:?}");
        }
    }

    #[test]
    fn toml_rendering_refuses_bad_templates() {
        let installed = world(vec![cuprated(), package("shop", &[], vec![])]);
        assert!(matches!(installed.render_toml("shop", "a = ").unwrap_err()[0], ResolveError::InvalidToml(_)));
        assert!(matches!(
            installed.render_toml("shop", "\"$(cuprated.name)\" = 1").unwrap_err()[0],
            ResolveError::ReferenceInKey(_)
        ));
        assert!(matches!(installed.render_toml("shop", "a = \"$(oops\"").unwrap_err()[0], ResolveError::Template { .. }));
        assert!(matches!(installed.render_toml("ghost", "").unwrap_err()[0], ResolveError::UnknownPackage(_)));
    }

    #[test]
    fn unknown_consumer_is_reported() {
        assert_eq!(only_error(&Installed::default(), "ghost"), ResolveError::UnknownPackage("ghost".into()));
    }
}
