//! A package's setup checklist for the owner, as the helper hands it to clients: the package's
//! own steps and items, checked and limited, with the items nodo answers (sites, services,
//! domains, backups) filled in from the helper's state.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerPageView {
    pub package: String,
    pub title: String,
    pub intro: Option<String>,
    pub steps: Vec<OwnerStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerStep {
    pub title: String,
    /// Already decided: a step the package left to nodo (`auto`) gets its state from its checks.
    pub state: OwnerStepState,
    pub items: Vec<OwnerItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerStepState {
    Done,
    Todo,
    Problem,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerCheckState {
    Ok,
    Failed,
    Waiting,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerCheck {
    pub label: String,
    pub state: OwnerCheckState,
    pub detail: Option<String>,
}

/// A DNS record the owner may need to create at their domain's provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsRecord {
    /// `A`, or `AAAA`.
    pub kind: String,
    /// Relative to `zone`, `@` for the zone itself.
    pub name: String,
    pub zone: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteFacts {
    pub package: String,
    pub site: String,
    /// The public host chosen in Hosting, or the address the app suggests when none is chosen.
    pub host: Option<String>,
    /// `host` is the app's suggestion, not a chosen address.
    pub suggested: bool,
    /// Public exposure is on.
    pub public: bool,
    /// The records to create by hand; empty when the public address is not known.
    pub records: Vec<DnsRecord>,
    /// The connected domain provider that publishes the records itself (dynamic DNS), by name.
    pub automatic_dns: Option<String>,
    pub checks: Vec<OwnerCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerItem {
    Text(String),
    /// Shown whole, with a way to copy it.
    Value {
        label: String,
        value: String,
    },
    Check(OwnerCheck),
    /// Drawn as a QR code, with the data below it as copyable text.
    Qr {
        label: String,
        data: String,
    },
    Site(SiteFacts),
    Service {
        package: String,
        check: OwnerCheck,
    },
    Domain {
        domain: String,
        provider: Option<String>,
        check: OwnerCheck,
    },
    Backup {
        check: OwnerCheck,
    },
}

impl OwnerItem {
    /// The checks this item carries, which an `auto` step's state follows.
    pub fn checks(&self) -> Vec<&OwnerCheck> {
        match self {
            OwnerItem::Check(check)
            | OwnerItem::Service { check, .. }
            | OwnerItem::Domain { check, .. }
            | OwnerItem::Backup { check } => vec![check],
            OwnerItem::Site(site) => site.checks.iter().collect(),
            OwnerItem::Text(_) | OwnerItem::Value { .. } | OwnerItem::Qr { .. } => Vec::new(),
        }
    }
}

/// An `auto` step's state from its checks: done when every check passed, a problem when any
/// failed, otherwise still to do. A step with no checks is unknown.
pub fn auto_state(items: &[OwnerItem]) -> OwnerStepState {
    let checks: Vec<&OwnerCheck> = items.iter().flat_map(OwnerItem::checks).collect();
    if checks.is_empty() {
        OwnerStepState::Unknown
    } else if checks.iter().any(|c| c.state == OwnerCheckState::Failed) {
        OwnerStepState::Problem
    } else if checks.iter().all(|c| c.state == OwnerCheckState::Ok) {
        OwnerStepState::Done
    } else {
        OwnerStepState::Todo
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(state: OwnerCheckState) -> OwnerItem {
        OwnerItem::Check(OwnerCheck {
            label: "c".into(),
            state,
            detail: None,
        })
    }

    #[test]
    fn auto_follows_the_checks() {
        use OwnerCheckState::*;
        assert_eq!(auto_state(&[]), OwnerStepState::Unknown);
        assert_eq!(
            auto_state(&[OwnerItem::Text("t".into())]),
            OwnerStepState::Unknown
        );
        assert_eq!(auto_state(&[check(Ok), check(Ok)]), OwnerStepState::Done);
        assert_eq!(
            auto_state(&[check(Ok), check(Waiting)]),
            OwnerStepState::Todo
        );
        assert_eq!(
            auto_state(&[check(Ok), check(Unknown)]),
            OwnerStepState::Todo
        );
        assert_eq!(
            auto_state(&[check(Waiting), check(Failed)]),
            OwnerStepState::Problem
        );
        let site = OwnerItem::Site(SiteFacts {
            package: "p".into(),
            site: "s".into(),
            host: None,
            suggested: false,
            public: false,
            records: vec![],
            automatic_dns: None,
            checks: vec![OwnerCheck {
                label: "x".into(),
                state: Failed,
                detail: None,
            }],
        });
        assert_eq!(auto_state(&[check(Ok), site]), OwnerStepState::Problem);
    }
}
