//! Parsing of `$(name.setting)` references and the escape form for a literal `$(`.

use std::fmt;

use nodo_manifest::is_valid_name;
use thiserror::Error;

use crate::schema::is_valid_setting_name;

/// Longer templates are refused rather than scanned.
pub const MAX_TEMPLATE_LEN: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TemplateError {
    #[error("'$(' has no closing ')'. Write '$$(' for a literal '$('")]
    Unterminated,
    #[error("invalid reference '$({0})'. References look like $(package.setting), and '$$(' is a literal '$('")]
    Invalid(String),
    #[error("the text is too long")]
    TooLong,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Reference {
    /// A package id or a role.
    pub target: String,
    pub setting: String,
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "$({}.{})", self.target, self.setting)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Literal(String),
    Reference(Reference),
}

fn parse_reference(inner: &str) -> Result<Reference, TemplateError> {
    let invalid = || TemplateError::Invalid(inner.chars().take(64).filter(|c| !c.is_control()).collect());
    let (target, setting) = inner.split_once('.').ok_or_else(invalid)?;
    if !is_valid_name(target) || !is_valid_setting_name(setting) {
        return Err(invalid());
    }
    Ok(Reference { target: target.to_string(), setting: setting.to_string() })
}

/// Splits text into literal runs and references, decoding the escape form.
pub fn parse_template(text: &str) -> Result<Vec<Part>, TemplateError> {
    if text.len() > MAX_TEMPLATE_LEN {
        return Err(TemplateError::TooLong);
    }
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut rest = text;
    while let Some(dollar) = rest.find('$') {
        literal.push_str(&rest[..dollar]);
        let run = &rest[dollar..];
        let count = run.bytes().take_while(|b| *b == b'$').count();
        let after = &run[count..];
        if !after.starts_with('(') {
            literal.push_str(&run[..count]);
            rest = after;
        } else if count > 1 {
            literal.push_str(&run[1..count]);
            literal.push('(');
            rest = &after[1..];
        } else {
            let close = after.find(')').ok_or(TemplateError::Unterminated)?;
            let reference = parse_reference(&after[1..close])?;
            if !literal.is_empty() {
                parts.push(Part::Literal(std::mem::take(&mut literal)));
            }
            parts.push(Part::Reference(reference));
            rest = &after[close + 1..];
        }
    }
    literal.push_str(rest);
    if !literal.is_empty() {
        parts.push(Part::Literal(literal));
    }
    Ok(parts)
}

/// Escapes text so [`parse_template`] returns it unchanged, with no references.
pub fn escape_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(dollar) = rest.find('$') {
        out.push_str(&rest[..dollar]);
        let run = &rest[dollar..];
        let count = run.bytes().take_while(|b| *b == b'$').count();
        if run[count..].starts_with('(') {
            out.push('$');
        }
        out.push_str(&run[..count]);
        rest = &run[count..];
    }
    out.push_str(rest);
    out
}

/// The literal text of parts known to hold no references.
pub(crate) fn literal_text(parts: &[Part]) -> Option<String> {
    match parts {
        [] => Some(String::new()),
        [Part::Literal(text)] => Some(text.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(target: &str, setting: &str) -> Part {
        Part::Reference(Reference { target: target.into(), setting: setting.into() })
    }

    fn lit(text: &str) -> Part {
        Part::Literal(text.into())
    }

    #[test]
    fn parses_references_and_surrounding_text() {
        assert_eq!(parse_template("$(a.b)").unwrap(), [reference("a", "b")]);
        assert_eq!(
            parse_template("http://$(monero-daemon.host):$(monero-daemon.rpc_port)/x").unwrap(),
            [
                lit("http://"),
                reference("monero-daemon", "host"),
                lit(":"),
                reference("monero-daemon", "rpc_port"),
                lit("/x"),
            ]
        );
        assert!(parse_template("").unwrap().is_empty());
    }

    #[test]
    fn dollar_alone_is_literal() {
        for text in ["$", "$$", "a$b", "$5.00", "cost: $", "$)", "$ (a.b)"] {
            assert_eq!(parse_template(text).unwrap(), [lit(text)], "{text}");
        }
    }

    #[test]
    fn escape_form_yields_a_literal() {
        assert_eq!(parse_template("$$(a.b)").unwrap(), [lit("$(a.b)")]);
        assert_eq!(parse_template("$$$(a.b)").unwrap(), [lit("$$(a.b)")]);
        assert_eq!(parse_template("x$$(").unwrap(), [lit("x$(")]);
        assert_eq!(parse_template("$$(a.b)$(c.d)").unwrap(), [lit("$(a.b)"), reference("c", "d")]);
    }

    #[test]
    fn rejects_malformed_references() {
        for text in ["$(", "$(a", "$(a.b", "x$(y"] {
            assert_eq!(parse_template(text), Err(TemplateError::Unterminated), "{text}");
        }
        for text in [
            "$()",
            "$(a)",
            "$(.b)",
            "$(a.)",
            "$(a.b.c)",
            "$(A.b)",
            "$(a.B)",
            "$( a.b)",
            "$(a.b )",
            "$($(a.b).c)",
            "$(a;rm -rf /.b)",
            "$(../x.b)",
            "$(a.b\n)",
        ] {
            assert!(matches!(parse_template(text), Err(TemplateError::Invalid(_))), "{text}");
        }
    }

    #[test]
    fn refuses_oversized_templates() {
        assert_eq!(parse_template(&"x".repeat(MAX_TEMPLATE_LEN + 1)), Err(TemplateError::TooLong));
    }

    #[test]
    fn multibyte_text_survives() {
        assert_eq!(parse_template("é$(a.b)ü$").unwrap(), [lit("é"), reference("a", "b"), lit("ü$")]);
    }

    #[test]
    fn escape_round_trips() {
        for text in ["", "plain", "$", "$(", "$$(", "$$$(a.b)", "a$(b)$c$$d(", "$(a.b)", "é$(ü", "((($"] {
            let parts = parse_template(&escape_literal(text)).unwrap();
            assert_eq!(literal_text(&parts).as_deref(), Some(text), "{text}");
        }
    }

    #[test]
    fn escape_round_trips_for_generated_text() {
        // Every string over this alphabet up to length 6 covers all the run and adjacency cases.
        let alphabet = ['$', '(', ')', 'a', '.'];
        let mut pending = vec![String::new()];
        while let Some(text) = pending.pop() {
            let parts = parse_template(&escape_literal(&text)).unwrap();
            assert_eq!(literal_text(&parts).as_deref(), Some(text.as_str()));
            if text.len() < 6 {
                pending.extend(alphabet.iter().map(|c| format!("{text}{c}")));
            }
        }
    }
}
