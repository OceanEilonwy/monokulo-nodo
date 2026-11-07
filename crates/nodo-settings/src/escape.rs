//! Writing resolved values into configuration formats.
//!
//! Values pass through an encoder for the output format and nothing else. Nothing here runs a
//! shell, but env files are still quoted to be inert under POSIX shell rules, so one sourced by
//! mistake cannot execute anything.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::schema::{is_valid_setting_name, Value};

/// Keeps generated names out of the variables that change how a program loads or runs, such as
/// `LD_PRELOAD` and `PATH`, whatever a package names its settings.
const ENV_PREFIX: &str = "NODO_";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EnvError {
    #[error("'{0}' cannot be used as an environment variable name")]
    InvalidName(String),
    #[error("settings '{0}' and '{1}' map to the same environment variable")]
    NameCollision(String, String),
    #[error("the value of '{0}' contains a character an environment file cannot hold safely")]
    UnsafeValue(String),
}

/// The variable name for a setting: `rpc-port` becomes `NODO_RPC_PORT`.
pub fn env_name(setting: &str) -> Result<String, EnvError> {
    if !is_valid_setting_name(setting) {
        return Err(EnvError::InvalidName(setting.to_string()));
    }
    let upper: String = setting.chars().map(|c| if c == '-' { '_' } else { c.to_ascii_uppercase() }).collect();
    Ok(format!("{ENV_PREFIX}{upper}"))
}

/// Environment pairs for passing directly to `execve`, with no file or shell in between.
pub fn env_pairs(values: &BTreeMap<String, Value>) -> Result<Vec<(String, String)>, EnvError> {
    let mut seen: BTreeMap<String, &String> = BTreeMap::new();
    let mut pairs = Vec::with_capacity(values.len());
    for (setting, value) in values {
        let name = env_name(setting)?;
        if let Some(other) = seen.insert(name.clone(), setting) {
            return Err(EnvError::NameCollision(other.clone(), setting.clone()));
        }
        let text = value.text();
        if text.contains('\0') {
            return Err(EnvError::UnsafeValue(setting.clone()));
        }
        pairs.push((name, text));
    }
    Ok(pairs)
}

/// A file of `NAME='value'` lines. Line oriented readers cannot represent a line break, so a value
/// holding any control character other than a tab is an error rather than a chance to inject a
/// second variable.
pub fn env_file(values: &BTreeMap<String, Value>) -> Result<String, EnvError> {
    let names: BTreeMap<String, &String> = values.keys().map(|k| Ok((env_name(k)?, k))).collect::<Result<_, _>>()?;
    let mut out = String::new();
    for (name, value) in env_pairs(values)? {
        if value.chars().any(|c| c.is_control() && c != '\t') {
            return Err(EnvError::UnsafeValue(names[&name].clone()));
        }
        out.push_str(&name);
        out.push_str("='");
        out.push_str(&value.replace('\'', r"'\''"));
        out.push_str("'\n");
    }
    Ok(out)
}

/// A value as TOML. The TOML library does the encoding, because hand written string escaping is
/// the classic source of config injection.
pub fn toml_value(value: &Value) -> toml::Value {
    match value {
        Value::Int(number) => toml::Value::Integer(*number),
        Value::Bool(flag) => toml::Value::Boolean(*flag),
        text => toml::Value::String(text.text()),
    }
}

/// A flat TOML document mapping setting names to values.
pub fn toml_file(values: &BTreeMap<String, Value>) -> Result<String, toml::ser::Error> {
    let table: toml::Table = values.iter().map(|(name, value)| (name.clone(), toml_value(value))).collect();
    toml::to_string(&table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::SecretString;
    use std::process::Command;

    fn values(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
        pairs.iter().map(|(name, value)| (name.to_string(), value.clone())).collect()
    }

    const INJECTIONS: &[&str] = &[
        "plain",
        "",
        "\"",
        "\\",
        "\\\"",
        "'",
        "'''",
        "\"\"\"",
        "x\"\nadmin = true\ny = \"",
        "x'\nadmin = true\ny = '",
        "x\"\r\n[other]\r\nadmin = true",
        "\\u0000",
        "#comment",
        "a = 1",
        "[table]",
        "$(touch pwned)",
        "`touch pwned`",
        "${HOME}",
        "; touch pwned",
        "| touch pwned",
        "&& touch pwned",
        "' ; touch pwned ; '",
        "'$(touch pwned)'",
        "\ttabbed",
        "é✓",
        "\u{7f}",
        "\u{2028}",
        "\u{1b}[2J",
    ];

    #[test]
    fn env_names_are_prefixed_and_valid() {
        assert_eq!(env_name("rpc-port").unwrap(), "NODO_RPC_PORT");
        assert_eq!(env_name("ld_preload").unwrap(), "NODO_LD_PRELOAD");
        assert_eq!(env_name("path").unwrap(), "NODO_PATH");
        for bad in ["", "A", "a b", "a=b", "a\nb", "1a", "a.b", "a$b"] {
            assert!(env_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn env_names_cannot_collide() {
        let input = values(&[("a-b", Value::Int(1)), ("a_b", Value::Int(2))]);
        assert!(matches!(env_pairs(&input), Err(EnvError::NameCollision(_, _))));
        assert!(matches!(env_file(&input), Err(EnvError::NameCollision(_, _))));
    }

    #[test]
    fn env_pairs_refuse_nul() {
        let input = values(&[("a", Value::Secret(SecretString::new("x\0y")))]);
        assert_eq!(env_pairs(&input), Err(EnvError::UnsafeValue("a".into())));
    }

    #[test]
    fn env_file_refuses_line_breaks_and_escape_sequences() {
        for text in ["x\ny", "x\rNODO_ADMIN=1", "x\n", "\u{1b}[2J", "\u{7f}", "x\0"] {
            let input = values(&[("a", Value::Secret(SecretString::new(text)))]);
            assert_eq!(env_file(&input), Err(EnvError::UnsafeValue("a".into())), "{text:?}");
        }
    }

    #[test]
    fn env_file_quotes_values() {
        let input = values(&[("a", Value::String("it's".into())), ("b", Value::Int(5)), ("c", Value::Bool(true))]);
        assert_eq!(env_file(&input).unwrap(), "NODO_A='it'\\''s'\nNODO_B='5'\nNODO_C='true'\n");
    }

    /// The real proof that the quoting is inert: source the output in a POSIX shell and confirm
    /// the value comes back unchanged and nothing ran. Production code never does this.
    #[test]
    fn env_file_is_inert_when_sourced_by_a_shell() {
        let dir = std::env::temp_dir().join(format!("nodo-settings-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (index, text) in INJECTIONS.iter().enumerate() {
            let input = values(&[("a", Value::Secret(SecretString::new(*text)))]);
            let Ok(file) = env_file(&input) else {
                assert!(text.chars().any(|c| c.is_control() && c != '\t'), "{text:?} was refused");
                continue;
            };
            let path = dir.join(format!("case-{index}.env"));
            std::fs::write(&path, file).unwrap();
            let output = Command::new("sh")
                .current_dir(&dir)
                .args(["-c", ". \"$1\"; printf %s \"$NODO_A\"", "sh"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(output.status.success(), "{text:?}");
            assert_eq!(String::from_utf8(output.stdout).unwrap(), *text);
        }
        let pwned = dir.join("pwned").exists();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(!pwned, "an injected command ran");
    }

    #[test]
    fn toml_output_cannot_be_injected() {
        for text in INJECTIONS {
            for value in [Value::String(text.to_string()), Value::Secret(SecretString::new(*text))] {
                let file = toml_file(&values(&[("a", value)])).unwrap();
                let parsed: toml::Table = toml::from_str(&file).unwrap_or_else(|e| panic!("{text:?}: {e}\n{file}"));
                assert_eq!(parsed.len(), 1, "{text:?} added keys: {file}");
                assert_eq!(parsed["a"].as_str(), Some(*text));
            }
        }
    }

    #[test]
    fn toml_keeps_types() {
        let input = values(&[("a", Value::Int(-5)), ("b", Value::Bool(false)), ("c", Value::Url("http://a/".into()))]);
        let parsed: toml::Table = toml::from_str(&toml_file(&input).unwrap()).unwrap();
        assert_eq!(parsed["a"].as_integer(), Some(-5));
        assert_eq!(parsed["b"].as_bool(), Some(false));
        assert_eq!(parsed["c"].as_str(), Some("http://a/"));
    }
}
