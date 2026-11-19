//! The screens by name, and the four tabs they sit under. Every interface has the same ones, so
//! fixtures and tests can enumerate them.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Home,
    /// Installed apps, each with its own page (update, hosting, settings, logs), and Get apps.
    Apps,
    Web,
    /// The Nodo itself: its updates, backups, network and device logs.
    System,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Home, Tab::Apps, Tab::Web, Tab::System];

    pub fn label(self) -> &'static str {
        match self {
            Tab::Home => "Home",
            Tab::Apps => "Apps",
            Tab::Web => "Web",
            Tab::System => "System",
        }
    }
}

/// One entry per design file, so fixtures and tests can enumerate the whole design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScreenId {
    Main,
    Search,
    Detail,
    Review,
    Installing,
    Unlock,
    Feeds,
    AddFeed,
    FeedKey,
    Settings,
    Reference,
    ApplyFailed,
    Updates,
    UpdateFailed,
    Logs,
    Rejected,
    Capability,
    NoDrive,
    WebSites,
    WebExposure,
    WebProviders,
    WebCheck,
    WebConfirm,
    WebCgnat,
    AutoUpdate,
    WebDomain,
    WebCertificate,
    WebTrust,
    /// First boot, and SSH keys afterwards. It has no design file, so it is not in `ALL`.
    Setup,
    /// Backups and restore, on the device and on a USB drive.
    Backup,
    /// Tor exposure of the node and the sites.
    Onion,
    /// First boot: rebuilding the device from a backup on a USB drive.
    Restore,
    /// Rollbacks that happened, and putting a package back to its previous version.
    Rollbacks,
    /// Wi-Fi and Bluetooth.
    Network,
    /// The installed apps.
    Apps,
    /// One installed app's page.
    AppPage,
    /// Confirming an app's removal.
    AppRemove,
    /// The Nodo itself: updates, backups, network, device logs, about.
    System,
    /// Installing a suite of apps together: review, questions, passphrase, progress, result.
    Bundle,
    /// An app's setup checklist for the owner.
    OwnerPage,
    /// The web GUI: what it is, where it answers, and its switch.
    WebGui,
}

impl ScreenId {
    pub const ALL: [ScreenId; 40] = [
        ScreenId::Main,
        ScreenId::Search,
        ScreenId::Detail,
        ScreenId::Review,
        ScreenId::Installing,
        ScreenId::Unlock,
        ScreenId::Feeds,
        ScreenId::AddFeed,
        ScreenId::FeedKey,
        ScreenId::Settings,
        ScreenId::Reference,
        ScreenId::ApplyFailed,
        ScreenId::Updates,
        ScreenId::UpdateFailed,
        ScreenId::Logs,
        ScreenId::Rejected,
        ScreenId::Capability,
        ScreenId::NoDrive,
        ScreenId::WebSites,
        ScreenId::WebExposure,
        ScreenId::WebProviders,
        ScreenId::WebCheck,
        ScreenId::WebConfirm,
        ScreenId::WebCgnat,
        ScreenId::AutoUpdate,
        ScreenId::WebDomain,
        ScreenId::WebCertificate,
        ScreenId::WebTrust,
        ScreenId::Backup,
        ScreenId::Onion,
        ScreenId::Restore,
        ScreenId::Rollbacks,
        ScreenId::Network,
        ScreenId::Apps,
        ScreenId::AppPage,
        ScreenId::AppRemove,
        ScreenId::System,
        ScreenId::Bundle,
        ScreenId::OwnerPage,
        ScreenId::WebGui,
    ];

    /// The design file's stem in lowercase, which also names `--screen` values and snapshots.
    pub fn slug(self) -> &'static str {
        match self {
            ScreenId::Main => "main",
            ScreenId::Search => "search",
            ScreenId::Detail => "detail",
            ScreenId::Review => "review",
            ScreenId::Installing => "installing",
            ScreenId::Unlock => "unlock",
            ScreenId::Feeds => "feeds",
            ScreenId::AddFeed => "addfeed",
            ScreenId::FeedKey => "feedkey",
            ScreenId::Settings => "settings",
            ScreenId::Reference => "reference",
            ScreenId::ApplyFailed => "applyfailed",
            ScreenId::Updates => "updates",
            ScreenId::UpdateFailed => "updatefailed",
            ScreenId::Logs => "logs",
            ScreenId::Rejected => "rejected",
            ScreenId::Capability => "capability",
            ScreenId::NoDrive => "nodrive",
            ScreenId::WebSites => "websites",
            ScreenId::WebExposure => "webexposure",
            ScreenId::WebProviders => "webproviders",
            ScreenId::WebCheck => "webcheck",
            ScreenId::WebConfirm => "webconfirm",
            ScreenId::WebCgnat => "webcgnat",
            ScreenId::AutoUpdate => "autoupdate",
            ScreenId::WebDomain => "webdomain",
            ScreenId::WebCertificate => "webcertificate",
            ScreenId::WebTrust => "webtrust",
            ScreenId::Setup => "setup",
            ScreenId::Backup => "backup",
            ScreenId::Onion => "onion",
            ScreenId::Restore => "restore",
            ScreenId::Rollbacks => "rollbacks",
            ScreenId::Network => "network",
            ScreenId::Apps => "apps",
            ScreenId::AppPage => "app",
            ScreenId::AppRemove => "appremove",
            ScreenId::System => "system",
            ScreenId::Bundle => "bundle",
            ScreenId::OwnerPage => "owner",
            ScreenId::WebGui => "webgui",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.slug() == slug)
    }
}
