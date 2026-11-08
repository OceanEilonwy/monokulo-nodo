//! `nodo-tui --ctl`: one helper request from the command line, for scripts and tests.
//!
//! Secrets are read from standard input, one per line, so they never appear in an argument list
//! that other users can read from `/proc`.

use std::io::{self, BufRead, IsTerminal};

use zeroize::Zeroizing;

use nodo_api::{
    Exposure, PublicConfirmation, RepairConfirmation, Request, Secret, SettingChange, SettingInput, SettingLiteral,
    SiteId,
};

pub const USAGE: &str = "\
nodo-tui --ctl COMMAND [ARGS]
  services | overview | packages | sites | sources | keys | drive
  detail ID | form PACKAGE | log SOURCE LINES
  start ID | stop ID | restart ID | install ID
  install-encrypted ID             (passphrase on stdin)
  init | unlock                    (passphrase on stdin)
  change-passphrase                (current, then new, on stdin)
  set PACKAGE NAME text|int|bool VALUE
  set PACKAGE NAME ref TARGET.SETTING
  set PACKAGE NAME secret          (value on stdin)
  reset PACKAGE NAME
  host PACKAGE SITE HOST
  expose PACKAGE SITE LOCAL ONION PUBLIC [TYPED_HOST]   (each flag 0 or 1)
  add-key LINE | remove-key FINGERPRINT
  drive-check | drive-repair CHECK_ID | drive-mount";

/// One line from stdin. On a terminal the line is typed without echo, so it never shows.
fn secret_line() -> Result<Secret, String> {
    if io::stdin().is_terminal() {
        return typed_secret();
    }
    let mut line = Zeroizing::new(String::new());
    io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(Secret::new(line.trim_end_matches(['\n', '\r'])))
}

fn typed_secret() -> Result<Secret, String> {
    use crossterm::event::{read, Event, KeyCode, KeyEventKind, KeyModifiers};
    eprint!("Secret: ");
    crossterm::terminal::enable_raw_mode().map_err(|e| e.to_string())?;
    let mut text = Zeroizing::new(String::new());
    let result = loop {
        match read() {
            Ok(Event::Key(key)) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err("cancelled".to_owned())
                }
                KeyCode::Char(c) => text.push(c),
                KeyCode::Backspace => {
                    text.pop();
                }
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(e.to_string()),
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    eprintln!();
    result.map(|()| Secret::new(text.as_str()))
}

fn flag(text: &str) -> Result<bool, String> {
    match text {
        "1" => Ok(true),
        "0" => Ok(false),
        _ => Err(format!("expected 0 or 1, got {text}")),
    }
}

/// Builds the request an argument list asks for.
pub fn parse(args: &[String]) -> Result<Request, String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let id = |s: &str| s.to_owned();
    let site = |package: &str, site: &str| SiteId { package: package.to_owned(), site: site.to_owned() };
    Ok(match a.as_slice() {
        ["services"] => Request::ListServices,
        ["overview"] => Request::DeviceOverview,
        ["packages"] => Request::ListPackages,
        ["sites"] => Request::ListSites,
        ["sources"] => Request::LogSources,
        ["keys"] => Request::ListSshKeys,
        ["drive"] => Request::DriveStatus,
        ["detail", x] => Request::PackageDetail { id: id(x) },
        ["form", x] => Request::SettingsForm { package: id(x) },
        ["log", x, n] => Request::ReadLog { source: id(x), max_lines: n.parse().map_err(|_| "LINES must be a number")? },
        ["start", x] => Request::StartService { id: id(x) },
        ["stop", x] => Request::StopService { id: id(x) },
        ["restart", x] => Request::RestartService { id: id(x) },
        ["install", x] => Request::InstallService { id: id(x), passphrase: None },
        ["install-encrypted", x] => Request::InstallService { id: id(x), passphrase: Some(secret_line()?) },
        ["init"] => Request::InitEncryption { passphrase: secret_line()? },
        ["unlock"] => Request::Unlock { passphrase: secret_line()? },
        ["change-passphrase"] => Request::ChangePassphrase { current: secret_line()?, new: secret_line()? },
        ["set", package, name, kind, rest @ ..] => {
            let value = match (*kind, rest) {
                ("text", [v]) => SettingInput::Literal(SettingLiteral::Text(id(v))),
                ("int", [v]) => SettingInput::Literal(SettingLiteral::Int(v.parse().map_err(|_| "not a number")?)),
                ("bool", [v]) => SettingInput::Literal(SettingLiteral::Bool(flag(v)?)),
                ("ref", [v]) => {
                    let (target, setting) = v.split_once('.').ok_or("ref takes TARGET.SETTING")?;
                    SettingInput::Reference { target: id(target), setting: id(setting) }
                }
                ("secret", []) => SettingInput::Secret(secret_line()?),
                _ => return Err(USAGE.to_owned()),
            };
            Request::ApplySettings { package: id(package), changes: vec![SettingChange { name: id(name), value }] }
        }
        ["reset", package, name] => Request::ApplySettings {
            package: id(package),
            changes: vec![SettingChange { name: id(name), value: SettingInput::Reset }],
        },
        ["host", package, s, host] => Request::SetPublicHost { site: site(package, s), host: id(host) },
        ["expose", package, s, local, onion, public, typed @ ..] => Request::SetExposure {
            site: site(package, s),
            exposure: Exposure { local: flag(local)?, onion: flag(onion)?, public: flag(public)? },
            confirmation: typed.first().map(|t| PublicConfirmation { typed_host: id(t) }),
        },
        ["add-key", line] => Request::AddSshKey { line: id(line) },
        ["remove-key", fp] => Request::RemoveSshKey { fingerprint: id(fp) },
        ["drive-check"] => Request::CheckDrive,
        ["drive-repair", check] => Request::RepairDrive(RepairConfirmation {
            check_id: check.parse().map_err(|_| "CHECK_ID must be a number")?,
            accept_data_loss_risk: true,
        }),
        ["drive-mount"] => Request::MountDrive,
        _ => return Err(USAGE.to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn parses_typed_settings_and_exposure() {
        let Ok(Request::ApplySettings { changes, .. }) = parse(&args("set cuprated outbound_peers int 64")) else {
            panic!()
        };
        assert_eq!(changes[0].value, SettingInput::Literal(SettingLiteral::Int(64)));
        let Ok(Request::ApplySettings { changes, .. }) = parse(&args("set shop port ref monero-daemon.rpc-port")) else {
            panic!()
        };
        assert!(matches!(changes[0].value, SettingInput::Reference { .. }));
        let Ok(Request::SetExposure { exposure, confirmation, .. }) =
            parse(&args("expose shop web 1 0 1 shop.example.com"))
        else {
            panic!()
        };
        assert!(exposure.public && exposure.local && !exposure.onion);
        assert_eq!(confirmation.unwrap().typed_host, "shop.example.com");
        assert!(parse(&args("expose shop web 1 0 2")).is_err());
        assert!(parse(&args("bogus")).is_err());
    }
}
