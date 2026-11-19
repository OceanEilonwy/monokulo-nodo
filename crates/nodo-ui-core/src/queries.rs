//! What the screens work out from a `Snapshot` alone, so every interface counts and orders the
//! same way.

use crate::model::*;

/// What Home lets one act on, in order: what needs the owner first (each with its one button),
/// then every app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeItem {
    Unlock,
    /// An app that keeps failing: its log.
    Failing(String),
    Updates,
    App(String),
}

/// What the owner meets where Home is: the first of these that applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landing {
    /// The device has no passphrase yet: first setup comes before anything else.
    Setup,
    /// The data drive is missing or cannot be used: what is wrong with it, and what can be done.
    NoDrive,
    Home,
}

/// The rows of the app page, in order.
pub const APP_SECTIONS: [&str; 3] = ["Hosting", "Settings", "Logs"];

impl Snapshot {
    /// Where the owner lands: first setup on a device without a passphrase, the data drive's
    /// page while the drive is missing, Home otherwise.
    pub fn landing(&self) -> Landing {
        if self.overview.setup_needed { Landing::Setup } else { self.landing_once_set_up() }
    }

    /// The same without first setup: what is under its page, and what every interface that
    /// only exists on a device that is set up shows.
    pub fn landing_once_set_up(&self) -> Landing {
        if self.overview.drive.is_some() { Landing::NoDrive } else { Landing::Home }
    }

    /// The sites that are online somewhere: on the home network, Tor or the internet.
    pub fn active_sites(&self) -> Vec<&Site> {
        self.web.sites.iter().filter(|site| site.exposure.local || site.exposure.onion || site.exposure.public).collect()
    }

    /// A domain provider is connected with at least one domain an app can use.
    pub fn has_provider(&self) -> bool {
        !self.domain_choices().is_empty()
    }

    /// The domains apps can be put under, each with its provider's name. A domain another provider
    /// already holds is left out: it is used through that one.
    pub fn domain_choices(&self) -> Vec<(String, String)> {
        let mut choices: Vec<(String, String)> = self
            .connected
            .iter()
            .flat_map(|p| p.domains.iter().filter(|d| d.held_by.is_none()).map(move |d| (d.name.clone(), p.name.clone())))
            .collect();
        // Before the helper reports providers (or with an older one), the single connected domain.
        if choices.is_empty() && !self.web.domain.is_empty() && !self.web.provider.is_empty() {
            choices.push((self.web.domain.clone(), self.web.provider.clone()));
        }
        choices
    }

    /// Held-back versions are not offered, so they do not count towards the badge.
    pub fn pending_updates(&self) -> usize {
        self.updates.iter().filter(|item| !item.held).count()
    }

    /// The device key is not in memory. The helper says so even when no app waits, such as
    /// before the first encrypted app is installed, and DNS keys still need it.
    pub fn is_locked(&self) -> bool {
        self.overview.locked || self.services.iter().any(|svc| svc.state == nodo_api::ServiceState::Locked)
    }

    /// Available updates first, then held ones, matching the rows the Updates screen shows.
    pub fn update_rows(&self) -> Vec<&UpdateItem> {
        let (held, available): (Vec<_>, Vec<_>) = self.updates.iter().partition(|item| item.held);
        available.into_iter().chain(held).collect()
    }

    /// The catalog's apps whose name or tagline holds `query`, from one source when `feed` names it.
    pub fn search_results(&self, query: &str, feed: Option<&str>) -> Vec<&PackageSummary> {
        let query = query.to_lowercase();
        self.catalog
            .iter()
            .filter(|pkg| feed.is_none_or(|feed| pkg.id.feed == feed))
            .filter(|pkg| {
                pkg.id.name.to_lowercase().contains(&query) || pkg.tagline.to_lowercase().contains(&query)
            })
            .collect()
    }

    pub fn home_items(&self) -> Vec<HomeItem> {
        let mut items = Vec::new();
        if self.is_locked() {
            items.push(HomeItem::Unlock);
        }
        for svc in self.services.iter().filter(|svc| svc.state == nodo_api::ServiceState::Failed) {
            items.push(HomeItem::Failing(svc.id.clone()));
        }
        if self.pending_updates() > 0 {
            items.push(HomeItem::Updates);
        }
        items.extend(self.services.iter().map(|svc| HomeItem::App(svc.id.clone())));
        items
    }

    /// The rows of an app's page: its setup page first when it has one, then Hosting, Settings and
    /// Logs.
    pub fn app_sections(&self, name: &str) -> Vec<String> {
        let mut rows: Vec<String> = self.owner_pages.iter().filter(|(p, _)| p == name).map(|(_, t)| t.clone()).collect();
        rows.extend(APP_SECTIONS.iter().map(|s| (*s).to_owned()));
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_comes_before_a_missing_drive_and_that_before_home() {
        let mut snapshot = Snapshot::default();
        assert_eq!(snapshot.landing(), Landing::Home);
        snapshot.overview.drive = Some(DriveFailure { message: "not connected".into(), checks: Vec::new(), waiting: String::new() });
        assert_eq!((snapshot.landing(), snapshot.landing_once_set_up()), (Landing::NoDrive, Landing::NoDrive));
        snapshot.overview.setup_needed = true;
        assert_eq!((snapshot.landing(), snapshot.landing_once_set_up()), (Landing::Setup, Landing::NoDrive));
        snapshot.overview.drive = None;
        assert_eq!((snapshot.landing(), snapshot.landing_once_set_up()), (Landing::Setup, Landing::Home));
    }
}
