//! Following a certificate while it is obtained, and the local CA that devices on the home
//! network can trust.

use crate::model::{CertRow, Snapshot};
use crate::Effect;

/// How often the page following a certificate reads the device's state again, in seconds.
pub const POLL_SECONDS: u64 = 2;

/// Reads the device's state again, for the certificate and trust pages, which show only that.
pub fn refresh() -> Vec<Effect> {
    vec![Effect::Refresh]
}

/// A public or local host while its certificate is obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Certificate {
    pub site: String,
    pub host: String,
}

impl Certificate {
    /// What a frontend runs every `POLL_SECONDS` while this page is shown.
    pub fn poll(&self) -> Vec<Effect> {
        refresh()
    }

    /// How the host's certificate stands, once the proxy reports one for it.
    pub fn row<'a>(&self, snapshot: &'a Snapshot) -> Option<&'a CertRow> {
        snapshot.web.cert(&self.host)
    }
}

/// The local CA: where devices on the home network get its root, and the fingerprint to compare
/// before trusting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trust {
    /// The page that offers the root, while it is served.
    pub url: Option<String>,
    /// SHA-256 of the root, once the proxy has created the CA.
    pub fingerprint: Option<String>,
}

impl Trust {
    pub fn of(snapshot: &Snapshot) -> Self {
        Self { url: snapshot.web.trust_url.clone(), fingerprint: snapshot.web.acme.local_ca.clone() }
    }

    /// The fingerprint in lines of 16 bytes, as the trust page itself shows it, so the two can be
    /// compared by eye.
    pub fn fingerprint_lines(&self) -> Vec<String> {
        let Some(fingerprint) = &self.fingerprint else { return Vec::new() };
        let parts: Vec<&str> = fingerprint.split(':').collect();
        parts.chunks(16).map(|chunk| chunk.join(":")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::load_snapshot;
    use crate::mock::MockBackend;

    #[test]
    fn a_certificate_page_polls_the_state_and_finds_its_row() {
        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let host = snapshot.web.certs.first().expect("the mock has certificates").host.clone();
        let page = Certificate { site: "shop".into(), host: host.clone() };
        assert_eq!(page.poll(), [Effect::Refresh]);
        assert_eq!(page.row(&snapshot).map(|row| row.host.as_str()), Some(host.as_str()));
        let unknown = Certificate { site: "shop".into(), host: "nowhere.example.com".into() };
        assert!(unknown.row(&snapshot).is_none());
    }

    #[test]
    fn the_fingerprint_is_shown_as_the_trust_page_shows_it() {
        let bytes: Vec<String> = (0..32).map(|byte| format!("{byte:02X}")).collect();
        let trust = Trust { url: None, fingerprint: Some(bytes.join(":")) };
        let lines = trust.fingerprint_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], bytes[..16].join(":"));
        assert_eq!(lines[1], bytes[16..].join(":"));
        assert!(Trust { url: None, fingerprint: None }.fingerprint_lines().is_empty());

        let mut mock = MockBackend::new();
        let snapshot = load_snapshot(&mut mock).expect("the mock answers");
        let trust = Trust::of(&snapshot);
        assert_eq!(trust.url, snapshot.web.trust_url);
        assert_eq!(trust.fingerprint, snapshot.web.acme.local_ca);
    }
}
