//! Runs untrusted settings files.
//!
//! The Lua state starts from an allow list of functions rather than a deny list, so a library
//! addition in a future Lua cannot silently widen what packages can reach. Three removals are less
//! obvious than `io` and `os`:
//!
//! - `pcall`, `xpcall` and coroutines can catch errors. The limit hook reports by raising an error,
//!   and a script that catches it could keep running.
//! - `string.rep`, `table.move` and the pattern functions can spin inside C, where the instruction
//!   hook never fires. For example, `string.rep("", 2^53)` loops without allocating.
//! - `collectgarbage`, `setmetatable` and `getmetatable` could disable collection, install `__gc`
//!   finalizers, or reach the string metatable.
//!
//! Each validation runs in a new Lua state, which is what makes validators pure: no upvalue or
//! global survives from one call to the next.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::{
    ChunkMode, Function, HookTriggers, Lua, LuaOptions, MultiValue, StdLib, Table, Value as LuaValue, VmState,
};
use regex::RegexBuilder;
use thiserror::Error;

use crate::schema::{
    is_valid_setting_name, DefaultValue, Endpoint, Generate, Schema, SettingDef, SettingType, Value,
    MAX_GENERATED_BYTES, MIN_GENERATED_BYTES, SETTINGS_API_VERSION,
};
use crate::template::{literal_text, parse_template, Part};

const MAX_SOURCE_LEN: usize = 256 * 1024;
const MAX_SETTINGS: usize = 128;
const MAX_LABEL_LEN: usize = 80;
const MAX_HELP_LEN: usize = 1024;
const MAX_CHOICES: usize = 64;
const MAX_CHOICE_LEN: usize = 128;
const MAX_MESSAGE_LEN: usize = 200;
const MAX_REGEX_LEN: usize = 512;
const MAX_REGEX_TEXT_LEN: usize = 64 * 1024;
const REGEX_SIZE_LIMIT: usize = 256 * 1024;

/// How often the hook runs. Small enough that a loop of expensive C calls, such as concatenating
/// megabyte strings, still reaches the wall clock check promptly.
const HOOK_INTERVAL: u32 = 64;

const ALLOWED_GLOBALS: &[&str] =
    &["assert", "error", "ipairs", "math", "next", "pairs", "select", "string", "table", "tonumber", "tostring", "type"];
const ALLOWED_STRING: &[&str] = &["byte", "char", "format", "len", "lower", "reverse", "sub", "upper"];
const ALLOWED_TABLE: &[&str] = &["concat", "insert", "remove", "unpack"];
// `math.random` is absent because validators must be deterministic.
const ALLOWED_MATH: &[&str] = &[
    "abs", "ceil", "floor", "fmod", "huge", "max", "maxinteger", "min", "mininteger", "modf", "pi", "sqrt",
    "tointeger", "type", "ult",
];

/// Resource limits for one run of Lua code, whether loading a file or calling one validator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub memory_bytes: usize,
    pub instructions: u64,
    /// Instructions do not account for time spent in C, so a run has a time limit as well.
    pub wall_time: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { memory_bytes: 8 * 1024 * 1024, instructions: 5_000_000, wall_time: Duration::from_secs(2) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    Memory,
    Instructions,
    WallTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SettingsError {
    #[error("settings code must not run as root")]
    RunningAsRoot,
    #[error("the settings file is larger than {MAX_SOURCE_LEN} bytes")]
    SourceTooLarge,
    #[error("settings file uses API version {found}, this device supports {SETTINGS_API_VERSION}")]
    UnsupportedApi { found: u32 },
    #[error("the settings file failed: {0}")]
    Script(String),
    #[error("the settings file exceeded its {0:?} limit")]
    LimitExceeded(Limit),
    #[error("invalid settings file: {}", .0.join("; "))]
    Invalid(Vec<String>),
    #[error("no setting is named '{0}'")]
    UnknownSetting(String),
    #[error("{setting}: {message}")]
    Rejected { setting: String, message: String },
}

/// The Lua code runs with no privileges to drop, but the architecture requires that it never runs
/// as root at all, so the privileged helper cannot end up doing this by accident.
fn check_euid(euid: u32) -> Result<(), SettingsError> {
    if euid == 0 {
        return Err(SettingsError::RunningAsRoot);
    }
    Ok(())
}

fn refuse_root() -> Result<(), SettingsError> {
    // Tests may legitimately run as root inside a build container. `check_euid` has its own test.
    if cfg!(test) {
        return Ok(());
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    check_euid(unsafe { libc::geteuid() })
}

/// Shared between the hook and the code that interprets its errors.
struct Budget {
    limits: Limits,
    used: Cell<u64>,
    deadline: Cell<Instant>,
    tripped: Cell<Option<Limit>>,
}

impl Budget {
    fn new(limits: Limits) -> Rc<Self> {
        Rc::new(Budget {
            limits,
            used: Cell::new(0),
            deadline: Cell::new(Instant::now() + limits.wall_time),
            tripped: Cell::new(None),
        })
    }

    fn reset(&self) {
        self.used.set(0);
        self.deadline.set(Instant::now() + self.limits.wall_time);
    }

    fn tick(&self) -> Result<VmState, mlua::Error> {
        self.used.set(self.used.get().saturating_add(u64::from(HOOK_INTERVAL)));
        let exceeded = if self.used.get() > self.limits.instructions {
            Some(Limit::Instructions)
        } else if Instant::now() > self.deadline.get() {
            Some(Limit::WallTime)
        } else {
            None
        };
        match exceeded {
            Some(limit) => {
                self.tripped.set(Some(limit));
                Err(mlua::Error::runtime("resource limit exceeded"))
            }
            None => Ok(VmState::Continue),
        }
    }
}

fn is_memory_error(error: &mlua::Error) -> bool {
    match error {
        mlua::Error::MemoryError(_) => true,
        mlua::Error::CallbackError { cause, .. } => is_memory_error(cause),
        other => other.to_string().contains("not enough memory"),
    }
}

/// Reduces untrusted error text to something safe for a terminal UI: one line, no escape sequences.
fn clean_message(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    let line = line.strip_prefix("runtime error: ").unwrap_or(line);
    line.chars().filter(|c| !c.is_control()).take(MAX_MESSAGE_LEN).collect()
}

struct Vm {
    lua: Lua,
    budget: Rc<Budget>,
}

impl Vm {
    fn new(limits: Limits) -> Result<Self, SettingsError> {
        let budget = Budget::new(limits);
        let setup = |lua: &Lua| -> mlua::Result<()> {
            let globals = lua.globals();
            keep_only(&globals, ALLOWED_GLOBALS)?;
            // These are the same tables the string metatable's `__index` points at, so removing
            // entries here also removes them from method calls such as `("x"):rep(9)`.
            keep_only(&globals.get::<Table>("string")?, ALLOWED_STRING)?;
            keep_only(&globals.get::<Table>("table")?, ALLOWED_TABLE)?;
            keep_only(&globals.get::<Table>("math")?, ALLOWED_MATH)?;

            let nodo = lua.create_table()?;
            nodo.set("api_version", SETTINGS_API_VERSION)?;
            nodo.set("matches", lua.create_function(lua_matches)?)?;
            globals.set("nodo", nodo)?;
            Ok(())
        };
        let lua = Lua::new_with(StdLib::STRING | StdLib::TABLE | StdLib::MATH, LuaOptions::default())
            .map_err(|e| SettingsError::Script(clean_message(&e.to_string())))?;
        setup(&lua).map_err(|e| SettingsError::Script(clean_message(&e.to_string())))?;
        lua.set_memory_limit(limits.memory_bytes).map_err(|e| SettingsError::Script(clean_message(&e.to_string())))?;
        let hook_budget = budget.clone();
        lua.set_global_hook(HookTriggers::new().every_nth_instruction(HOOK_INTERVAL), move |_, _| hook_budget.tick())
            .map_err(|e| SettingsError::Script(clean_message(&e.to_string())))?;
        Ok(Vm { lua, budget })
    }

    /// Runs the file and returns its table. Binary chunks are refused because Lua does not verify
    /// bytecode, and crafted bytecode can corrupt memory.
    fn eval(&self, source: &str) -> Result<Table, SettingsError> {
        self.eval_named(source, "settings")
    }

    fn eval_named(&self, source: &str, name: &str) -> Result<Table, SettingsError> {
        let result = self.lua.load(source).set_name(name).set_mode(ChunkMode::Text).eval::<LuaValue>();
        let value = result.map_err(|e| self.classify(&e))?;
        match value {
            LuaValue::Table(table) => Ok(table),
            other => Err(SettingsError::Invalid(vec![format!(
                "the file must return a table, not {}",
                other.type_name()
            )])),
        }
    }

    /// Hook errors can arrive wrapped in other errors, so the recorded flag decides.
    fn classify(&self, error: &mlua::Error) -> SettingsError {
        if let Some(limit) = self.budget.tripped.get() {
            return SettingsError::LimitExceeded(limit);
        }
        if is_memory_error(error) {
            return SettingsError::LimitExceeded(Limit::Memory);
        }
        SettingsError::Script(clean_message(&error.to_string()))
    }
}

/// The settings sandbox, for other package code that must run under exactly the same rules, such
/// as the screen's panel functions (docs/SCREEN.md). Each `Sandbox` is a fresh Lua state: create
/// one per run so that nothing survives between runs.
pub struct Sandbox {
    vm: Vm,
}

impl Sandbox {
    /// Refuses to run as root, like settings code.
    pub fn new(limits: Limits) -> Result<Self, SettingsError> {
        refuse_root()?;
        Ok(Sandbox { vm: Vm::new(limits)? })
    }

    /// Runs a text chunk (never bytecode) and returns the table it returns. `name` appears in
    /// error messages.
    pub fn eval_table(&self, source: &str, name: &str) -> Result<Table, SettingsError> {
        if source.len() > MAX_SOURCE_LEN {
            return Err(SettingsError::SourceTooLarge);
        }
        self.vm.budget.reset();
        self.vm.eval_named(source, name)
    }

    /// The state, for converting values in and out. Calls into package code go through
    /// [`Sandbox::run`] so that they are metered.
    pub fn lua(&self) -> &Lua {
        &self.vm.lua
    }

    /// Runs `body` with a fresh instruction and time budget. Exceeding a limit or the memory cap
    /// becomes [`SettingsError::LimitExceeded`], any other error a one-line [`SettingsError::Script`].
    pub fn run<T>(&self, body: impl FnOnce(&Lua) -> mlua::Result<T>) -> Result<T, SettingsError> {
        self.vm.budget.reset();
        body(&self.vm.lua).map_err(|e| self.vm.classify(&e))
    }
}

fn keep_only(table: &Table, allowed: &[&str]) -> mlua::Result<()> {
    let mut remove = Vec::new();
    for pair in table.pairs::<LuaValue, LuaValue>() {
        let (key, _) = pair?;
        let keep = matches!(&key, LuaValue::String(name) if allowed.iter().any(|a| name.as_bytes().as_ref() == a.as_bytes()));
        if !keep {
            remove.push(key);
        }
    }
    for key in remove {
        table.raw_set(key, LuaValue::Nil)?;
    }
    Ok(())
}

/// A linear time replacement for Lua patterns.
fn lua_matches(_: &Lua, (text, pattern): (mlua::String, mlua::String)) -> mlua::Result<bool> {
    let text = text.to_str()?;
    let pattern = pattern.to_str()?;
    if text.len() > MAX_REGEX_TEXT_LEN {
        return Err(mlua::Error::runtime("nodo.matches: the text is too long"));
    }
    if pattern.len() > MAX_REGEX_LEN {
        return Err(mlua::Error::runtime("nodo.matches: the pattern is too long"));
    }
    let regex = RegexBuilder::new(&pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|_| mlua::Error::runtime("nodo.matches: invalid pattern"))?;
    Ok(regex.is_match(&text))
}

/// Collects every problem in a file, so a package author sees them all at once.
struct Issues(Vec<String>);

impl Issues {
    fn add(&mut self, field: &str, problem: impl AsRef<str>) {
        self.0.push(format!("{field}: {}", problem.as_ref()));
    }
}

fn is_display_text(text: &str, allow_newline: bool) -> bool {
    text.chars().all(|c| !c.is_control() || (allow_newline && c == '\n'))
}

fn read_text(issues: &mut Issues, field: &str, value: Option<&LuaValue>) -> Option<String> {
    match value? {
        LuaValue::String(text) => match text.to_str() {
            Ok(text) => Some(text.to_string()),
            Err(_) => {
                issues.add(field, "must be valid UTF-8");
                None
            }
        },
        other => {
            issues.add(field, format!("must be a string, not {}", other.type_name()));
            None
        }
    }
}

fn read_int(issues: &mut Issues, field: &str, value: Option<&LuaValue>) -> Option<i64> {
    match value? {
        LuaValue::Integer(number) => Some(*number),
        other => {
            issues.add(field, format!("must be an integer, not {}", other.type_name()));
            None
        }
    }
}

fn read_fields(issues: &mut Issues, field: &str, table: &Table, allowed: &[&str]) -> BTreeMap<String, LuaValue> {
    let mut fields = BTreeMap::new();
    for pair in table.pairs::<LuaValue, LuaValue>() {
        let Ok((key, value)) = pair else {
            issues.add(field, "could not be read");
            continue;
        };
        let name = match &key {
            LuaValue::String(name) => name.to_str().map(|n| n.to_string()).ok(),
            _ => None,
        };
        match name {
            Some(name) if allowed.contains(&name.as_str()) => {
                fields.insert(name, value);
            }
            Some(name) => {
                let shown: String = name.chars().filter(|c| !c.is_control()).take(40).collect();
                issues.add(field, format!("unknown key '{shown}'"));
            }
            None => issues.add(field, "has a key that is not a string"),
        }
    }
    fields
}

const SETTING_KEYS: &[&str] = &[
    "name", "type", "label", "help", "required", "default", "public", "endpoint", "validate", "min", "max",
    "choices", "generate",
];

fn read_generate(issues: &mut Issues, field: &str, value: Option<&LuaValue>) -> Option<Generate> {
    let value = value?;
    let LuaValue::Table(table) = value else {
        issues.add(field, format!("must be a table such as {{ hex = 32 }}, not {}", value.type_name()));
        return None;
    };
    let fields = read_fields(issues, field, table, &["hex"]);
    let Some(bytes) = read_int(issues, &format!("{field}.hex"), fields.get("hex")) else {
        if !fields.contains_key("hex") {
            issues.add(field, "must say how many bytes, as { hex = 32 }");
        }
        return None;
    };
    match u8::try_from(bytes) {
        Ok(bytes) if (MIN_GENERATED_BYTES..=MAX_GENERATED_BYTES).contains(&bytes) => Some(Generate::Hex { bytes }),
        _ => {
            issues.add(
                &format!("{field}.hex"),
                format!("must be between {MIN_GENERATED_BYTES} and {MAX_GENERATED_BYTES} bytes"),
            );
            None
        }
    }
}

fn read_type(issues: &mut Issues, field: &str, fields: &BTreeMap<String, LuaValue>) -> Option<SettingType> {
    let name = read_text(issues, &format!("{field}.type"), fields.get("type"));
    if name.is_none() && !fields.contains_key("type") {
        issues.add(field, "has no type");
    }
    let name = name?;
    let has_range = fields.contains_key("min") || fields.contains_key("max");
    if has_range && name != "int" {
        issues.add(field, "min and max only apply to type int");
    }
    if fields.contains_key("choices") && name != "enum" {
        issues.add(field, "choices only apply to type enum");
    }
    match name.as_str() {
        "string" => Some(SettingType::String),
        "bool" => Some(SettingType::Bool),
        "monero-address" => Some(SettingType::MoneroAddress),
        "url" => Some(SettingType::Url),
        "secret" => Some(SettingType::Secret),
        "int" => {
            let min = read_int(issues, &format!("{field}.min"), fields.get("min"));
            let max = read_int(issues, &format!("{field}.max"), fields.get("max"));
            if let (Some(min), Some(max)) = (min, max) {
                if min > max {
                    issues.add(field, "min is greater than max");
                }
            }
            Some(SettingType::Int { min, max })
        }
        "enum" => Some(SettingType::Enum { choices: read_choices(issues, &format!("{field}.choices"), fields) }),
        _ => {
            issues.add(&format!("{field}.type"), "is not a known setting type");
            None
        }
    }
}

fn read_choices(issues: &mut Issues, field: &str, fields: &BTreeMap<String, LuaValue>) -> Vec<String> {
    let Some(LuaValue::Table(list)) = fields.get("choices") else {
        issues.add(field, "must be a list of strings");
        return Vec::new();
    };
    let count = list.raw_len();
    if count == 0 || count > MAX_CHOICES {
        issues.add(field, format!("must have between 1 and {MAX_CHOICES} entries"));
        return Vec::new();
    }
    let mut choices = Vec::new();
    for index in 1..=count {
        let value = list.raw_get::<LuaValue>(index).unwrap_or(LuaValue::Nil);
        let entry = format!("{field}[{index}]");
        let Some(choice) = read_text(issues, &entry, Some(&value)) else { continue };
        if choice.is_empty() || choice.len() > MAX_CHOICE_LEN || !is_display_text(&choice, false) {
            issues.add(&entry, "is not a usable choice");
        } else if choices.contains(&choice) {
            issues.add(&entry, "is a duplicate");
        } else {
            choices.push(choice);
        }
    }
    choices
}

/// Names of settings an earlier version declared and this one removed. Each must be a valid
/// setting name that this file no longer declares, so a retirement can never hide a live setting.
fn read_retired(issues: &mut Issues, fields: &BTreeMap<String, LuaValue>, declared: &BTreeSet<&str>) -> Vec<String> {
    let list = match fields.get("retired") {
        None => return Vec::new(),
        Some(LuaValue::Table(list)) => list,
        Some(other) => {
            issues.add("retired", format!("must be a list of setting names, not {}", other.type_name()));
            return Vec::new();
        }
    };
    let count = list.raw_len();
    if count > MAX_SETTINGS {
        issues.add("retired", format!("has more than {MAX_SETTINGS} entries"));
        return Vec::new();
    }
    let mut retired = Vec::new();
    for index in 1..=count {
        let value = list.raw_get::<LuaValue>(index).unwrap_or(LuaValue::Nil);
        let entry = format!("retired[{index}]");
        let Some(name) = read_text(issues, &entry, Some(&value)) else { continue };
        if !is_valid_setting_name(&name) {
            issues.add(&entry, "is not a valid setting name");
        } else if declared.contains(name.as_str()) {
            issues.add(&entry, format!("names '{name}', which is still declared"));
        } else if retired.contains(&name) {
            issues.add(&entry, "is a duplicate");
        } else {
            retired.push(name);
        }
    }
    retired
}

fn read_endpoint(issues: &mut Issues, field: &str, ty: &SettingType, value: Option<&LuaValue>) -> Option<Endpoint> {
    let name = read_text(issues, field, value)?;
    let endpoint = match name.as_str() {
        "tcp" => Endpoint::Tcp,
        "udp" => Endpoint::Udp,
        "unix" => Endpoint::Unix,
        _ => {
            issues.add(field, "must be tcp, udp or unix");
            return None;
        }
    };
    let fits = match endpoint {
        Endpoint::Tcp | Endpoint::Udp => matches!(ty, SettingType::Int { .. } | SettingType::Url),
        Endpoint::Unix => matches!(ty, SettingType::String),
    };
    if !fits {
        issues.add(field, format!("cannot be {name} on a setting of type {}", ty.name()));
    }
    Some(endpoint)
}

fn read_default(issues: &mut Issues, field: &str, ty: &SettingType, value: Option<&LuaValue>) -> Option<DefaultValue> {
    let default = match value? {
        LuaValue::Integer(number) => DefaultValue::Int(*number),
        LuaValue::Boolean(flag) => DefaultValue::Bool(*flag),
        LuaValue::String(_) => DefaultValue::Text(read_text(issues, field, value)?),
        other => {
            issues.add(field, format!("cannot be a {}", other.type_name()));
            return None;
        }
    };
    let problem = match &default {
        DefaultValue::Int(number) => ty.check(&Value::Int(*number)).err(),
        DefaultValue::Bool(flag) => ty.check(&Value::Bool(*flag)).err(),
        DefaultValue::Text(text) => match parse_template(text) {
            Err(error) => Some(error.to_string()),
            Ok(parts) => check_text_default(ty, &parts),
        },
    };
    if let Some(problem) = problem {
        issues.add(field, problem);
        return None;
    }
    Some(default)
}

fn check_text_default(ty: &SettingType, parts: &[Part]) -> Option<String> {
    if matches!(ty, SettingType::Secret) {
        return match parts {
            [Part::Reference(_)] => None,
            _ => Some("a secret's default can only be a single reference".to_string()),
        };
    }
    literal_text(parts).and_then(|literal| ty.parse_literal(&literal).err())
}

fn read_setting(issues: &mut Issues, field: &str, table: &Table) -> Option<SettingDef> {
    let fields = read_fields(issues, field, table, SETTING_KEYS);

    let name = read_text(issues, &format!("{field}.name"), fields.get("name"));
    if !fields.contains_key("name") {
        issues.add(field, "has no name");
    }
    if let Some(name) = &name {
        if !is_valid_setting_name(name) {
            issues.add(&format!("{field}.name"), "must be lowercase letters, digits, '_' and '-'");
        }
    }

    let label = read_text(issues, &format!("{field}.label"), fields.get("label"));
    match &label {
        Some(label) if label.is_empty() || label.len() > MAX_LABEL_LEN || !is_display_text(label, false) => {
            issues.add(&format!("{field}.label"), format!("must be 1 to {MAX_LABEL_LEN} bytes of plain text"));
        }
        None if !fields.contains_key("label") => issues.add(field, "has no label"),
        _ => {}
    }

    let help = read_text(issues, &format!("{field}.help"), fields.get("help"));
    if let Some(help) = &help {
        if help.len() > MAX_HELP_LEN || !is_display_text(help, true) {
            issues.add(&format!("{field}.help"), format!("must be at most {MAX_HELP_LEN} bytes of plain text"));
        }
    }

    let required = match fields.get("required") {
        None => false,
        Some(LuaValue::Boolean(flag)) => *flag,
        Some(other) => {
            issues.add(&format!("{field}.required"), format!("must be a boolean, not {}", other.type_name()));
            false
        }
    };

    let has_validator = match fields.get("validate") {
        None => false,
        Some(LuaValue::Function(_)) => true,
        Some(other) => {
            issues.add(&format!("{field}.validate"), format!("must be a function, not {}", other.type_name()));
            false
        }
    };

    let public = read_text(issues, &format!("{field}.public"), fields.get("public"));
    if let Some(public) = &public {
        if !is_valid_setting_name(public) {
            issues.add(&format!("{field}.public"), "must be lowercase letters, digits, '_' and '-'");
        }
    }

    let ty = read_type(issues, field, &fields)?;
    if public.is_some() && matches!(ty, SettingType::Secret) {
        issues.add(&format!("{field}.public"), "a secret can never be public");
    }
    let endpoint = read_endpoint(issues, &format!("{field}.endpoint"), &ty, fields.get("endpoint"));
    let default = read_default(issues, &format!("{field}.default"), &ty, fields.get("default"));
    let generate = read_generate(issues, &format!("{field}.generate"), fields.get("generate"));
    if fields.contains_key("generate") {
        if !matches!(ty, SettingType::Secret) {
            issues.add(&format!("{field}.generate"), "only applies to type secret");
        }
        if fields.contains_key("default") {
            issues.add(&format!("{field}.generate"), "cannot be combined with a default");
        }
    }

    Some(SettingDef {
        name: name?,
        ty,
        label: label?,
        help,
        required,
        default,
        public,
        endpoint,
        has_validator,
        generate,
    })
}

fn read_schema(root: &Table) -> Result<Schema, SettingsError> {
    // The version comes first because a newer file may legitimately have keys this build rejects.
    let found = match root.raw_get::<LuaValue>("api_version") {
        Ok(LuaValue::Integer(version)) => version,
        _ => return Err(SettingsError::Invalid(vec!["api_version must be an integer".to_string()])),
    };
    if found != i64::from(SETTINGS_API_VERSION) {
        return Err(SettingsError::UnsupportedApi { found: u32::try_from(found).unwrap_or(u32::MAX) });
    }

    let mut issues = Issues(Vec::new());
    let fields = read_fields(&mut issues, "file", root, &["api_version", "settings", "retired"]);
    let mut settings = Vec::new();
    match fields.get("settings") {
        None => {}
        Some(LuaValue::Table(list)) => {
            let count = list.raw_len();
            if count > MAX_SETTINGS {
                issues.add("settings", format!("has more than {MAX_SETTINGS} entries"));
            }
            for index in 1..=count.min(MAX_SETTINGS) {
                let field = format!("settings[{index}]");
                match list.raw_get::<LuaValue>(index) {
                    Ok(LuaValue::Table(entry)) => {
                        if let Some(setting) = read_setting(&mut issues, &field, &entry) {
                            settings.push(setting);
                        }
                    }
                    _ => issues.add(&field, "must be a table"),
                }
            }
        }
        Some(other) => issues.add("settings", format!("must be a list, not {}", other.type_name())),
    }

    let mut names = BTreeSet::new();
    let mut exports = BTreeSet::new();
    for setting in &settings {
        if !names.insert(setting.name.as_str()) {
            issues.add(&format!("setting '{}'", setting.name), "is declared twice");
        }
        if let Some(public) = &setting.public {
            if !exports.insert(public.as_str()) {
                issues.add(&format!("setting '{}'", setting.name), "reuses a public name");
            }
        }
    }
    // A reference tries public names first, so a public name equal to another setting's private
    // name would make it unclear which of the two a grant covers.
    for setting in &settings {
        if let Some(public) = &setting.public {
            if public != &setting.name && names.contains(public.as_str()) {
                issues.add(&format!("setting '{}'", setting.name), "has a public name equal to another setting's name");
            }
        }
    }

    let retired = read_retired(&mut issues, &fields, &names);

    if !issues.0.is_empty() {
        return Err(SettingsError::Invalid(issues.0));
    }
    Ok(Schema { api_version: SETTINGS_API_VERSION, settings, retired })
}

/// A settings file that has been run and checked.
#[derive(Debug, Clone)]
pub struct LoadedSettings {
    schema: Schema,
    source: String,
    limits: Limits,
}

/// Runs a settings file in the sandbox and returns what it declares.
pub fn load_settings(source: &str, limits: Limits) -> Result<LoadedSettings, SettingsError> {
    refuse_root()?;
    if source.len() > MAX_SOURCE_LEN {
        return Err(SettingsError::SourceTooLarge);
    }
    let vm = Vm::new(limits)?;
    let root = vm.eval(source)?;
    let schema = read_schema(&root)?;
    let loaded = LoadedSettings { schema, source: source.to_string(), limits };
    loaded.check_defaults()?;
    Ok(loaded)
}

impl LoadedSettings {
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    fn check_defaults(&self) -> Result<(), SettingsError> {
        let mut issues = Vec::new();
        for setting in self.schema.settings.iter().filter(|s| s.has_validator) {
            let value = match &setting.default {
                Some(DefaultValue::Int(number)) => Value::Int(*number),
                Some(DefaultValue::Bool(flag)) => Value::Bool(*flag),
                Some(DefaultValue::Text(text)) => {
                    let literal = parse_template(text).ok().and_then(|parts| literal_text(&parts));
                    match literal.map(|literal| setting.ty.parse_literal(&literal)) {
                        Some(Ok(value)) => value,
                        // A default holding references can only be validated after resolution.
                        _ => continue,
                    }
                }
                None => continue,
            };
            match self.validate_value(&setting.name, &value) {
                Ok(()) => {}
                Err(SettingsError::Rejected { setting, message }) => {
                    issues.push(format!("the default for {setting} fails its own validator: {message}"));
                }
                Err(other) => return Err(other),
            }
        }
        if issues.is_empty() {
            Ok(())
        } else {
            Err(SettingsError::Invalid(issues))
        }
    }

    /// Checks a value against the setting's type and then its validation function.
    pub fn validate_value(&self, name: &str, value: &Value) -> Result<(), SettingsError> {
        let setting = self.schema.setting(name).ok_or_else(|| SettingsError::UnknownSetting(name.to_string()))?;
        setting
            .ty
            .check(value)
            .map_err(|message| SettingsError::Rejected { setting: name.to_string(), message })?;
        if !setting.has_validator {
            return Ok(());
        }
        refuse_root()?;
        let vm = Vm::new(self.limits)?;
        let validator = find_validator(&vm, &self.source, name)?;
        vm.budget.reset();
        let argument = to_lua(&vm.lua, value).map_err(|e| vm.classify(&e))?;
        let secret = matches!(value, Value::Secret(_));
        let rejected = |message: String| SettingsError::Rejected {
            setting: name.to_string(),
            // A validator could echo its argument, which must not reach logs or the screen.
            message: if secret { "was rejected by the package".to_string() } else { message },
        };
        let results = match validator.call::<MultiValue>(argument) {
            Ok(results) => results,
            Err(error) => {
                return match vm.classify(&error) {
                    SettingsError::Script(message) => Err(rejected(message)),
                    other => Err(other),
                }
            }
        };
        let mut results = results.into_iter();
        match results.next() {
            Some(LuaValue::Boolean(true)) => Ok(()),
            Some(LuaValue::Boolean(false)) => {
                let message = match results.next() {
                    Some(LuaValue::String(text)) => clean_message(&text.to_string_lossy()),
                    _ => "is not valid".to_string(),
                };
                Err(rejected(if message.is_empty() { "is not valid".to_string() } else { message }))
            }
            // Fail closed: forgetting to return anything must not count as passing.
            _ => Err(rejected("the validator did not return true or false".to_string())),
        }
    }

    /// Validates a resolved set of values and reports every failure.
    pub fn validate_values(&self, values: &BTreeMap<String, Value>) -> Vec<SettingsError> {
        values.iter().filter_map(|(name, value)| self.validate_value(name, value).err()).collect()
    }
}

fn find_validator(vm: &Vm, source: &str, name: &str) -> Result<Function, SettingsError> {
    let root = vm.eval(source)?;
    let missing = || SettingsError::Script("the settings file changed between runs".to_string());
    let list: Table = root.raw_get("settings").map_err(|_| missing())?;
    for index in 1..=list.raw_len() {
        let entry: Table = list.raw_get(index).map_err(|_| missing())?;
        if entry.raw_get::<String>("name").ok().as_deref() == Some(name) {
            return entry.raw_get("validate").map_err(|_| missing());
        }
    }
    Err(missing())
}

fn to_lua(lua: &Lua, value: &Value) -> mlua::Result<LuaValue> {
    Ok(match value {
        Value::Int(number) => LuaValue::Integer(*number),
        Value::Bool(flag) => LuaValue::Boolean(*flag),
        text => LuaValue::String(lua.create_string(text.text())?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monero::encode_address;

    fn load(source: &str) -> Result<LoadedSettings, SettingsError> {
        load_settings(source, Limits::default())
    }

    fn with_settings(body: &str) -> String {
        format!("return {{ api_version = 1, settings = {{ {body} }} }}")
    }

    fn one_validator(body: &str) -> LoadedSettings {
        load(&with_settings(&format!(
            r#"{{ name = "v", type = "string", label = "V", validate = function(value) {body} end }}"#
        )))
        .unwrap()
    }

    /// Evaluates a Lua expression in the sandbox and returns it as a string.
    fn eval(expression: &str) -> Result<String, SettingsError> {
        let vm = Vm::new(Limits::default())?;
        vm.lua.load(expression).set_mode(ChunkMode::Text).eval::<String>().map_err(|e| vm.classify(&e))
    }

    fn invalid(source: &str) -> Vec<String> {
        match load(source) {
            Err(SettingsError::Invalid(issues)) => issues,
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn loads_a_full_schema() {
        let address = encode_address(18, 64);
        let source = format!(
            r#"return {{
                api_version = nodo.api_version,
                settings = {{
                    {{ name = "motd", type = "string", label = "Message", help = "Shown\nto users", default = "hi" }},
                    {{ name = "port", type = "int", label = "Port", default = 18081, min = 1024, max = 65535,
                       public = "rpc-port", endpoint = "tcp" }},
                    {{ name = "on", type = "bool", label = "On", default = true }},
                    {{ name = "mode", type = "enum", label = "Mode", choices = {{ "a", "b" }}, default = "b" }},
                    {{ name = "payout", type = "monero-address", label = "Payout", default = "{address}" }},
                    {{ name = "daemon", type = "url", label = "Daemon", default = "$(monero-daemon.rpc-url)" }},
                    {{ name = "key", type = "secret", label = "Key", required = true }},
                    {{ name = "socket", type = "string", label = "Socket", endpoint = "unix", public = "socket" }},
                }},
            }}"#
        );
        let schema = load(&source).unwrap().schema().clone();
        assert_eq!(schema.api_version, SETTINGS_API_VERSION);
        assert_eq!(schema.settings.len(), 8);
        let port = schema.exported("rpc-port").unwrap();
        assert_eq!(port.name, "port");
        assert_eq!(port.ty, SettingType::Int { min: Some(1024), max: Some(65535) });
        assert_eq!(port.endpoint, Some(Endpoint::Tcp));
        assert!(schema.setting("key").unwrap().required);
        assert_eq!(schema.setting("mode").unwrap().default, Some(DefaultValue::Text("b".into())));
    }

    #[test]
    fn retired_settings_are_listed_by_name() {
        let schema = load(
            r#"return { api_version = 1, retired = { "old_switch", "older" },
                settings = { { name = "port", type = "int", label = "Port", default = 1 } } }"#,
        )
        .unwrap()
        .schema()
        .clone();
        assert_eq!(schema.retired, ["old_switch", "older"]);
        assert!(schema.is_retired("old_switch") && !schema.is_retired("port") && !schema.is_retired("ghost"));
    }

    #[test]
    fn a_retired_name_must_be_a_removed_setting() {
        let source = |retired: &str| {
            format!(
                "return {{ api_version = 1, retired = {retired}, settings = {{ \
                 {{ name = \"port\", type = \"int\", label = \"Port\", default = 1 }} }} }}"
            )
        };
        assert!(invalid(&source(r#"{ "port" }"#))[0].contains("still declared"));
        assert!(invalid(&source(r#"{ "Bad Name" }"#))[0].contains("not a valid setting name"));
        assert!(invalid(&source(r#"{ "a", "a" }"#))[0].contains("duplicate"));
        assert!(invalid(&source(r#""a""#))[0].contains("must be a list"));
        assert!(invalid(&source("{ 1 }"))[0].contains("retired[1]"));
    }

    #[test]
    fn an_empty_settings_file_is_valid() {
        assert!(load("return { api_version = 1 }").unwrap().schema().settings.is_empty());
    }

    #[test]
    fn checks_the_api_version() {
        assert_eq!(load("return { api_version = 2, future_key = {} }").unwrap_err(), SettingsError::UnsupportedApi {
            found: 2
        });
        assert_eq!(load("return { api_version = 0 }").unwrap_err(), SettingsError::UnsupportedApi { found: 0 });
        assert!(matches!(load("return {}"), Err(SettingsError::Invalid(_))));
        assert!(matches!(load("return { api_version = '1' }"), Err(SettingsError::Invalid(_))));
        assert!(matches!(load("return { api_version = 1.5 }"), Err(SettingsError::Invalid(_))));
    }

    #[test]
    fn root_is_refused() {
        assert_eq!(check_euid(0), Err(SettingsError::RunningAsRoot));
        assert_eq!(check_euid(1000), Ok(()));
    }

    #[test]
    fn the_vendored_interpreter_is_lua_5_4() {
        // Other versions have different escape hatches, so a dependency bump must not change this.
        let version: String = Lua::new().globals().get("_VERSION").unwrap();
        assert_eq!(version, "Lua 5.4");
    }

    #[test]
    fn dangerous_globals_do_not_exist() {
        for name in [
            "io", "os", "debug", "package", "require", "load", "loadstring", "loadfile", "dofile", "print", "pcall",
            "xpcall", "coroutine", "collectgarbage", "setmetatable", "getmetatable", "rawset", "rawget", "rawequal",
            "rawlen", "utf8", "_G", "_VERSION", "warn",
        ] {
            let result = eval(&format!("return type({name})")).unwrap();
            assert_eq!(result, "nil", "global {name} should be absent");
        }
    }

    #[test]
    fn only_allow_listed_library_functions_exist() {
        let list = |table: &str| {
            let vm = Vm::new(Limits::default()).unwrap();
            let names: Vec<String> = vm
                .lua
                .load(format!("local n = {{}} for k in pairs({table}) do n[#n + 1] = k end return n"))
                .eval::<Vec<String>>()
                .unwrap();
            let mut names = names;
            names.sort();
            names
        };
        let sorted = |allowed: &[&str]| {
            let mut allowed: Vec<String> = allowed.iter().map(|s| s.to_string()).collect();
            allowed.sort();
            allowed
        };
        assert_eq!(list("string"), sorted(ALLOWED_STRING));
        assert_eq!(list("table"), sorted(ALLOWED_TABLE));
        assert_eq!(list("math"), sorted(ALLOWED_MATH));
        let mut globals = sorted(ALLOWED_GLOBALS);
        globals.push("nodo".to_string());
        globals.sort();
        assert_eq!(list("_ENV"), globals);
    }

    #[test]
    fn removed_functions_are_unreachable_through_the_string_metatable() {
        for name in ["rep", "dump", "find", "match", "gmatch", "gsub", "pack", "unpack"] {
            assert_eq!(eval(&format!("return type((\"x\").{name})")).unwrap(), "nil", "string method {name}");
        }
    }

    #[test]
    fn escape_attempts_fail() {
        for attack in [
            r#"return io.open("/etc/passwd"):read("a")"#,
            r#"return os.execute("id")"#,
            r#"return os.getenv("HOME")"#,
            r#"return require("os")"#,
            r#"return load("return 1")()"#,
            r#"return debug.getregistry()"#,
            r#"return package.loaded.os"#,
            r#"return dofile("/etc/passwd")"#,
            r#"return _G.io"#,
            r#"return _ENV.io.open"#,
            r#"return string.dump(function() end)"#,
            r#"return ("x"):rep(9)"#,
            r#"return getmetatable("").__index"#,
            r#"return coroutine.wrap(function() end)"#,
            r#"return pcall(error)"#,
        ] {
            let result = eval(attack);
            assert!(matches!(result, Err(SettingsError::Script(_))), "{attack} returned {result:?}");
        }
    }

    #[test]
    fn binary_chunks_are_refused() {
        // A Lua 5.4 bytecode header. It must be refused before any parsing of the content.
        let source = "\x1bLuaT\0\x19\u{93}\r\n\x1a\n";
        assert!(matches!(load(source), Err(SettingsError::Script(_))));
    }

    #[test]
    fn stops_an_infinite_loop() {
        let result = load("while true do end");
        assert_eq!(result.unwrap_err(), SettingsError::LimitExceeded(Limit::Instructions));
    }

    #[test]
    fn instruction_limit_is_configurable_and_exact_enough() {
        let limits = Limits { instructions: 1000, ..Limits::default() };
        let spin = |count: u32| format!("for i = 1, {count} do end return {{ api_version = 1 }}");
        assert!(load_settings(&spin(100), limits).is_ok());
        assert_eq!(load_settings(&spin(100_000), limits).unwrap_err(), SettingsError::LimitExceeded(Limit::Instructions));
    }

    #[test]
    fn stops_unbounded_table_growth() {
        let result = load("local t = {} local i = 0 while true do i = i + 1 t[i] = {} end");
        assert_eq!(result.unwrap_err(), SettingsError::LimitExceeded(Limit::Memory));
    }

    #[test]
    fn stops_string_doubling() {
        let result = load(r#"local s = "x" while true do s = s .. s end"#);
        assert_eq!(result.unwrap_err(), SettingsError::LimitExceeded(Limit::Memory));
    }

    #[test]
    fn stops_table_concat_growth() {
        let result = load(
            r#"local s = "xxxxxxxxxxxxxxxx" while true do s = table.concat({ s, s, s, s }) end"#,
        );
        assert_eq!(result.unwrap_err(), SettingsError::LimitExceeded(Limit::Memory));
    }

    #[test]
    fn memory_limit_is_configurable() {
        let limits = Limits { memory_bytes: 256 * 1024, ..Limits::default() };
        let source = r#"local t = {} for i = 1, 100000 do t[i] = i end return { api_version = 1 }"#;
        assert_eq!(load_settings(source, limits).unwrap_err(), SettingsError::LimitExceeded(Limit::Memory));
        assert!(load(source).is_ok());
    }

    #[test]
    fn expensive_c_calls_hit_the_wall_clock() {
        // Few instructions, but each one copies megabytes in C, so the instruction count alone
        // would take minutes to reach.
        let limits = Limits { wall_time: Duration::from_millis(100), ..Limits::default() };
        let source = r#"local s = "x" for i = 1, 21 do s = s .. s end while true do local t = s .. "y" end"#;
        let started = Instant::now();
        let result = load_settings(source, limits);
        assert_eq!(result.unwrap_err(), SettingsError::LimitExceeded(Limit::WallTime));
        assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
    }

    #[test]
    fn deep_recursion_and_nesting_fail_cleanly() {
        let recursion = load("local function f() return 1 + f() end f()").unwrap_err();
        assert!(matches!(recursion, SettingsError::Script(_) | SettingsError::LimitExceeded(_)), "{recursion:?}");
        let nesting = format!("return {}1{}", "(".repeat(100_000), ")".repeat(100_000));
        assert!(load(&nesting).is_err());
    }

    #[test]
    fn oversized_source_is_refused() {
        assert_eq!(load(&" ".repeat(MAX_SOURCE_LEN + 1)).unwrap_err(), SettingsError::SourceTooLarge);
    }

    #[test]
    fn validator_limits_apply_and_cannot_be_caught() {
        let forever = one_validator("while true do end");
        assert_eq!(
            forever.validate_value("v", &Value::String("x".into())).unwrap_err(),
            SettingsError::LimitExceeded(Limit::Instructions)
        );
        let hog = one_validator("local t = {} local i = 0 while true do i = i + 1 t[i] = {} end");
        assert_eq!(
            hog.validate_value("v", &Value::String("x".into())).unwrap_err(),
            SettingsError::LimitExceeded(Limit::Memory)
        );
    }

    #[test]
    fn validators_see_values_and_report_messages() {
        let source = with_settings(
            r#"{ name = "port", type = "int", label = "P", validate = function(v) return v % 2 == 0, "must be even" end },
               { name = "on", type = "bool", label = "B", validate = function(v) return v == true end },
               { name = "name", type = "string", label = "N",
                 validate = function(v) return nodo.matches(v, "^[a-z]+$"), "letters only" end }"#,
        );
        let loaded = load(&source).unwrap();
        assert_eq!(loaded.validate_value("port", &Value::Int(4)), Ok(()));
        assert_eq!(
            loaded.validate_value("port", &Value::Int(5)),
            Err(SettingsError::Rejected { setting: "port".into(), message: "must be even".into() })
        );
        assert_eq!(loaded.validate_value("on", &Value::Bool(true)), Ok(()));
        assert_eq!(
            loaded.validate_value("on", &Value::Bool(false)),
            Err(SettingsError::Rejected { setting: "on".into(), message: "is not valid".into() })
        );
        assert_eq!(loaded.validate_value("name", &Value::String("abc".into())), Ok(()));
        assert!(loaded.validate_value("name", &Value::String("ab1".into())).is_err());
        assert!(matches!(loaded.validate_value("nope", &Value::Int(1)), Err(SettingsError::UnknownSetting(_))));
        assert!(
            matches!(loaded.validate_value("port", &Value::Bool(true)), Err(SettingsError::Rejected { .. })),
            "the type check runs first"
        );
    }

    #[test]
    fn validators_are_pure_between_calls() {
        let source = r#"
            local calls = 0
            return { api_version = 1, settings = {
                { name = "v", type = "string", label = "V", validate = function()
                    calls = calls + 1
                    leaked = (leaked or 0) + 1
                    string.extra = (string.extra or 0) + 1
                    return calls == 1 and leaked == 1 and string.extra == 1, "state survived"
                end },
            } }"#;
        let loaded = load(source).unwrap();
        for _ in 0..3 {
            assert_eq!(loaded.validate_value("v", &Value::String("x".into())), Ok(()));
        }
    }

    #[test]
    fn validators_must_return_a_boolean() {
        for body in ["", "return nil", "return 1", "return 'true'", "return {}"] {
            let loaded = one_validator(body);
            assert!(
                matches!(loaded.validate_value("v", &Value::String("x".into())), Err(SettingsError::Rejected { .. })),
                "{body:?} must not count as passing"
            );
        }
        let raises = one_validator(r#"error("bad value")"#);
        let Err(SettingsError::Rejected { message, .. }) = raises.validate_value("v", &Value::String("x".into())) else {
            panic!("expected a rejection");
        };
        assert!(message.contains("bad value"), "{message}");
    }

    #[test]
    fn validator_messages_cannot_inject_terminal_escapes() {
        let loaded = one_validator(r#"return false, "\27[2Jgone\nsecond line""#);
        let Err(SettingsError::Rejected { message, .. }) = loaded.validate_value("v", &Value::String("x".into())) else {
            panic!("expected a rejection");
        };
        assert_eq!(message, "[2Jgone");
    }

    #[test]
    fn validators_cannot_leak_secrets_through_messages() {
        let source = with_settings(
            r#"{ name = "key", type = "secret", label = "K", validate = function(v) return false, "got " .. v end },
               { name = "raises", type = "secret", label = "R", validate = function(v) error(v) end }"#,
        );
        let loaded = load(&source).unwrap();
        for name in ["key", "raises"] {
            let error = loaded.validate_value(name, &Value::Secret(crate::SecretString::new("hunter2"))).unwrap_err();
            assert!(!error.to_string().contains("hunter2"), "{error}");
            assert!(!format!("{error:?}").contains("hunter2"));
        }
    }

    #[test]
    fn matches_is_immune_to_catastrophic_patterns() {
        let loaded = one_validator(r#"return nodo.matches(value, "^(a+)+$"), "no match""#);
        let started = Instant::now();
        assert!(loaded.validate_value("v", &Value::String("a".repeat(4000) + "b")).is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
        let bad_pattern = one_validator(r#"return nodo.matches(value, "(")"#);
        assert!(bad_pattern.validate_value("v", &Value::String("x".into())).is_err());
    }

    #[test]
    fn default_failing_its_own_validator_is_rejected() {
        let issues = invalid(&with_settings(
            r#"{ name = "v", type = "int", label = "V", default = 3, validate = function(v) return v > 5, "too small" end }"#,
        ));
        assert!(issues[0].contains("too small"), "{issues:?}");
    }

    #[test]
    fn reports_every_schema_problem() {
        let issues = invalid(&with_settings(
            r#"{ name = "Bad Name", type = "string", label = "x" },
               { name = "no_label", type = "string" },
               { name = "no_type", label = "x" },
               { name = "bad_type", type = "float", label = "x" },
               { name = "typo", type = "string", label = "x", pubilc = "y" },
               { name = "dup", type = "string", label = "x" },
               { name = "dup", type = "string", label = "x" },
               "not a table""#,
        ));
        for expected in [
            "settings[1].name",
            "settings[2]: has no label",
            "settings[3]: has no type",
            "settings[4].type",
            "unknown key 'pubilc'",
            "'dup': is declared twice",
            "settings[8]: must be a table",
        ] {
            assert!(issues.iter().any(|i| i.contains(expected)), "missing {expected:?} in {issues:#?}");
        }
    }

    #[test]
    fn rejects_inconsistent_declarations() {
        for (body, expected) in [
            (r#"{ name = "a", type = "secret", label = "x", public = "a" }"#, "never be public"),
            (r#"{ name = "a", type = "secret", label = "x", default = "hunter2" }"#, "single reference"),
            (r#"{ name = "a", type = "string", label = "x", default = 5 }"#, "must be of type string"),
            (r#"{ name = "a", type = "int", label = "x", default = "abc" }"#, "whole number"),
            (r#"{ name = "a", type = "int", label = "x", default = 1.5 }"#, "cannot be a number"),
            (r#"{ name = "a", type = "int", label = "x", default = 5, min = 10 }"#, "between"),
            (r#"{ name = "a", type = "int", label = "x", min = 10, max = 1 }"#, "greater than max"),
            (r#"{ name = "a", type = "string", label = "x", min = 1 }"#, "only apply to type int"),
            (r#"{ name = "a", type = "string", label = "x", choices = { "a" } }"#, "only apply to type enum"),
            (r#"{ name = "a", type = "enum", label = "x" }"#, "list of strings"),
            (r#"{ name = "a", type = "enum", label = "x", choices = {} }"#, "between 1 and"),
            (r#"{ name = "a", type = "enum", label = "x", choices = { "a", "a" } }"#, "duplicate"),
            (r#"{ name = "a", type = "enum", label = "x", choices = { "a" }, default = "b" }"#, "must be one of"),
            (r#"{ name = "a", type = "url", label = "x", default = "ftp://a" }"#, "must use http"),
            (r#"{ name = "a", type = "string", label = "x", default = "$(unterminated" }"#, "closing"),
            (r#"{ name = "a", type = "string", label = "x", required = "yes" }"#, "must be a boolean"),
            (r#"{ name = "a", type = "string", label = "x", validate = "code" }"#, "must be a function"),
            (r#"{ name = "a", type = "string", label = "x", endpoint = "tcp" }"#, "cannot be tcp"),
            (r#"{ name = "a", type = "int", label = "x", endpoint = "unix" }"#, "cannot be unix"),
            (r#"{ name = "a", type = "int", label = "x", endpoint = "icmp" }"#, "must be tcp"),
            (r#"{ name = "a", type = "string", label = "\27[2J" }"#, "plain text"),
            (r#"{ name = "a", type = "string", label = "x", help = "\27[2J" }"#, "plain text"),
            (r#"{ name = "a", type = "string", label = "x", public = "Bad Name" }"#, "lowercase"),
        ] {
            let issues = invalid(&with_settings(body));
            assert!(issues.iter().any(|i| i.contains(expected)), "{body}: wanted {expected:?}, got {issues:?}");
        }
    }

    #[test]
    fn secrets_can_ask_the_device_to_generate_them() {
        let loaded =
            load(&with_settings(r#"{ name = "k", type = "secret", label = "K", required = true, generate = { hex = 32 } }"#))
                .unwrap();
        assert_eq!(loaded.schema().setting("k").unwrap().generate, Some(Generate::Hex { bytes: 32 }));
        for (body, expected) in [
            (r#"{ name = "a", type = "string", label = "x", generate = { hex = 32 } }"#, "only applies to type secret"),
            (r#"{ name = "a", type = "secret", label = "x", default = "$(o.k)", generate = { hex = 32 } }"#, "default"),
            (r#"{ name = "a", type = "secret", label = "x", generate = { hex = 8 } }"#, "between 16 and 64"),
            (r#"{ name = "a", type = "secret", label = "x", generate = { hex = 65 } }"#, "between 16 and 64"),
            (r#"{ name = "a", type = "secret", label = "x", generate = { hex = 1.5 } }"#, "must be an integer"),
            (r#"{ name = "a", type = "secret", label = "x", generate = { base64 = 32 } }"#, "unknown key"),
            (r#"{ name = "a", type = "secret", label = "x", generate = {} }"#, "how many bytes"),
            (r#"{ name = "a", type = "secret", label = "x", generate = "hex" }"#, "must be a table"),
        ] {
            let issues = invalid(&with_settings(body));
            assert!(issues.iter().any(|i| i.contains(expected)), "{body}: wanted {expected:?}, got {issues:?}");
        }
    }

    #[test]
    fn public_names_cannot_be_confused_with_other_settings() {
        let issues = invalid(&with_settings(
            r#"{ name = "a", type = "string", label = "x", public = "b" },
               { name = "b", type = "secret", label = "x" }"#,
        ));
        assert!(issues.iter().any(|i| i.contains("another setting's name")), "{issues:?}");
        let issues = invalid(&with_settings(
            r#"{ name = "a", type = "string", label = "x", public = "same" },
               { name = "b", type = "string", label = "x", public = "same" }"#,
        ));
        assert!(issues.iter().any(|i| i.contains("reuses a public name")), "{issues:?}");
        assert!(load(&with_settings(r#"{ name = "a", type = "string", label = "x", public = "a" }"#)).is_ok());
    }

    #[test]
    fn a_secret_may_default_to_a_reference() {
        assert!(load(&with_settings(r#"{ name = "a", type = "secret", label = "x", default = "$(other.key)" }"#)).is_ok());
    }

    #[test]
    fn non_table_results_are_refused() {
        for source in ["return 1", "return nil", "return 'x'", "return function() end", ""] {
            assert!(matches!(load(source), Err(SettingsError::Invalid(_))), "{source:?}");
        }
    }

    #[test]
    fn syntax_errors_are_reported() {
        assert!(matches!(load("return {"), Err(SettingsError::Script(_))));
    }
}
