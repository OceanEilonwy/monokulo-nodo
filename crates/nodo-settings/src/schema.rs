//! The declarative result of a settings file, and the typed values settings hold.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::monero::{check_monero_address, MoneroNetwork};
use crate::template::{literal_text, parse_template};
use crate::url::check_url;

/// The only settings API version this build understands.
pub const SETTINGS_API_VERSION: u32 = 1;

/// Longest string or secret value, which also bounds how far repeated references can expand.
pub const MAX_TEXT_LEN: usize = 4096;
const MAX_SETTING_NAME_LEN: usize = 63;

pub fn is_valid_setting_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_SETTING_NAME_LEN
        && bytes[0].is_ascii_lowercase()
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SettingType {
    String,
    Int {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<i64>,
    },
    Bool,
    Enum {
        choices: Vec<String>,
    },
    MoneroAddress,
    Url,
    Secret,
}

impl SettingType {
    pub fn name(&self) -> &'static str {
        match self {
            SettingType::String => "string",
            SettingType::Int { .. } => "int",
            SettingType::Bool => "bool",
            SettingType::Enum { .. } => "enum",
            SettingType::MoneroAddress => "monero-address",
            SettingType::Url => "url",
            SettingType::Secret => "secret",
        }
    }

    /// Checks a value of any origin against this type. Messages never quote the value.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        match (self, value) {
            (SettingType::String, Value::String(text)) => {
                if text.len() > MAX_TEXT_LEN {
                    return Err(format!("is longer than {MAX_TEXT_LEN} bytes"));
                }
                if text.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
                    return Err("contains a control character".to_string());
                }
                Ok(())
            }
            (SettingType::Int { min, max }, Value::Int(number)) => {
                if min.is_some_and(|min| *number < min) || max.is_some_and(|max| *number > max) {
                    return Err(format!("must be between {} and {}", bound(min, "any"), bound(max, "any")));
                }
                Ok(())
            }
            (SettingType::Bool, Value::Bool(_)) => Ok(()),
            (SettingType::Enum { choices }, Value::Enum(choice)) => {
                if !choices.contains(choice) {
                    return Err(format!("must be one of: {}", choices.join(", ")));
                }
                Ok(())
            }
            (SettingType::MoneroAddress, Value::MoneroAddress(address)) => match check_monero_address(address)? {
                MoneroNetwork::Mainnet => Ok(()),
                _ => Err("is not a mainnet address".to_string()),
            },
            (SettingType::Url, Value::Url(url)) => check_url(url).map(|_| ()).map_err(str::to_string),
            (SettingType::Secret, Value::Secret(secret)) => {
                if secret.expose().len() > MAX_TEXT_LEN {
                    return Err(format!("is longer than {MAX_TEXT_LEN} bytes"));
                }
                if secret.expose().contains('\0') {
                    return Err("contains a NUL byte".to_string());
                }
                Ok(())
            }
            _ => Err(format!("must be of type {}, not {}", self.name(), value.type_name())),
        }
    }

    /// Reads literal text, as typed by a user or written in a package, as a value of this type.
    pub fn parse_literal(&self, text: &str) -> Result<Value, String> {
        let value = match self {
            SettingType::String => Value::String(text.to_string()),
            SettingType::Int { .. } => {
                let digits = text.strip_prefix('-').unwrap_or(text);
                if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err("must be a whole number".to_string());
                }
                Value::Int(text.parse().map_err(|_| "is out of range".to_string())?)
            }
            SettingType::Bool => match text {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => return Err("must be true or false".to_string()),
            },
            SettingType::Enum { .. } => Value::Enum(text.to_string()),
            SettingType::MoneroAddress => Value::MoneroAddress(text.to_string()),
            SettingType::Url => Value::Url(text.to_string()),
            SettingType::Secret => Value::Secret(SecretString::new(text)),
        };
        self.check(&value)?;
        Ok(value)
    }
}

fn bound(limit: &Option<i64>, none: &str) -> String {
    limit.map_or_else(|| none.to_string(), |n| n.to_string())
}

/// A secret string. Formatting never reveals it, so it cannot slip into a log or error message.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(text: impl Into<String>) -> Self {
        SecretString(text.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// A fully resolved, typed setting value. Not serializable, so secrets only leave through the
/// explicit output functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    String(String),
    Int(i64),
    Bool(bool),
    Enum(String),
    MoneroAddress(String),
    Url(String),
    Secret(SecretString),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => "string",
            Value::Int(_) => "int",
            Value::Bool(_) => "bool",
            Value::Enum(_) => "enum",
            Value::MoneroAddress(_) => "monero-address",
            Value::Url(_) => "url",
            Value::Secret(_) => "secret",
        }
    }

    /// The value as plain text, including a secret's content. Callers decide where it may go.
    pub(crate) fn text(&self) -> String {
        match self {
            Value::String(text) | Value::Enum(text) | Value::MoneroAddress(text) | Value::Url(text) => text.clone(),
            Value::Int(number) => number.to_string(),
            Value::Bool(flag) => flag.to_string(),
            Value::Secret(secret) => secret.expose().to_string(),
        }
    }
}

/// A package's default. Text may hold references. Secrets can only default to a reference, because
/// a literal default would be the same known value on every device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DefaultValue {
    Text(String),
    Int(i64),
    Bool(bool),
}

/// Network access that referencing a setting grants to the package holding the reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Endpoint {
    /// The setting is a port number or a loopback URL.
    Tcp,
    Udp,
    /// The setting is `unix:/run/apps/<id>/<name>.sock`.
    Unix,
}

/// Fewest random bytes a generated secret may have.
pub const MIN_GENERATED_BYTES: u8 = 16;
/// Most random bytes a generated secret may have.
pub const MAX_GENERATED_BYTES: u8 = 64;

/// How the device creates a secret's value when the package is installed and has none.
///
/// The value is made once and then kept like any other setting. Some programs can never change a
/// key once data has been written with it, so nothing here ever regenerates a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Generate {
    /// This many random bytes, written as lowercase hex.
    Hex { bytes: u8 },
}

impl Generate {
    /// How many random bytes [`Generate::encode`] needs.
    pub fn byte_len(&self) -> usize {
        match self {
            Generate::Hex { bytes } => usize::from(*bytes),
        }
    }

    /// Turns random bytes from the caller into the secret. The caller supplies the randomness so
    /// this crate stays free of I/O.
    pub fn encode(&self, random: &[u8]) -> Result<SecretString, String> {
        if random.len() != self.byte_len() {
            return Err(format!("needs {} random bytes, got {}", self.byte_len(), random.len()));
        }
        match self {
            Generate::Hex { .. } => {
                let mut text = String::with_capacity(random.len() * 2);
                for byte in random {
                    text.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
                    text.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
                }
                Ok(SecretString::new(text))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingDef {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: SettingType,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<DefaultValue>,
    /// The name other packages reference this setting by. Absent means not public.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<Endpoint>,
    /// Whether the settings file supplies a validation function for this setting.
    #[serde(default)]
    pub has_validator: bool,
    /// Only on secrets, and never together with a default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generate: Option<Generate>,
}

/// What a settings file declares. Plain data, so root code can use it without running any Lua.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Schema {
    pub api_version: u32,
    pub settings: Vec<SettingDef>,
    /// Settings an earlier version declared and this one removed on purpose. A value stored
    /// for one of them is dropped rather than refused; a value for a setting that is neither
    /// declared nor retired is still an error.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired: Vec<String>,
}

impl Schema {
    /// An empty schema for packages with no settings file.
    pub fn empty() -> Self {
        Schema { api_version: SETTINGS_API_VERSION, settings: Vec::new(), retired: Vec::new() }
    }

    pub fn setting(&self, name: &str) -> Option<&SettingDef> {
        self.settings.iter().find(|s| s.name == name)
    }

    pub fn exported(&self, public_name: &str) -> Option<&SettingDef> {
        self.settings.iter().find(|s| s.public.as_deref() == Some(public_name))
    }

    /// Whether this version removed the setting on purpose, so a stored value for it is dropped.
    pub fn is_retired(&self, name: &str) -> bool {
        self.setting(name).is_none() && self.retired.iter().any(|r| r == name)
    }
}

/// A setting's stored value before resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingValue {
    /// Used as is. Never scanned for references.
    Literal(Value),
    /// Text that may contain references and escapes.
    Template(String),
}

impl SettingValue {
    /// Interprets what a user typed. A secret is always literal, so a password that happens to
    /// contain `$(` stays a password rather than becoming a reference or a syntax error.
    pub fn from_user_input(def: &SettingDef, text: &str) -> Result<Self, String> {
        if matches!(def.ty, SettingType::Secret) {
            return def.ty.parse_literal(text).map(SettingValue::Literal);
        }
        let parts = parse_template(text).map_err(|e| e.to_string())?;
        match literal_text(&parts) {
            Some(literal) => def.ty.parse_literal(&literal).map(SettingValue::Literal),
            None => Ok(SettingValue::Template(text.to_string())),
        }
    }
}

impl From<&DefaultValue> for SettingValue {
    fn from(default: &DefaultValue) -> Self {
        match default {
            DefaultValue::Text(text) => SettingValue::Template(text.clone()),
            DefaultValue::Int(number) => SettingValue::Literal(Value::Int(*number)),
            DefaultValue::Bool(flag) => SettingValue::Literal(Value::Bool(*flag)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monero::encode_address;

    fn def(ty: SettingType) -> SettingDef {
        SettingDef {
            name: "x".into(),
            ty,
            label: "X".into(),
            help: None,
            required: false,
            default: None,
            public: None,
            endpoint: None,
            has_validator: false,
            generate: None,
        }
    }

    #[test]
    fn type_checks_reject_the_wrong_kind() {
        assert!(SettingType::Bool.check(&Value::Int(1)).is_err());
        assert!(SettingType::Url.check(&Value::String("http://a".into())).is_err());
        assert!(SettingType::String.check(&Value::Secret(SecretString::new("x"))).is_err());
    }

    #[test]
    fn literals_are_parsed_strictly() {
        let int = SettingType::Int { min: Some(1), max: Some(10) };
        assert_eq!(int.parse_literal("5"), Ok(Value::Int(5)));
        for bad in ["", "0", "11", "5.0", "0x5", "+5", " 5", "5 ", "99999999999999999999", "-"] {
            assert!(int.parse_literal(bad).is_err(), "{bad:?}");
        }
        assert!(SettingType::Bool.parse_literal("yes").is_err());
        let choice = SettingType::Enum { choices: vec!["a".into()] };
        assert!(choice.parse_literal("a").is_ok());
        assert!(choice.parse_literal("b").is_err());
    }

    #[test]
    fn strings_refuse_control_characters_and_excess_length() {
        assert!(SettingType::String.parse_literal("line\nline\ttab").is_ok());
        assert!(SettingType::String.parse_literal("nul\0").is_err());
        assert!(SettingType::String.parse_literal("esc\x1b[2J").is_err());
        assert!(SettingType::String.parse_literal(&"x".repeat(MAX_TEXT_LEN + 1)).is_err());
        assert!(SettingType::Secret.parse_literal("nul\0").is_err());
    }

    #[test]
    fn only_mainnet_addresses_are_accepted() {
        assert!(SettingType::MoneroAddress.parse_literal(&encode_address(18, 64)).is_ok());
        assert!(SettingType::MoneroAddress.parse_literal(&encode_address(24, 64)).is_err());
        assert!(SettingType::MoneroAddress.parse_literal("not an address").is_err());
    }

    #[test]
    fn secrets_are_redacted_when_formatted() {
        let value = Value::Secret(SecretString::new("hunter2"));
        assert!(!format!("{value:?}").contains("hunter2"));
        let stored = SettingValue::Literal(value);
        assert!(!format!("{stored:?}").contains("hunter2"));
    }

    #[test]
    fn typed_secret_input_is_never_a_reference() {
        let secret = def(SettingType::Secret);
        let value = SettingValue::from_user_input(&secret, "pa$(ss").unwrap();
        assert_eq!(value, SettingValue::Literal(Value::Secret(SecretString::new("pa$(ss"))));
        let value = SettingValue::from_user_input(&secret, "$(other.secret)").unwrap();
        assert!(matches!(value, SettingValue::Literal(Value::Secret(_))));
    }

    #[test]
    fn typed_input_recognises_references_and_escapes() {
        let string = def(SettingType::String);
        assert_eq!(
            SettingValue::from_user_input(&string, "$(a.b)").unwrap(),
            SettingValue::Template("$(a.b)".into())
        );
        assert_eq!(
            SettingValue::from_user_input(&string, "$$(a.b)").unwrap(),
            SettingValue::Literal(Value::String("$(a.b)".into()))
        );
        assert!(SettingValue::from_user_input(&string, "$(a").is_err());
        let int = def(SettingType::Int { min: None, max: None });
        assert_eq!(SettingValue::from_user_input(&int, "7").unwrap(), SettingValue::Literal(Value::Int(7)));
    }

    #[test]
    fn generated_secrets_are_hex_of_exactly_the_declared_length() {
        let generate = Generate::Hex { bytes: 16 };
        let secret = generate.encode(&[0x00, 0x0f, 0xa5, 0xff, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]).unwrap();
        assert_eq!(secret.expose(), "000fa5ff0102030405060708090a0b0c");
        assert!(generate.encode(&[0; 15]).is_err());
        assert!(generate.encode(&[0; 17]).is_err());
        assert!(!format!("{secret:?}").contains("000f"));
    }

    #[test]
    fn schema_round_trips_through_toml() {
        let mut enumerated = def(SettingType::Enum { choices: vec!["a".into(), "b".into()] });
        enumerated.default = Some(DefaultValue::Text("a".into()));
        let mut number = def(SettingType::Int { min: Some(1), max: None });
        number.default = Some(DefaultValue::Int(4));
        let mut key = def(SettingType::Secret);
        key.generate = Some(Generate::Hex { bytes: 32 });
        let schema = Schema { api_version: 1, settings: vec![enumerated, number, key], retired: Vec::new() };
        let text = toml::to_string(&schema).unwrap();
        assert_eq!(toml::from_str::<Schema>(&text).unwrap(), schema);
    }
}
