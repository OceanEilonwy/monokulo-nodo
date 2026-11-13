//! Running a panel's Lua function in the settings sandbox.

use std::time::Duration;

use mlua::{Function, Lua, Table, Value as LuaValue};
use nodo_settings::{mlua, Limit, Limits, Sandbox, SettingsError};
use serde_json::Value as Json;

use crate::output::{read_output, PanelOutput};

/// Tighter than settings: a panel runs every few seconds on a small board.
pub const PANEL_LIMITS: Limits =
    Limits { memory_bytes: 4 * 1024 * 1024, instructions: 1_000_000, wall_time: Duration::from_millis(200) };

/// JSON deeper than this is refused rather than converted.
pub const MAX_DEPTH: usize = 32;
/// Values in one piece of data, counting every object, array, string and number.
pub const MAX_NODES: usize = 50_000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    #[error("{0}")]
    Script(String),
    #[error("exceeded its {0} limit")]
    Limit(&'static str),
    #[error("returned {0}")]
    Output(String),
    #[error("data {0}")]
    Data(String),
}

fn from_settings(error: SettingsError) -> RunError {
    match error {
        SettingsError::LimitExceeded(Limit::Memory) => RunError::Limit("memory"),
        SettingsError::LimitExceeded(Limit::Instructions) => RunError::Limit("instruction"),
        SettingsError::LimitExceeded(Limit::WallTime) => RunError::Limit("time"),
        SettingsError::SourceTooLarge => RunError::Script("the panel file is too large".into()),
        SettingsError::Invalid(issues) => RunError::Script(issues.join("; ")),
        other => RunError::Script(other.to_string()),
    }
}

/// Loads `source`, calls its `panel(data, history)` and reads what it returns. Every call gets a
/// new Lua state, so nothing survives from one run to the next.
pub fn run_panel(source: &str, name: &str, data: &Json, history: &[f64]) -> Result<PanelOutput, RunError> {
    run_panel_with(source, name, data, history, PANEL_LIMITS)
}

pub fn run_panel_with(
    source: &str,
    name: &str,
    data: &Json,
    history: &[f64],
    limits: Limits,
) -> Result<PanelOutput, RunError> {
    check_shape(data)?;
    let sandbox = Sandbox::new(limits).map_err(from_settings)?;
    let module = sandbox.eval_table(source, name).map_err(from_settings)?;
    let panel: Function = match module.raw_get::<LuaValue>("panel") {
        Ok(LuaValue::Function(f)) => f,
        _ => return Err(RunError::Script("the file must return a table with a panel function".into())),
    };
    let result = sandbox
        .run(|lua| {
            let data = to_lua(lua, data)?;
            let history = lua.create_sequence_from(history.iter().copied())?;
            panel.call::<LuaValue>((data, history))
        })
        .map_err(from_settings)?;
    match result {
        LuaValue::Table(table) => read_output(&table).map_err(RunError::Output),
        other => Err(RunError::Output(format!("{} instead of a table", other.type_name()))),
    }
}

/// Refuses data too deep or too large before any of it reaches Lua.
fn check_shape(data: &Json) -> Result<(), RunError> {
    let mut nodes = 0usize;
    let mut stack = vec![(data, 1usize)];
    while let Some((value, depth)) = stack.pop() {
        nodes += 1;
        if nodes > MAX_NODES {
            return Err(RunError::Data(format!("has more than {MAX_NODES} values")));
        }
        if depth > MAX_DEPTH {
            return Err(RunError::Data(format!("nests deeper than {MAX_DEPTH} levels")));
        }
        match value {
            Json::Array(items) => stack.extend(items.iter().map(|item| (item, depth + 1))),
            Json::Object(map) => stack.extend(map.values().map(|item| (item, depth + 1))),
            _ => {}
        }
    }
    Ok(())
}

fn to_lua(lua: &Lua, value: &Json) -> mlua::Result<LuaValue> {
    Ok(match value {
        Json::Null => LuaValue::Nil,
        Json::Bool(flag) => LuaValue::Boolean(*flag),
        Json::Number(number) => match number.as_i64() {
            Some(integer) => LuaValue::Integer(integer),
            None => LuaValue::Number(number.as_f64().unwrap_or(f64::NAN)),
        },
        Json::String(text) => LuaValue::String(lua.create_string(text)?),
        Json::Array(items) => {
            let table: Table = lua.create_table_with_capacity(items.len(), 0)?;
            for (index, item) in items.iter().enumerate() {
                table.raw_set(index + 1, to_lua(lua, item)?)?;
            }
            LuaValue::Table(table)
        }
        Json::Object(map) => {
            let table: Table = lua.create_table_with_capacity(0, map.len())?;
            for (key, item) in map {
                table.raw_set(key.as_str(), to_lua(lua, item)?)?;
            }
            LuaValue::Table(table)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::Status;
    use serde_json::json;

    const SYNC: &str = r#"
return {
  panel = function(data, history)
    local pct = math.floor(data.height / data.target_height * 100)
    return {
      big = pct .. "%", unit = "synced", bar = pct / 100,
      rows = { { "height", data.height .. " / " .. data.target_height }, { "peers", data.outgoing_connections_count } },
      sample = pct, series = history,
      status = data.synchronized and "ok" or "busy",
    }
  end,
}"#;

    fn sync_data() -> Json {
        json!({ "height": 2121400, "target_height": 3478000, "outgoing_connections_count": 32, "synchronized": false })
    }

    #[test]
    fn a_panel_turns_data_into_values() {
        let out = run_panel(SYNC, "sync", &sync_data(), &[58.0, 60.0]).unwrap();
        assert_eq!(out.big.as_deref(), Some("60%"));
        assert_eq!(out.unit.as_deref(), Some("synced"));
        assert_eq!(out.bar, Some(0.6));
        assert_eq!(out.rows, vec![("height".into(), "2121400 / 3478000".into()), ("peers".into(), "32".into())]);
        assert_eq!(out.sample, Some(60.0));
        assert_eq!(out.series, vec![58.0, 60.0]);
        assert_eq!(out.status, Some(Status::Busy));
    }

    #[test]
    fn mistakes_fail_the_run_instead_of_drawing_wrongly() {
        let returns = |body: &str| run_panel(&format!("return {{ panel = function(d, h) return {body} end }}"), "p", &json!({}), &[]);
        assert!(matches!(returns("{ colour = 'red' }"), Err(RunError::Output(_))), "unknown field");
        assert!(matches!(returns("{ bar = 1.5 }"), Err(RunError::Output(_))), "bar out of range");
        assert!(matches!(returns("{ big = 'x\\27[2J' }"), Err(RunError::Output(_))), "escape sequence");
        assert!(matches!(returns("{ big = '12345678901234567' }"), Err(RunError::Output(_))), "too long");
        assert!(matches!(returns("{ status = 'great' }"), Err(RunError::Output(_))));
        assert!(matches!(returns("{ series = { 1, 0/0 } }"), Err(RunError::Output(_))), "NaN");
        assert!(matches!(returns("{ rows = { { 'a' } } }"), Err(RunError::Output(_))));
        assert!(matches!(returns("'text'"), Err(RunError::Output(_))));
        assert!(returns("{}").unwrap() == PanelOutput::default(), "every field is optional");
    }

    #[test]
    fn the_sandbox_is_the_settings_one() {
        let run = |body: &str| run_panel(&format!("return {{ panel = function(d, h) {body} end }}"), "p", &json!({}), &[]);
        for reach in ["return io.open('/etc/passwd')", "return os.execute('id')", "return require('x')", "return load('return 1')()"] {
            assert!(matches!(run(reach), Err(RunError::Script(_))), "{reach}");
        }
        assert_eq!(run("while true do end"), Err(RunError::Limit("instruction")));
        let grow = run("local t = {} for i = 1, 1e9 do t[i] = ('x'):upper() .. i end");
        assert!(matches!(grow, Err(RunError::Limit(_))), "{grow:?}");
        // Bytecode is refused.
        assert!(run_panel("\x1bLua", "p", &json!({}), &[]).is_err());
        // A file without a panel function.
        assert!(matches!(run_panel("return {}", "p", &json!({}), &[]), Err(RunError::Script(_))));
    }

    #[test]
    fn deep_or_huge_data_never_reaches_lua() {
        let mut deep = json!(1);
        for _ in 0..40 {
            deep = json!([deep]);
        }
        assert!(matches!(run_panel(SYNC, "p", &deep, &[]), Err(RunError::Data(_))));
        let huge = Json::Array(vec![json!(1); MAX_NODES + 1]);
        assert!(matches!(run_panel(SYNC, "p", &huge, &[]), Err(RunError::Data(_))));
    }

    #[test]
    fn runs_do_not_share_state() {
        let counter = "count = (count or 0) + 1  return { big = tostring(count) }";
        let src = format!("return {{ panel = function(d, h) {counter} end }}");
        assert_eq!(run_panel(&src, "p", &json!({}), &[]).unwrap().big.as_deref(), Some("1"));
        assert_eq!(run_panel(&src, "p", &json!({}), &[]).unwrap().big.as_deref(), Some("1"));
    }
}
