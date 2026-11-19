//! Logs: one source's lines, only its problems, a search through them, and following it live.
//!
//! The sources are the device's (`Snapshot::log_sources`). A frontend keeps which one is shown
//! and the words being searched for; [`Logs`] keeps the lines and what narrows them.

use super::Step;
use crate::model::{LogKind, LogLine, LogView, Snapshot};
use crate::{Effect, Outcome};

/// How often a followed log is read again.
pub const FOLLOW_SECONDS: u64 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logs {
    /// The lines last read, and what the helper suggests about them.
    pub view: LogView,
    /// Only lines that report a problem are listed.
    pub errors_only: bool,
    /// The log is read again every [`FOLLOW_SECONDS`].
    pub follow: bool,
    /// Opened from an app's page: only that app's log. Otherwise the device logs, with every
    /// source to choose from.
    pub app: Option<String>,
}

impl Logs {
    /// One source's log, opened by name: an app's from its page, or a device part's.
    pub fn of(snapshot: &Snapshot, source: &str) -> Self {
        let is_app = snapshot.services.iter().any(|svc| svc.id == source);
        Self { view: LogView::default(), errors_only: false, follow: true, app: is_app.then(|| source.to_owned()) }
    }

    /// The device logs: every source, problems first.
    pub fn device() -> Self {
        Self { view: LogView::default(), errors_only: true, follow: true, app: None }
    }

    /// Reads a source's log. The answer is shown only if that source is still the one shown.
    pub fn load(source: &str) -> Vec<Effect> {
        vec![Effect::LoadLogs(source.to_owned())]
    }

    /// Reads the shown source again if the log is followed. A frontend calls this every
    /// [`FOLLOW_SECONDS`].
    pub fn follow_up(&self, shown: Option<&str>) -> Vec<Effect> {
        match shown {
            Some(source) if self.follow => Self::load(source),
            _ => Vec::new(),
        }
    }

    pub fn toggle_errors_only(&mut self) {
        self.errors_only = !self.errors_only;
    }

    pub fn toggle_follow(&mut self) {
        self.follow = !self.follow;
    }

    /// The lines to list: problems only when asked, and those that contain the words searched
    /// for, whatever their case.
    pub fn lines(&self, find: &str) -> Vec<&LogLine> {
        let needle = find.to_lowercase();
        self.view
            .lines
            .iter()
            .filter(|line| !self.errors_only || line.kind == LogKind::Error)
            .filter(|line| needle.is_empty() || line.text.to_lowercase().contains(&needle))
            .collect()
    }

    /// What to say when [`Logs::lines`] is empty.
    pub fn empty_text(&self) -> &'static str {
        if self.errors_only { "No problems in this log." } else { "Nothing in this log yet." }
    }

    /// Takes the lines of the source that is `shown`. A slow answer for a source the owner has
    /// already left does not replace what is shown.
    pub fn apply(&mut self, outcome: &Outcome, shown: Option<&str>) -> Step {
        if let Outcome::LogsLoaded { source, view } = outcome {
            if shown == Some(source.as_str()) {
                self.view = view.clone();
            }
        }
        Step::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{execute_effect, load_snapshot};
    use crate::mock::MockBackend;

    fn loaded(mock: &mut MockBackend, logs: &mut Logs, source: &str, shown: Option<&str>) {
        for effect in Logs::load(source) {
            for outcome in execute_effect(mock, effect) {
                assert_eq!(logs.apply(&outcome, shown), Step::Stay);
            }
        }
    }

    #[test]
    fn an_apps_log_is_its_own_and_the_device_logs_start_on_problems() {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let app = snapshot.services[0].id.clone();
        let logs = Logs::of(&snapshot, &app);
        assert_eq!((logs.app.as_deref(), logs.errors_only, logs.follow), (Some(app.as_str()), false, true));
        assert_eq!(Logs::of(&snapshot, "System").app, None);
        let device = Logs::device();
        assert_eq!((device.app, device.errors_only, device.follow), (None, true, true));
    }

    #[test]
    fn a_late_answer_for_another_source_is_not_shown() {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let (first, second) = (snapshot.log_sources[0].clone(), snapshot.log_sources[1].clone());
        let mut logs = Logs::of(&snapshot, &first);
        loaded(&mut mock, &mut logs, &first, Some(&first));
        let shown = logs.view.clone();
        assert!(!shown.lines.is_empty(), "the mock's first source has lines");

        loaded(&mut mock, &mut logs, &second, Some(&first));
        assert_eq!(logs.view, shown);
        loaded(&mut mock, &mut logs, &second, None);
        assert_eq!(logs.view, shown);
        loaded(&mut mock, &mut logs, &second, Some(&second));
        assert_eq!(logs.view, mock_view(&mut mock, &second));
    }

    fn mock_view(mock: &mut MockBackend, source: &str) -> LogView {
        use crate::model::ScreenData;
        mock.logs(source)
    }

    #[test]
    fn problems_only_and_the_search_narrow_the_lines() {
        let line = |kind, text: &str| LogLine { kind, text: text.to_owned() };
        let mut logs = Logs::device();
        logs.errors_only = false;
        logs.view.lines = vec![
            line(LogKind::Info, "Started moneropay"),
            line(LogKind::Error, "bind: Address already in use"),
            line(LogKind::Marker, "restart"),
            line(LogKind::Error, "moneropay exited with status 1"),
        ];
        assert_eq!(logs.lines("").len(), 4);
        let texts = |lines: Vec<&LogLine>| lines.into_iter().map(|l| l.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(logs.lines("MONEROPAY")), ["Started moneropay", "moneropay exited with status 1"]);

        logs.toggle_errors_only();
        assert_eq!(texts(logs.lines("")), ["bind: Address already in use", "moneropay exited with status 1"]);
        assert_eq!(texts(logs.lines("address")), ["bind: Address already in use"]);
        assert!(logs.lines("no such words").is_empty());
        assert_eq!(logs.empty_text(), "No problems in this log.");
        logs.toggle_errors_only();
        assert_eq!(logs.empty_text(), "Nothing in this log yet.");
    }

    #[test]
    fn only_a_followed_log_is_read_again() {
        let mut logs = Logs::device();
        assert_eq!(logs.follow_up(Some("System")), [Effect::LoadLogs("System".into())]);
        assert!(logs.follow_up(None).is_empty());
        logs.toggle_follow();
        assert!(logs.follow_up(Some("System")).is_empty());
    }
}
