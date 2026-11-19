//! The awkward fixture app, tests/fixtures/nodo-fixture-forms, loaded with the parsers the device
//! uses and installed in the mock, and a mock crowded with enough apps, sources, updates and
//! providers that every list scrolls on a small terminal.

use nodo_api::{Exposure, Origin, ServiceState, ServiceStatus, SettingCurrent, SettingField, SettingKind};
use nodo_manifest::Manifest;
use nodo_settings::{DefaultValue, Limits, Schema, SettingDef, SettingType};
use nodo_tui::app::App;
use nodo_ui_core::mock::MockBackend;
use nodo_ui_core::model::*;
use nodo_ui_core::exec::load_snapshot;

pub const ID: &str = "nodo-fixture-forms";

fn read(file: &str) -> String {
    let path = format!("{}/../../tests/fixtures/{ID}/{file}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

pub fn manifest() -> Manifest {
    Manifest::parse(&read("manifest.toml")).expect("the fixture's manifest passes the device's checks")
}

/// The fixture's settings file, run in the device's sandbox, validators and all.
pub fn loaded() -> nodo_settings::LoadedSettings {
    nodo_settings::load_settings(&read("settings.lua"), Limits::default()).expect("the fixture's settings load")
}

pub fn schema() -> Schema {
    loaded().schema().clone()
}

/// What the helper reports for a setting with nothing stored, as `NodoSettings::form` builds it.
fn helper_field(def: &SettingDef) -> SettingField {
    let secret = def.ty == SettingType::Secret;
    let kind = match &def.ty {
        SettingType::String => SettingKind::Text,
        SettingType::Int { min, max } => SettingKind::Int { min: *min, max: *max },
        SettingType::Bool => SettingKind::Bool,
        SettingType::Enum { choices } => SettingKind::Choice(choices.clone()),
        SettingType::MoneroAddress => SettingKind::MoneroAddress,
        SettingType::Url => SettingKind::Url,
        SettingType::Secret => SettingKind::Secret,
    };
    let default = def.default.as_ref().filter(|_| !secret).map(|default| match default {
        DefaultValue::Text(text) => text.clone(),
        DefaultValue::Int(n) => n.to_string(),
        DefaultValue::Bool(b) => b.to_string(),
    });
    SettingField {
        name: def.name.clone(),
        label: def.label.clone(),
        help: def.help.clone().unwrap_or_default(),
        required: def.required,
        kind,
        // A generated secret is made when the app is installed, so it is set by the time the form opens.
        current: match (secret, def.generate.is_some()) {
            (true, true) => SettingCurrent::SecretSet,
            (true, false) => SettingCurrent::SecretUnset,
            (false, _) => SettingCurrent::Default,
        },
        default,
        public_name: def.public.clone(),
    }
}

/// The fixture's settings form as the TUI gets it from the helper on a fresh install.
pub fn form() -> SettingsForm {
    let schema = schema();
    let public_settings = schema.settings.iter().filter_map(|def| def.public.as_ref().map(|p| format!("{ID}.{p}"))).collect();
    let fields = schema.settings.iter().map(|def| nodo_ui_core::helper_backend::field(helper_field(def))).collect();
    SettingsForm { package: ID.to_owned(), fields, public_settings }
}

/// Installs the fixture in the mock: running, with its form and one site per web service.
pub fn install(mock: &mut MockBackend) {
    let manifest = manifest();
    mock.services.push(ServiceStatus {
        id: ID.to_owned(),
        state: ServiceState::Running,
        detail: String::new(),
        roles: Vec::new(),
        origin: Origin::Feed(Some("community".to_owned())),
        uptime_secs: Some(600),
    });
    mock.catalog.push(PackageSummary {
        id: PackageId::new("community", ID),
        status: PackageStatus::Installed,
        version: manifest.version.clone(),
        size: "1 MB".to_owned(),
        tagline: "Every kind of setting, awkwardly".to_owned(),
        description: vec!["A test app whose settings form is taller than any terminal.".to_owned()],
        needs: Vec::new(),
        bundle: None,
    });
    mock.forms.insert(ID.to_owned(), form());
    for web in &manifest.web {
        let name = web.suggested_subdomain.clone().unwrap_or_else(|| web.name.clone());
        mock.web.sites.push(Site {
            package: name.clone(),
            exposure: Exposure { local: true, onion: false, public: false },
            local_url: format!("https://{name}.local"),
            onion_url: format!("http://{name}…onion"),
            public_host: None,
            public_aliases: Vec::new(),
            locked: false,
            rate_limit_unenforced: false,
            local_waiting: false,
        });
    }
}

/// A mock with the fixture installed and long lists everywhere: apps, sources, updates, sites and
/// provider accounts, so each list is taller than a small terminal.
pub fn crowded() -> MockBackend {
    let mut mock = MockBackend::new();
    install(&mut mock);
    for n in 0..24 {
        let id = format!("crowd-app-{n:02}");
        mock.services.push(ServiceStatus {
            id: id.clone(),
            state: if n % 7 == 3 { ServiceState::Stopped } else { ServiceState::Running },
            detail: String::new(),
            roles: Vec::new(),
            origin: Origin::Feed(Some("community".to_owned())),
            uptime_secs: Some(3600 * n),
        });
        mock.catalog.push(PackageSummary {
            id: PackageId::new("community", &id),
            status: if n % 2 == 0 { PackageStatus::Installed } else { PackageStatus::Available },
            version: format!("1.{n}.0"),
            size: "3 MB".to_owned(),
            tagline: format!("Crowd app number {n}"),
            description: vec![format!("Crowd app number {n}, here to make lists long.")],
            needs: Vec::new(),
            bundle: None,
        });
        mock.updates.push(UpdateItem {
            name: id.clone(),
            from: format!("1.{n}.0"),
            to: format!("1.{n}.1"),
            source: "community".to_owned(),
            third_party: true,
            note: UpdateNote::Ready,
            held: n % 5 == 4,
            preselected: true,
            grows: None,
        });
        mock.web.sites.push(Site {
            package: id.clone(),
            exposure: Exposure { local: true, onion: n % 3 == 0, public: false },
            local_url: format!("https://{id}.local"),
            onion_url: format!("http://{id}…onion"),
            public_host: None,
            public_aliases: Vec::new(),
            locked: false,
            rate_limit_unenforced: false,
            local_waiting: false,
        });
    }
    let template = mock.feeds[0].clone();
    for n in 0..12 {
        let mut feed = template.clone();
        feed.name = format!("crowd-source-{n:02}");
        feed.address = format!("https://example.com/crowd-source-{n:02}");
        mock.feeds.push(feed);
    }
    for n in 0..8 {
        mock.connected.push(ConnectedProvider {
            id: format!("crowd-dns-{n}"),
            name: format!("Crowd DNS {n}"),
            domains: vec![ConnectedDomain { name: format!("crowd{n}.example"), used_by: Vec::new(), held_by: None }],
            lists_domains: true,
            problem: None,
        });
    }
    mock
}

/// A loaded app on the crowded mock, at `size`.
pub fn crowded_app(size: (u16, u16)) -> (App, MockBackend) {
    let mut mock = crowded();
    let mut app = App::new();
    app.data = load_snapshot(&mut mock).expect("the mock always answers");
    mock.calls.clear();
    app.size = size;
    (app, mock)
}
