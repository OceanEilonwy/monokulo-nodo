//! The settings worker: runs package Lua in a separate, unprivileged process.
//!
//! The helper never loads Lua itself. It sends package sources and the values to check, and
//! reads back plain data. The child runs as a dedicated account with tight resource limits, on
//! top of the Lua sandbox's own limits.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};

use nodo_settings::{load_settings, Limits, LoadedSettings, Schema};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// Largest request or response, which bounds how much either side reads.
pub const MAX_WORKER_IO: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkerRequest {
    pub packages: Vec<WorkerPackage>,
    pub checks: Vec<WorkerCheck>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerPackage {
    pub id: String,
    pub source: String,
}

/// A value to run through its setting's validator, as the literal text of its type.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerCheck {
    pub package: String,
    pub setting: String,
    pub text: String,
}

impl std::fmt::Debug for WorkerCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The text can be a secret.
        write!(f, "WorkerCheck({}.{})", self.package, self.setting)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WorkerResponse {
    pub schemas: BTreeMap<String, Result<Schema, String>>,
    /// One message per failed check. Messages never quote a secret.
    pub rejections: Vec<String>,
}

/// The worker's logic, separate from its process so tests can call it directly.
pub fn run(request: &WorkerRequest) -> WorkerResponse {
    let mut loaded: BTreeMap<&str, LoadedSettings> = BTreeMap::new();
    let mut response = WorkerResponse::default();
    for package in &request.packages {
        let result = load_settings(&package.source, Limits::default());
        let schema = match result {
            Ok(settings) => {
                let schema = settings.schema().clone();
                loaded.insert(&package.id, settings);
                Ok(schema)
            }
            Err(e) => Err(e.to_string()),
        };
        response.schemas.insert(package.id.clone(), schema);
    }
    for check in &request.checks {
        let Some(settings) = loaded.get(check.package.as_str()) else {
            response
                .rejections
                .push(format!("{}: settings did not load", check.package));
            continue;
        };
        let Some(def) = settings.schema().setting(&check.setting) else {
            response.rejections.push(format!(
                "{}: no setting is named {}",
                check.package, check.setting
            ));
            continue;
        };
        let outcome = def
            .ty
            .parse_literal(&check.text)
            .map_err(|message| format!("{}: {message}", check.setting))
            .and_then(|value| {
                settings
                    .validate_value(&check.setting, &value)
                    .map_err(|e| e.to_string())
            });
        if let Err(message) = outcome {
            response
                .rejections
                .push(format!("{}.{message}", check.package));
        }
    }
    response
}

/// Lowers the process's own limits before it reads any input. Run in the child only.
fn limit_self() -> io::Result<()> {
    use rustix::process::{setrlimit, Resource, Rlimit};
    let set = |resource, value: u64| {
        setrlimit(
            resource,
            Rlimit {
                current: Some(value),
                maximum: Some(value),
            },
        )
        .map_err(io::Error::from)
    };
    set(Resource::As, 512 * 1024 * 1024)?;
    set(Resource::Cpu, 30)?;
    set(Resource::Nofile, 16)?;
    set(Resource::Fsize, 0)?;
    // No child processes at all.
    set(Resource::Nproc, 0)?;
    set(Resource::Core, 0)?;
    Ok(())
}

/// The child's entry point: request on stdin, response on stdout.
pub fn serve_stdio() -> Result<(), String> {
    if rustix::process::geteuid().is_root() {
        return Err("the settings worker must not run as root".to_owned());
    }
    limit_self().map_err(|e| format!("could not set limits: {e}"))?;
    let mut input = Zeroizing::new(Vec::new());
    io::stdin()
        .take(MAX_WORKER_IO as u64 + 1)
        .read_to_end(&mut input)
        .map_err(|e| e.to_string())?;
    if input.len() > MAX_WORKER_IO {
        return Err("request too large".to_owned());
    }
    let request: WorkerRequest =
        serde_json::from_slice(&input).map_err(|_| "malformed request".to_owned())?;
    let response = run(&request);
    let output = serde_json::to_vec(&response).map_err(|e| e.to_string())?;
    let mut stdout = io::stdout().lock();
    stdout.write_all(&output).map_err(|e| e.to_string())?;
    stdout.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"return { api_version = 1, settings = {
        { name = "port", type = "int", label = "Port", default = 8080, min = 1024, max = 65535 },
        { name = "key", type = "secret", label = "Key",
          validate = function(v) return #v >= 4, "too short: " .. v end },
    } }"#;

    fn check(setting: &str, text: &str) -> WorkerCheck {
        WorkerCheck {
            package: "shop".into(),
            setting: setting.into(),
            text: text.into(),
        }
    }

    #[test]
    fn loads_schemas_and_runs_validators_without_echoing_secrets() {
        let request = WorkerRequest {
            packages: vec![
                WorkerPackage {
                    id: "shop".into(),
                    source: SOURCE.into(),
                },
                WorkerPackage {
                    id: "broken".into(),
                    source: "return 1".into(),
                },
            ],
            checks: vec![
                check("port", "80"),
                check("port", "9000"),
                check("key", "abc"),
                check("key", "abcd"),
                check("ghost", "x"),
            ],
        };
        let response = run(&request);
        assert!(response.schemas["shop"].is_ok());
        assert!(response.schemas["broken"].is_err());
        assert_eq!(response.rejections.len(), 3, "{:?}", response.rejections);
        assert!(response.rejections.iter().all(|r| !r.contains("abc")));
    }

    #[test]
    fn debug_hides_check_text() {
        assert!(!format!("{:?}", check("key", "hunter2")).contains("hunter2"));
    }
}
