//! The owner page's JSON, version 1: what a package's own service answers at its manifest's
//! `[owner_page] url`. The helper fetches it, checks it with [`PageDoc::check`], fills in the items
//! it answers itself (sites, services, domains, backups) and hands the TUI a typed view.
//!
//! The document is untrusted. Every field has a length limit and control characters are refused
//! (a line break is allowed where a paragraph is), so nothing a package writes can move the
//! terminal's cursor or change its colours.
//!
//! ```json
//! {"version": 1, "title": "Pingers setup", "intro": "…",
//!  "steps": [{"title": "Put the four sites online", "state": "auto",
//!             "items": [{"kind": "text", "text": "…"},
//!                       {"kind": "value", "label": "PINGERS_SERVER_HOST", "value": "https://…"},
//!                       {"kind": "check", "label": "Push relay answers", "state": "ok", "detail": "…"},
//!                       {"kind": "qr", "label": "Scan with the phone's camera", "data": "https://…"},
//!                       {"kind": "site", "package": "pingers-server", "site": "api"},
//!                       {"kind": "service", "package": "pingers-server"},
//!                       {"kind": "domain", "domain": "example.org"},
//!                       {"kind": "backup"}]}]}
//! ```

use serde::{Deserialize, Serialize};

use crate::validate::{is_valid_domain, is_valid_id, is_valid_name};

/// The only version this build reads.
pub const PAGE_VERSION: u32 = 1;
/// The most the helper reads from the package's answer.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
pub const MAX_STEPS: usize = 32;
pub const MAX_ITEMS: usize = 32;
pub const MAX_TITLE_LEN: usize = 128;
pub const MAX_INTRO_LEN: usize = 1024;
pub const MAX_TEXT_LEN: usize = 2048;
pub const MAX_LABEL_LEN: usize = 128;
pub const MAX_VALUE_LEN: usize = 2048;
pub const MAX_DETAIL_LEN: usize = 512;
/// Fits a QR code at error correction level M (2331 bytes) with room to spare, and a terminal at
/// level L reasonably.
pub const MAX_QR_LEN: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageDoc {
    pub version: u32,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intro: Option<String>,
    pub steps: Vec<StepDoc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepDoc {
    pub title: String,
    pub state: StepState,
    #[serde(default)]
    pub items: Vec<ItemDoc>,
}

/// A step's state as the package sees it. `Auto` leaves it to nodo, from the step's checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepState {
    Done,
    Todo,
    Problem,
    Unknown,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckState {
    Ok,
    Failed,
    Waiting,
    Unknown,
}

/// One item of a step. `Site`, `Service`, `Domain` and `Backup` only name what to show; the helper
/// fills them in from its own state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ItemDoc {
    Text {
        text: String,
    },
    Value {
        label: String,
        value: String,
    },
    Check {
        label: String,
        state: CheckState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Qr {
        label: String,
        data: String,
    },
    Site {
        package: String,
        site: String,
    },
    Service {
        package: String,
    },
    Domain {
        domain: String,
    },
    Backup {},
}

/// Text of at most `max` characters, without control characters; a line break only where
/// `lines` allows it.
fn text(field: &str, value: &str, max: usize, lines: bool, empty_ok: bool) -> Result<(), String> {
    if !empty_ok && value.trim().is_empty() {
        return Err(format!("{field} is empty"));
    }
    if value.chars().count() > max {
        return Err(format!("{field} is longer than {max} characters"));
    }
    if value
        .chars()
        .any(|c| c.is_control() && !(lines && c == '\n'))
    {
        return Err(format!("{field} contains a control character"));
    }
    Ok(())
}

impl PageDoc {
    /// Every limit of version 1. The first problem found is the answer.
    pub fn check(&self) -> Result<(), String> {
        if self.version != PAGE_VERSION {
            return Err(format!(
                "version {} is not supported; this Nodo reads version {PAGE_VERSION}",
                self.version
            ));
        }
        text("title", &self.title, MAX_TITLE_LEN, false, false)?;
        if let Some(intro) = &self.intro {
            text("intro", intro, MAX_INTRO_LEN, true, true)?;
        }
        if self.steps.len() > MAX_STEPS {
            return Err(format!("it has more than {MAX_STEPS} steps"));
        }
        for (s, step) in self.steps.iter().enumerate() {
            text(
                &format!("steps[{s}].title"),
                &step.title,
                MAX_TITLE_LEN,
                false,
                false,
            )?;
            if step.items.len() > MAX_ITEMS {
                return Err(format!("steps[{s}] has more than {MAX_ITEMS} items"));
            }
            for (i, item) in step.items.iter().enumerate() {
                let at = |name: &str| format!("steps[{s}].items[{i}].{name}");
                match item {
                    ItemDoc::Text { text: body } => {
                        text(&at("text"), body, MAX_TEXT_LEN, true, false)?
                    }
                    ItemDoc::Value { label, value } => {
                        text(&at("label"), label, MAX_LABEL_LEN, false, false)?;
                        text(&at("value"), value, MAX_VALUE_LEN, true, false)?;
                    }
                    ItemDoc::Check { label, detail, .. } => {
                        text(&at("label"), label, MAX_LABEL_LEN, false, false)?;
                        if let Some(detail) = detail {
                            text(&at("detail"), detail, MAX_DETAIL_LEN, true, true)?;
                        }
                    }
                    ItemDoc::Qr { label, data } => {
                        text(&at("label"), label, MAX_LABEL_LEN, false, false)?;
                        text(&at("data"), data, MAX_QR_LEN, false, false)?;
                        if data.len() > MAX_QR_LEN {
                            return Err(format!(
                                "{} is longer than {MAX_QR_LEN} bytes",
                                at("data")
                            ));
                        }
                    }
                    ItemDoc::Site { package, site } => {
                        if !is_valid_id(package) {
                            return Err(format!("{} is not a package id", at("package")));
                        }
                        if !is_valid_name(site) {
                            return Err(format!("{} is not a site name", at("site")));
                        }
                    }
                    ItemDoc::Service { package } => {
                        if !is_valid_id(package) {
                            return Err(format!("{} is not a package id", at("package")));
                        }
                    }
                    ItemDoc::Domain { domain } => {
                        if !is_valid_domain(domain) {
                            return Err(format!("{} is not a domain name", at("domain")));
                        }
                    }
                    ItemDoc::Backup {} => {}
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "version": 1,
      "title": "Pingers setup",
      "intro": "optional short paragraph",
      "steps": [
        {
          "title": "Put the four sites online",
          "state": "auto",
          "items": [
            {"kind": "text", "text": "Line one.\nLine two."},
            {"kind": "value", "label": "PINGERS_SERVER_HOST", "value": "https://pingers.example.org"},
            {"kind": "check", "label": "Push relay answers", "state": "ok", "detail": "in 40 ms"},
            {"kind": "check", "label": "No detail", "state": "waiting"},
            {"kind": "qr", "label": "Scan with the phone's camera", "data": "https://pingers.example.org/j#s1abc"},
            {"kind": "site", "package": "pingers-server", "site": "api"},
            {"kind": "service", "package": "pingers-server"},
            {"kind": "domain", "domain": "example.org"},
            {"kind": "backup"}
          ]
        },
        {"title": "Nothing here yet", "state": "todo"}
      ]
    }"#;

    fn parse(text: &str) -> Result<PageDoc, String> {
        let doc: PageDoc = serde_json::from_str(text).map_err(|e| e.to_string())?;
        doc.check()?;
        Ok(doc)
    }

    #[test]
    fn the_sample_parses_and_round_trips() {
        let doc = parse(SAMPLE).unwrap();
        assert_eq!(doc.steps.len(), 2);
        assert_eq!(doc.steps[0].state, StepState::Auto);
        assert_eq!(doc.steps[0].items.len(), 9);
        assert_eq!(doc.steps[0].items[8], ItemDoc::Backup {});
        assert!(doc.steps[1].items.is_empty());
        let again = serde_json::to_string(&doc).unwrap();
        assert_eq!(parse(&again).unwrap(), doc);
    }

    #[test]
    fn unknown_fields_kinds_and_versions_are_refused() {
        for bad in [
            SAMPLE.replace("\"version\": 1", "\"version\": 2"),
            SAMPLE.replace("\"intro\"", "\"extra\": 1, \"intro\""),
            SAMPLE.replace(
                "{\"kind\": \"backup\"}",
                "{\"kind\": \"backup\", \"where\": \"usb\"}",
            ),
            SAMPLE.replace("\"kind\": \"backup\"", "\"kind\": \"shell\""),
            SAMPLE.replace("\"state\": \"auto\"", "\"state\": \"maybe\""),
            SAMPLE.replace("\"state\": \"ok\"", "\"state\": \"done\""),
        ] {
            assert!(parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn terminal_escapes_and_oversized_fields_are_refused() {
        let long = "x".repeat(MAX_TEXT_LEN + 1);
        for bad in [
            SAMPLE.replace("Pingers setup", "\\u001b[2JPingers"),
            SAMPLE.replace("Push relay answers", "Push\\nrelay"),
            SAMPLE.replace("Line one.", "\\u001b]52;c;eA==\\u0007"),
            SAMPLE.replace("Line one.\\nLine two.", &long),
            SAMPLE.replace(
                "https://pingers.example.org/j#s1abc",
                &"q".repeat(MAX_QR_LEN + 1),
            ),
            SAMPLE.replace("\"pingers-server\", \"site\"", "\"../etc\", \"site\""),
            SAMPLE.replace("\"example.org\"", "\"example\""),
            SAMPLE.replace("\"title\": \"Nothing here yet\"", "\"title\": \"\""),
        ] {
            assert!(parse(&bad).is_err(), "{bad}");
        }
        let many: Vec<String> = (0..=MAX_STEPS)
            .map(|i| format!("{{\"title\": \"s{i}\", \"state\": \"todo\"}}"))
            .collect();
        let doc = format!(
            "{{\"version\": 1, \"title\": \"t\", \"steps\": [{}]}}",
            many.join(",")
        );
        assert!(parse(&doc).is_err(), "too many steps");
    }
}
