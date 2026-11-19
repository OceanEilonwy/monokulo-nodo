//! Changes kept aside: a page's draft that the owner saved in the interface but has not put on
//! the device yet.
//!
//! Three pages edit a draft before anything reaches the helper: an app's settings
//! (`settings::Settings`), a site's hosting (`web::hosting::Hosting`) and the automatic-update
//! choices (`updates::AutoUpdate`). A frontend that keeps those drafts between visits, as the web
//! pages do in their session, offers three buttons on each:
//!
//! - **Save** keeps the draft as it is and leaves the page. Nothing reaches the device; the
//!   draft is staged ([`saved_notice`]).
//! - **Save & Apply** is the flow's own save (`Settings::save`, `Hosting::save`,
//!   `AutoUpdate::save`), with its questions and checks, which puts the draft live.
//! - **Reset** drops the draft, so the page shows the device's state again.
//!
//! [`Stageable`] counts a draft's changes against the device's [`Snapshot`], and [`summary`]
//! lists the drafts that still change something, for a count of unsaved changes beside the
//! navigation and a page that lists them. A draft whose changes the device has caught up with
//! (it was applied elsewhere) counts nothing and is left out.
//!
//! The drafts themselves stay the frontend's to keep: this module holds no state.

use super::settings::Settings;
use super::updates::AutoUpdate;
use super::web::hosting::Hosting;
use super::{Dest, Notice};
use crate::model::Snapshot;

/// Which page a staged draft belongs to. The order is the order of [`summary`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StageKey {
    /// The automatic-update choices.
    AutoUpdate,
    /// One app's settings, by app.
    Settings(String),
    /// One site's hosting, by site.
    Hosting(String),
}

impl StageKey {
    /// The page's name, for a list of changes: `Automatic updates`, `moneropay settings`.
    pub fn title(&self) -> String {
        match self {
            Self::AutoUpdate => "Automatic updates".to_owned(),
            Self::Settings(package) => format!("{package} settings"),
            Self::Hosting(site) => format!("{site} hosting"),
        }
    }

    /// The page where the draft is changed and applied, where the frontend can name it. The
    /// settings page needs the form the helper gives, and automatic updates have no `Dest`, so
    /// a frontend maps those itself.
    pub fn dest(&self) -> Option<Dest> {
        match self {
            Self::AutoUpdate | Self::Settings(_) => None,
            Self::Hosting(site) => Some(Dest::AppPage(site.clone())),
        }
    }
}

/// A draft that can be staged.
pub trait Stageable {
    /// The page it belongs to.
    fn stage_key(&self) -> StageKey;

    /// How many of its choices differ from what the device has now. Zero means applying it
    /// would change nothing.
    fn changes(&self, snapshot: &Snapshot) -> usize;
}

impl Stageable for Settings {
    fn stage_key(&self) -> StageKey {
        StageKey::Settings(self.form.package.clone())
    }

    /// The fields changed from the form as the helper gave it.
    fn changes(&self, _snapshot: &Snapshot) -> usize {
        self.changed_indices().len()
    }
}

impl Stageable for AutoUpdate {
    fn stage_key(&self) -> StageKey {
        StageKey::AutoUpdate
    }

    /// Against the choices stored now: how often, the window (one change for either end), and
    /// each app turned on or off. An app the device no longer lists does not count.
    fn changes(&self, snapshot: &Snapshot) -> usize {
        let live = &snapshot.auto_update;
        let draft = &self.draft;
        let frequency = usize::from(draft.frequency != live.frequency);
        let window = usize::from(draft.window_start != live.window_start || draft.window_end != live.window_end);
        let apps = draft
            .packages
            .iter()
            .filter(|entry| live.packages.iter().any(|stored| stored.name == entry.name && stored.enabled != entry.enabled))
            .count();
        frequency + window + apps
    }
}

impl Stageable for Hosting {
    fn stage_key(&self) -> StageKey {
        StageKey::Hosting(self.site.clone())
    }

    /// Against the site as the device serves it: each of home network, Tor and internet
    /// turned on or off; and, with the internet ticked and a domain to use, the main address
    /// and each other name added or taken away. A site that is gone counts nothing.
    fn changes(&self, snapshot: &Snapshot) -> usize {
        let Some(site) = snapshot.web.sites.iter().find(|site| site.package == self.site) else {
            return 0;
        };
        let (draft, live) = (self.draft, site.exposure);
        let reach = [draft.local != live.local, draft.onion != live.onion, draft.public != live.public]
            .into_iter()
            .filter(|changed| *changed)
            .count();
        if !(draft.public && self.has_provider) {
            return reach;
        }
        let host = usize::from(site.public_host.as_deref() != Some(self.host().as_str()));
        let added = self.aliases.iter().filter(|alias| !site.public_aliases.contains(alias)).count();
        let removed = site.public_aliases.iter().filter(|alias| !self.aliases.contains(alias)).count();
        reach + host + added + removed
    }
}

/// One staged draft, as a list of changes shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedChange {
    pub key: StageKey,
    /// [`StageKey::title`].
    pub title: String,
    /// [`Stageable::changes`], never zero.
    pub changes: usize,
}

/// The drafts that still change something, in [`StageKey`] order.
pub fn summary<'a>(drafts: impl IntoIterator<Item = &'a dyn Stageable>, snapshot: &Snapshot) -> Vec<StagedChange> {
    let mut list: Vec<StagedChange> = drafts
        .into_iter()
        .filter_map(|draft| {
            let changes = draft.changes(snapshot);
            (changes > 0).then(|| {
                let key = draft.stage_key();
                StagedChange { title: key.title(), key, changes }
            })
        })
        .collect();
    list.sort_by(|a, b| a.key.cmp(&b.key));
    list
}

/// Every change of every staged draft, for the count beside the navigation.
pub fn total(list: &[StagedChange]) -> usize {
    list.iter().map(|change| change.changes).sum()
}

/// What keeping a draft says: it is kept, and is not on the device yet.
pub fn saved_notice(key: &StageKey, changes: usize) -> Notice {
    let what = if changes == 1 { "1 change".to_owned() } else { format!("{changes} changes") };
    Notice::info(format!("{} kept: {what}, not on the Nodo yet. Apply them to put them live.", key.title()))
}

/// What keeping a draft says when it changes nothing, so nothing was kept.
pub fn nothing_to_keep(key: &StageKey) -> Notice {
    Notice::info(format!("{} has no changes to keep.", key.title()))
}

/// What dropping a draft says.
pub fn reset_notice(key: &StageKey) -> Notice {
    Notice::info(format!("{}: changes dropped. It shows what the Nodo has.", key.title()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::load_snapshot;
    use crate::flows::web::hosting;
    use crate::mock::MockBackend;
    use nodo_api::{AliasMode, PublicAlias};

    fn snapshot() -> Snapshot {
        let mut mock = MockBackend::new();
        load_snapshot(&mut mock).expect("the mock answers")
    }

    #[test]
    fn an_untouched_draft_changes_nothing_and_is_not_listed() {
        let snapshot = snapshot();
        let auto = AutoUpdate::new(snapshot.auto_update.clone());
        let site = snapshot.web.sites.first().expect("the mock has sites").package.clone();
        let host = hosting::open(&snapshot, &site).expect("a site");
        assert_eq!(auto.changes(&snapshot), 0);
        assert_eq!(host.changes(&snapshot), 0);
        let drafts: [&dyn Stageable; 2] = [&auto, &host];
        assert!(summary(drafts, &snapshot).is_empty());
    }

    #[test]
    fn automatic_updates_count_how_often_the_window_and_each_app() {
        let snapshot = snapshot();
        let mut auto = AutoUpdate::new(snapshot.auto_update.clone());
        auto.next_frequency();
        let frequency_changed = auto.draft.frequency != snapshot.auto_update.frequency;
        assert!(auto.set_window("01:00 to 02:00"));
        let first = snapshot.auto_update.packages.first().expect("the mock lists apps").name.clone();
        auto.toggle(&first);
        assert_eq!(auto.changes(&snapshot), usize::from(frequency_changed) + 1 + 1);
        auto.toggle(&first);
        assert_eq!(auto.changes(&snapshot), usize::from(frequency_changed) + 1);
    }

    #[test]
    fn hosting_counts_reach_address_and_names() {
        let snapshot = snapshot();
        let site = snapshot.web.sites.iter().find(|site| !site.exposure.public).expect("a site not public").clone();
        let mut host = hosting::open(&snapshot, &site.package).expect("a site");
        host.set_exposure(!site.exposure.local, site.exposure.onion, site.exposure.public);
        assert_eq!(host.changes(&snapshot), 1);
        host.set_exposure(site.exposure.local, site.exposure.onion, site.exposure.public);
        assert_eq!(host.changes(&snapshot), 0);
        // Going public with a provider: the internet, the new address and a new name.
        if host.has_provider {
            host.set_exposure(site.exposure.local, site.exposure.onion, true);
            let with_address = host.changes(&snapshot);
            assert!(with_address >= 1);
            host.aliases.push(PublicAlias { host: format!("www.{}", host.host()), mode: AliasMode::Redirect });
            assert_eq!(host.changes(&snapshot), with_address + 1);
        }
    }

    #[test]
    fn the_summary_lists_changed_drafts_in_order_and_totals_them() {
        let snapshot = snapshot();
        let mut auto = AutoUpdate::new(snapshot.auto_update.clone());
        assert!(auto.set_window("01:00 to 02:00"));
        let site = snapshot.web.sites.first().expect("a site").clone();
        let mut host = hosting::open(&snapshot, &site.package).expect("a site");
        host.set_exposure(!site.exposure.local, !site.exposure.onion, site.exposure.public);
        let drafts: [&dyn Stageable; 2] = [&host, &auto];
        let list = summary(drafts, &snapshot);
        assert_eq!(list.iter().map(|change| change.key.clone()).collect::<Vec<_>>(), [StageKey::AutoUpdate, StageKey::Hosting(site.package.clone())]);
        assert_eq!(list[0].title, "Automatic updates");
        assert_eq!(total(&list), 3);
    }

    #[test]
    fn the_notices_say_what_happened() {
        let key = StageKey::Settings("moneropay".to_owned());
        assert_eq!(saved_notice(&key, 2).text, "moneropay settings kept: 2 changes, not on the Nodo yet. Apply them to put them live.");
        assert_eq!(saved_notice(&key, 1).text, "moneropay settings kept: 1 change, not on the Nodo yet. Apply them to put them live.");
        assert_eq!(reset_notice(&StageKey::AutoUpdate).text, "Automatic updates: changes dropped. It shows what the Nodo has.");
    }
}
