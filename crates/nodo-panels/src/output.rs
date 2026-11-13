//! What a panel function returns, read out of Lua into plain values the screen can draw.
//!
//! The reader is strict: an unknown field, a wrong type or an out of range value fails the whole
//! run, so a package's mistake shows as a failed panel rather than as something drawn wrongly.

use mlua::{Table, Value};
use nodo_settings::mlua;

pub const MAX_BIG: usize = 16;
pub const MAX_UNIT: usize = 16;
pub const MAX_ROWS: usize = 6;
pub const MAX_LABEL: usize = 16;
pub const MAX_VALUE: usize = 40;
pub const MAX_SERIES: usize = 120;
pub const MAX_NOTE: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Busy,
    Warn,
    Error,
    Idle,
}

impl Status {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "ok" => Status::Ok,
            "busy" => Status::Busy,
            "warn" => Status::Warn,
            "error" => Status::Error,
            "idle" => Status::Idle,
            _ => return None,
        })
    }
}

/// One run's result.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PanelOutput {
    pub big: Option<String>,
    pub unit: Option<String>,
    /// From 0 to 1.
    pub bar: Option<f64>,
    pub rows: Vec<(String, String)>,
    pub series: Vec<f64>,
    pub sample: Option<f64>,
    pub status: Option<Status>,
    pub note: Option<String>,
}

const FIELDS: &[&str] = &["big", "unit", "bar", "rows", "series", "sample", "status", "note"];

/// Reads the table a panel function returned.
pub fn read_output(table: &Table) -> Result<PanelOutput, String> {
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, _) = pair.map_err(|e| e.to_string())?;
        let known = matches!(&key, Value::String(name) if FIELDS.iter().any(|f| name.as_bytes().as_ref() == f.as_bytes()));
        if !known {
            return Err(format!("unknown field {}", describe_key(&key)));
        }
    }
    let get = |name: &str| table.raw_get::<Value>(name).map_err(|e| e.to_string());
    let mut out = PanelOutput {
        big: text_field(get("big")?, "big", MAX_BIG)?,
        unit: text_field(get("unit")?, "unit", MAX_UNIT)?,
        note: text_field(get("note")?, "note", MAX_NOTE)?,
        sample: number_field(get("sample")?, "sample")?,
        ..PanelOutput::default()
    };
    if let Some(bar) = number_field(get("bar")?, "bar")? {
        if !(0.0..=1.0).contains(&bar) {
            return Err("bar must be from 0 to 1".into());
        }
        out.bar = Some(bar);
    }
    match get("status")? {
        Value::Nil => {}
        Value::String(text) => {
            let text = text.to_str().map_err(|_| "status is not text")?;
            out.status = Some(Status::parse(&text).ok_or("status must be ok, busy, warn, error or idle")?);
        }
        other => return Err(format!("status must be text, not {}", other.type_name())),
    }
    match get("rows")? {
        Value::Nil => {}
        Value::Table(rows) => {
            let entries = sequence(&rows, "rows", MAX_ROWS)?;
            for (index, entry) in entries.into_iter().enumerate() {
                let Value::Table(pair) = entry else {
                    return Err(format!("rows[{}] must be {{ label, value }}", index + 1));
                };
                let parts = sequence(&pair, "a row", 2)?;
                if parts.len() != 2 {
                    return Err(format!("rows[{}] must be {{ label, value }}", index + 1));
                }
                let mut parts = parts.into_iter();
                let label = row_text(parts.next(), MAX_LABEL, index)?;
                let value = row_text(parts.next(), MAX_VALUE, index)?;
                out.rows.push((label, value));
            }
        }
        other => return Err(format!("rows must be a list, not {}", other.type_name())),
    }
    match get("series")? {
        Value::Nil => {}
        Value::Table(series) => {
            for (index, value) in sequence(&series, "series", MAX_SERIES)?.into_iter().enumerate() {
                out.series.push(finite(&value).ok_or_else(|| format!("series[{}] must be a number", index + 1))?);
            }
        }
        other => return Err(format!("series must be a list, not {}", other.type_name())),
    }
    Ok(out)
}

fn describe_key(key: &Value) -> String {
    match key {
        Value::String(name) => format!("'{}'", clean(&name.to_string_lossy(), 24)),
        other => format!("of type {}", other.type_name()),
    }
}

/// A Lua sequence `{ a, b, c }`, refusing other keys and more than `max` items.
fn sequence(table: &Table, what: &str, max: usize) -> Result<Vec<Value>, String> {
    let mut count = 0usize;
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, _) = pair.map_err(|e| e.to_string())?;
        if !matches!(key, Value::Integer(_)) {
            return Err(format!("{what} must be a list"));
        }
        count += 1;
        if count > max {
            return Err(format!("{what} has more than {max} items"));
        }
    }
    let mut items = Vec::with_capacity(count);
    for index in 1..=count {
        let value: Value = table.raw_get(index).map_err(|e| e.to_string())?;
        if value.is_nil() {
            return Err(format!("{what} must be a list without gaps"));
        }
        items.push(value);
    }
    Ok(items)
}

fn finite(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Integer(n) => *n as f64,
        Value::Number(n) => *n,
        _ => return None,
    };
    number.is_finite().then_some(number)
}

fn number_field(value: Value, name: &str) -> Result<Option<f64>, String> {
    match value {
        Value::Nil => Ok(None),
        other => finite(&other).map(Some).ok_or_else(|| format!("{name} must be a finite number")),
    }
}

fn text_field(value: Value, name: &str, max: usize) -> Result<Option<String>, String> {
    match value {
        Value::Nil => Ok(None),
        Value::String(text) => checked_text(&text.to_string_lossy(), max).map(Some).map_err(|problem| format!("{name} {problem}")),
        // Numbers are common for values, so they are accepted and shown as Lua would print them.
        Value::Integer(n) => checked_text(&n.to_string(), max).map(Some).map_err(|problem| format!("{name} {problem}")),
        other => Err(format!("{name} must be text, not {}", other.type_name())),
    }
}

fn row_text(value: Option<Value>, max: usize, index: usize) -> Result<String, String> {
    match value {
        Some(Value::String(text)) => checked_text(&text.to_string_lossy(), max),
        Some(Value::Integer(n)) => checked_text(&n.to_string(), max),
        Some(Value::Number(n)) if n.is_finite() => checked_text(&n.to_string(), max),
        _ => Err("must be text".into()),
    }
    .map_err(|problem| format!("rows[{}] {problem}", index + 1))
}

/// One line of printable text, so nothing a package returns can move the cursor or recolour the
/// console.
fn checked_text(text: &str, max: usize) -> Result<String, String> {
    if text.chars().any(char::is_control) {
        return Err("must not contain control characters".into());
    }
    if text.chars().count() > max {
        return Err(format!("is longer than {max} characters"));
    }
    Ok(text.to_string())
}

/// For error messages built from package text.
pub(crate) fn clean(text: &str, max: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max).collect()
}
