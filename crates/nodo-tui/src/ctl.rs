//! `nodo-tui --ctl`: one helper request from the command line, for scripts and tests.
//!
//! Secrets are read from standard input, one per line, so they never appear in an argument list
//! that other users can read from `/proc`.

use std::io::{self, BufRead, IsTerminal};

use zeroize::Zeroizing;

use nodo_api::{
    Exposure, PublicConfirmation, RepairConfirmation, Request, Secret, SettingChange, SettingGrant, SettingInput,
    SettingLiteral,
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
  drive-check | drive-repair CHECK_ID | drive-mount
  node | roles | bind ROLE PACKAGE | unbind ROLE | grants
  grant CONSUMER PROVIDER SETTING | revoke CONSUMER PROVIDER SETTING
  data-drive PACKAGE 0|1
  feeds | refresh-feeds | catalog | probe-feed ADDRESS | add-feed ADDRESS FINGERPRINT
  feed-enable NAME 0|1 | remove-feed NAME | feed-key NAME | trust-key NAME FROM TO TYPED_NAME
  review FEED PACKAGE
  install-pkg FEED PACKAGE PKGVER [PERMISSION...]   (sends exactly this approval)
  remove-pkg PACKAGE                                 (its data stays on the drive)
  approve-install FEED PACKAGE                      (reviews, then approves what it showed)
  approve-install-encrypted FEED PACKAGE            (the same, device passphrase on stdin)
  updates | update-status | check-updates | review-update PACKAGE
  apply-update PACKAGE TO | apply-update-approved PACKAGE TO
  hold PACKAGE VERSION 0|1 | dismiss PACKAGE | auto-run
  auto-get | auto-set FREQUENCY START END [PACKAGE...]
  health | backups | delete-backup NAME
  backup                           (device passphrase or an empty line, then backup passphrase)
  inspect-backup NAME              (backup passphrase on stdin)
  restore NAME TYPED_WORD          (backup passphrase, then new device passphrase or empty)
  usb-drives | usb-backups DEVICE
  usb-backup DEVICE                (device passphrase or an empty line, then backup passphrase)
  usb-inspect DEVICE NAME          (backup passphrase on stdin)
  usb-restore DEVICE NAME TYPED_WORD   (backup passphrase, then new device passphrase or empty)
  apply-update-pass PACKAGE TO     (device passphrase on stdin, for an update that grows storage)
  update-storage                   (updates that need the device passphrase to grow storage)
  system-status | system-check | system-apply SERIAL | system-auto 0|1
  system-run                       (root only, sent by the nodo-systemupdate schedule)
  web-status | dns-providers | test-dns | retire-stale | reach PACKAGE SITE
  set-dns DOMAIN PROVIDER FIELD...  (one value per field on stdin)
  domain-providers | refresh-provider ID | remove-provider ID | provider-domain ID DOMAIN
  add-provider KIND FIELD...  (one value per field on stdin; lists the key's domains)
  add-provider-for DOMAIN KIND FIELD...  (for a provider that cannot list domains)
  acme | certs | local-names | local-status
  local-address-changed            (root only, sent by nodo-localnet)
  acme-set EMAIL|- 0|1 [LOCAL_DOMAIN]   (1 is Let's Encrypt staging)
  dns01 [FIELD...]                 (one value per field on stdin, none removes them)
  onion-status | node-onion 0|1     (the node onion serves only its restricted RPC)
  rollbacks | rollback PACKAGE TO  (back to the version kept from before its last update)
  wifi | wifi-on 0|1 | wifi-scan | wifi-forget SSID
  wifi-join SSID wpa2|wpa3|wpa2-wpa3   (passphrase on stdin)
  wifi-join-open SSID              (joins an open network: its traffic is not encrypted)
  bt | bt-on 0|1 | bt-scan | bt-pair ADDRESS | bt-connect ADDRESS 0|1 | bt-remove ADDRESS
  screen-pin                       (device passphrase, then the new 6-digit PIN, on stdin)
  screen-pin-clear                 (device passphrase on stdin; the screen can then only look)
  screen-status";

/// A secret line where an empty line means none was given.
fn optional_secret() -> Result<Option<Secret>, String> {
    let secret = secret_line()?;
    Ok((!secret.expose().is_empty()).then_some(secret))
}

/// The second request of a command that first reviews and then approves what the review
/// showed, as a user does. The helper checks the approval against the package either way.
pub fn follow_up(args: &[String], first: &nodo_api::Response) -> Result<Option<Request>, String> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let nodo_api::Response::InstallReview(review) = first else {
        return Ok(None);
    };
    let approval = nodo_api::PermissionApproval {
        pkgver: review.pkgver.clone(),
        permissions: review.permissions.clone(),
    };
    Ok(match a.as_slice() {
        ["approve-install", feed, package] => Some(Request::InstallPackage {
            feed: (*feed).to_owned(),
            package: (*package).to_owned(),
            approval,
            passphrase: None,
        }),
        ["approve-install-encrypted", feed, package] => Some(Request::InstallPackage {
            feed: (*feed).to_owned(),
            package: (*package).to_owned(),
            approval,
            passphrase: Some(secret_line()?),
        }),
        ["apply-update-approved", package, to] => Some(Request::ApplyUpdates {
            updates: vec![nodo_api::UpdateApproval {
                package: (*package).to_owned(),
                to: (*to).to_owned(),
                approval: Some(approval),
            }],
        }),
        _ => None,
    })
}

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
        ["screen-pin"] => Request::SetScreenPin { passphrase: secret_line()?, pin: Some(secret_line()?) },
        ["screen-pin-clear"] => Request::SetScreenPin { passphrase: secret_line()?, pin: None },
        ["screen-status"] => Request::ScreenStatus,
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
        ["node"] => Request::NodeStatus,
        ["roles"] => Request::ListRoleBindings,
        ["bind", role, package] => Request::SetRoleBinding { role: id(role), package: Some(id(package)) },
        ["unbind", role] => Request::SetRoleBinding { role: id(role), package: None },
        ["grants"] => Request::ListGrants,
        [verb @ ("grant" | "revoke"), consumer, provider, setting] => Request::SetGrant {
            grant: SettingGrant { consumer: id(consumer), provider: id(provider), setting: id(setting) },
            approved: *verb == "grant",
        },
        ["data-drive", package, on] => Request::UseDataDrive { package: id(package), enabled: flag(on)? },
        ["feeds"] => Request::ListFeeds,
        ["refresh-feeds"] => Request::RefreshFeeds,
        ["catalog"] => Request::Catalog,
        ["probe-feed", address] => Request::ProbeFeed { address: id(address) },
        ["add-feed", address, fingerprint] => Request::AddFeed { address: id(address), fingerprint: id(fingerprint) },
        ["feed-enable", name, on] => Request::SetFeedEnabled { name: id(name), enabled: flag(on)? },
        ["remove-feed", name] => Request::RemoveFeed { name: id(name) },
        ["feed-key", name] => Request::FeedKeyChange { name: id(name) },
        ["trust-key", name, from, to, typed] => Request::TrustFeedKey {
            name: id(name),
            confirmation: nodo_api::KeyChangeConfirmation { from: id(from), to: id(to), typed_feed_name: id(typed) },
        },
        ["review", feed, package]
        | ["approve-install", feed, package]
        | ["approve-install-encrypted", feed, package] => Request::ReviewInstall { feed: id(feed), package: id(package) },
        ["install-pkg", feed, package, pkgver, permissions @ ..] => Request::InstallPackage {
            feed: id(feed),
            package: id(package),
            approval: nodo_api::PermissionApproval {
                pkgver: id(pkgver),
                permissions: permissions.iter().map(|p| id(p)).collect(),
            },
            passphrase: None,
        },
        ["remove-pkg", package] => Request::RemovePackage { package: id(package) },
        ["updates"] => Request::ListUpdates,
        ["update-status"] => Request::UpdateStatus,
        ["check-updates"] => Request::CheckUpdates,
        ["review-update", package] | ["apply-update-approved", package, _] => {
            Request::ReviewUpdate { package: id(package) }
        }
        ["apply-update", package, to] => Request::ApplyUpdates {
            updates: vec![nodo_api::UpdateApproval { package: id(package), to: id(to), approval: None }],
        },
        ["hold", package, version, on] => Request::SetHold { package: id(package), version: id(version), held: flag(on)? },
        ["dismiss", package] => Request::DismissUpdate { package: id(package) },
        ["auto-run"] => Request::RunAutoUpdate,
        ["auto-get"] => Request::GetAutoUpdate,
        ["auto-set", frequency, start, end, packages @ ..] => Request::SetAutoUpdate(nodo_api::AutoUpdatePrefs {
            frequency: id(frequency),
            window_start: id(start),
            window_end: id(end),
            enabled: packages.iter().map(|p| id(p)).collect(),
        }),
        ["health"] => Request::Health,
        ["backups"] => Request::ListBackups,
        ["delete-backup", name] => Request::DeleteBackup { name: id(name) },
        ["backup"] => Request::CreateBackup { device_passphrase: optional_secret()?, backup_passphrase: secret_line()? },
        ["inspect-backup", name] => Request::InspectBackup { name: id(name), passphrase: secret_line()? },
        ["restore", name, typed] => Request::RestoreBackup {
            name: id(name),
            passphrase: secret_line()?,
            new_device_passphrase: optional_secret()?,
            typed: id(typed),
        },
        ["usb-drives"] => Request::ListUsbDrives,
        ["usb-backups", device] => Request::ListUsbBackups { device: id(device) },
        ["usb-backup", device] => Request::CreateUsbBackup {
            device: id(device),
            device_passphrase: optional_secret()?,
            backup_passphrase: secret_line()?,
        },
        ["usb-inspect", device, name] => {
            Request::InspectUsbBackup { device: id(device), name: id(name), passphrase: secret_line()? }
        }
        ["usb-restore", device, name, typed] => Request::RestoreUsbBackup {
            device: id(device),
            name: id(name),
            passphrase: secret_line()?,
            new_device_passphrase: optional_secret()?,
            typed: id(typed),
        },
        ["apply-update-pass", package, to] => Request::ApplyUpdatesWithPassphrase {
            updates: vec![nodo_api::UpdateApproval { package: id(package), to: id(to), approval: None }],
            passphrase: secret_line()?,
        },
        ["update-storage"] => Request::UpdateStorageGrowth,
        ["system-status"] => Request::SystemUpdateStatus,
        ["system-check"] => Request::CheckSystemUpdate,
        ["system-apply", serial] => {
            Request::ApplySystemUpdate { serial: serial.parse().map_err(|_| "SERIAL must be a number")? }
        }
        ["system-auto", on] => Request::SetSystemAutoUpdate { enabled: flag(on)? },
        ["system-run"] => Request::RunSystemUpdate,
        ["web-status"] => Request::WebStatus,
        ["dns-providers"] => Request::ListDnsProviders,
        ["test-dns"] => Request::TestDnsProvider,
        ["retire-stale"] => Request::RetireStaleRecords,
        ["reach", package, s] => Request::CheckReachabilityDetail { site: site(package, s) },
        ["set-dns", domain, provider, fields @ ..] if !fields.is_empty() => Request::SetDnsProvider {
            domain: id(domain),
            provider: id(provider),
            fields: fields.iter().map(|f| Ok((id(f), secret_line()?))).collect::<Result<_, String>>()?,
        },
        ["domain-providers"] => Request::ListDomainProviders,
        ["refresh-provider", provider] => Request::RefreshDomainProvider { id: id(provider) },
        ["remove-provider", provider] => Request::RemoveDomainProvider { id: id(provider) },
        ["provider-domain", provider, domain] => Request::AddProviderDomain { id: id(provider), domain: id(domain) },
        ["add-provider", kind, fields @ ..] if !fields.is_empty() => Request::AddDomainProvider {
            kind: id(kind),
            fields: fields.iter().map(|f| Ok((id(f), secret_line()?))).collect::<Result<_, String>>()?,
            domain: None,
        },
        ["add-provider-for", domain, kind, fields @ ..] if !fields.is_empty() => Request::AddDomainProvider {
            kind: id(kind),
            fields: fields.iter().map(|f| Ok((id(f), secret_line()?))).collect::<Result<_, String>>()?,
            domain: Some(id(domain)),
        },
        ["acme"] => Request::AcmeSettings,
        ["certs"] => Request::CertificateStatus,
        ["local-names"] => Request::CheckLocalNames,
        ["local-status"] => Request::LocalSiteStatus,
        ["onion-status"] => Request::OnionStatus,
        ["node-onion", on] => Request::SetNodeOnion { enabled: flag(on)? },
        ["rollbacks"] => Request::ListRollbacks,
        ["rollback", package, to] => Request::RollbackPackage { package: id(package), to: id(to) },
        ["wifi"] => Request::WifiStatus,
        ["wifi-on", on] => Request::SetWifiEnabled { enabled: flag(on)? },
        ["wifi-scan"] => Request::ScanWifi,
        ["wifi-forget", ssid] => Request::ForgetWifi { ssid: id(ssid) },
        ["wifi-join", ssid, security] => Request::JoinWifi(nodo_api::WifiJoin {
            ssid: id(ssid),
            security: match *security {
                "wpa2" => nodo_api::WifiSecurity::Wpa2Personal,
                "wpa3" => nodo_api::WifiSecurity::Wpa3Personal,
                "wpa2-wpa3" => nodo_api::WifiSecurity::Wpa2Wpa3Personal,
                other => return Err(format!("expected wpa2, wpa3 or wpa2-wpa3, got {other}")),
            },
            passphrase: Some(secret_line()?),
            accept_open: false,
        }),
        ["wifi-join-open", ssid] => Request::JoinWifi(nodo_api::WifiJoin {
            ssid: id(ssid),
            security: nodo_api::WifiSecurity::Open,
            passphrase: None,
            accept_open: true,
        }),
        ["bt"] => Request::BluetoothStatus,
        ["bt-on", on] => Request::SetBluetoothEnabled { enabled: flag(on)? },
        ["bt-scan"] => Request::ScanBluetooth,
        ["bt-pair", address] => Request::PairBluetooth { address: id(address) },
        ["bt-connect", address, on] => Request::ConnectBluetooth { address: id(address), connect: flag(on)? },
        ["bt-remove", address] => Request::RemoveBluetooth { address: id(address) },
        ["local-address-changed"] => Request::LocalAddressChanged,
        ["acme-set", email, staging @ ("0" | "1"), rest @ ..] if rest.len() <= 1 => {
            Request::SetAcmeSettings(nodo_api::AcmeSettingsInput {
                email: (*email != "-").then(|| id(email)),
                staging: *staging == "1",
                local_domain: rest.first().map(|d| id(d)),
            })
        }
        ["dns01", fields @ ..] => Request::SetDns01Credentials {
            fields: fields.iter().map(|f| Ok((id(f), secret_line()?))).collect::<Result<_, String>>()?,
        },
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

    #[test]
    fn parses_bindings_and_grants() {
        let Ok(Request::SetGrant { grant, approved: false }) = parse(&args("revoke shop cuprated token")) else {
            panic!()
        };
        assert_eq!((grant.consumer.as_str(), grant.provider.as_str()), ("shop", "cuprated"));
        assert!(matches!(
            parse(&args("bind monero-daemon cuprated")),
            Ok(Request::SetRoleBinding { package: Some(_), .. })
        ));
        assert!(matches!(parse(&args("unbind monero-daemon")), Ok(Request::SetRoleBinding { package: None, .. })));
        assert!(matches!(parse(&args("data-drive vault 1")), Ok(Request::UseDataDrive { enabled: true, .. })));
        assert!(matches!(parse(&args("node")), Ok(Request::NodeStatus)));
    }

    #[test]
    fn parses_system_updates() {
        assert!(matches!(parse(&args("system-apply 7")), Ok(Request::ApplySystemUpdate { serial: 7 })));
        assert!(parse(&args("system-apply seven")).is_err());
        assert!(matches!(parse(&args("system-auto 1")), Ok(Request::SetSystemAutoUpdate { enabled: true })));
        assert!(matches!(parse(&args("system-auto 0")), Ok(Request::SetSystemAutoUpdate { enabled: false })));
        assert!(matches!(parse(&args("system-run")), Ok(Request::RunSystemUpdate)));
        assert!(matches!(parse(&args("system-check")), Ok(Request::CheckSystemUpdate)));
        assert!(matches!(parse(&args("update-storage")), Ok(Request::UpdateStorageGrowth)));
    }

    #[test]
    fn parses_radio_commands() {
        assert!(matches!(parse(&args("wifi")), Ok(Request::WifiStatus)));
        assert!(matches!(parse(&args("wifi-on 0")), Ok(Request::SetWifiEnabled { enabled: false })));
        assert!(matches!(parse(&args("wifi-scan")), Ok(Request::ScanWifi)));
        let Ok(Request::JoinWifi(join)) = parse(&args("wifi-join-open cafe")) else { panic!() };
        assert!(join.accept_open && join.passphrase.is_none());
        assert!(parse(&args("wifi-join home wep")).is_err());
        assert!(matches!(parse(&args("wifi-forget home")), Ok(Request::ForgetWifi { .. })));
        assert!(matches!(parse(&args("bt-on 1")), Ok(Request::SetBluetoothEnabled { enabled: true })));
        assert!(matches!(parse(&args("bt-connect AA:BB:CC:DD:EE:FF 0")), Ok(Request::ConnectBluetooth { connect: false, .. })));
        assert!(matches!(parse(&args("bt-remove AA:BB:CC:DD:EE:FF")), Ok(Request::RemoveBluetooth { .. })));
    }

    #[test]
    fn parses_onion_and_rollback() {
        assert!(matches!(parse(&args("onion-status")), Ok(Request::OnionStatus)));
        assert!(matches!(parse(&args("node-onion 1")), Ok(Request::SetNodeOnion { enabled: true })));
        assert!(parse(&args("node-onion yes")).is_err());
        assert!(matches!(parse(&args("rollbacks")), Ok(Request::ListRollbacks)));
        let Ok(Request::RollbackPackage { package, to }) = parse(&args("rollback demo 1.0_1")) else { panic!() };
        assert_eq!((package.as_str(), to.as_str()), ("demo", "1.0_1"));
    }
}
