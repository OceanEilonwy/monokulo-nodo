//! References in settings: the editor for text with reference chips, which references fit a
//! field, and the search the picker and the `$(` suggestions share.
//!
//! A value is typed text and `$(app.setting)` references kept apart as pieces, so nothing typed
//! ever becomes a reference: a chip only comes from the picker or a suggestion. The helper
//! escapes the text pieces when it stores them.

use crate::model::{FieldKind, Piece, ReferenceOption};

/// One cursor step in the editor: a typed character, or a whole reference chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unit {
    Char(char),
    Ref { app: String, setting: String },
}

/// Text with reference chips, being edited. A chip is one unit, so the cursor steps over it and
/// Backspace removes it whole.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PieceEditor {
    pub units: Vec<Unit>,
    pub cursor: usize,
    /// The highlighted suggestion while `$(` is being completed.
    pub suggestion: usize,
    /// Esc hides the suggestions until the next character is typed.
    pub suggestions_closed: bool,
}

impl PieceEditor {
    pub fn new(pieces: &[Piece]) -> Self {
        let mut units = Vec::new();
        for piece in pieces {
            match piece {
                Piece::Text(text) => units.extend(text.chars().map(Unit::Char)),
                Piece::Ref { app, setting } => units.push(Unit::Ref { app: app.clone(), setting: setting.clone() }),
            }
        }
        let cursor = units.len();
        Self { units, cursor, suggestion: 0, suggestions_closed: false }
    }

    pub fn pieces(&self) -> Vec<Piece> {
        let mut pieces = Vec::new();
        let mut text = String::new();
        for unit in &self.units {
            match unit {
                Unit::Char(c) => text.push(*c),
                Unit::Ref { app, setting } => {
                    if !text.is_empty() {
                        pieces.push(Piece::Text(std::mem::take(&mut text)));
                    }
                    pieces.push(Piece::Ref { app: app.clone(), setting: setting.clone() });
                }
            }
        }
        if !text.is_empty() {
            pieces.push(Piece::Text(text));
        }
        pieces
    }

    /// Whether there is anything besides one reference, which decides whether a new reference
    /// would be assembled into text.
    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    pub fn insert_char(&mut self, c: char) {
        self.units.insert(self.cursor, Unit::Char(c));
        self.cursor += 1;
        self.suggestion = 0;
        self.suggestions_closed = false;
    }

    pub fn insert_ref(&mut self, app: &str, setting: &str) {
        self.units.insert(self.cursor, Unit::Ref { app: app.to_owned(), setting: setting.to_owned() });
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.units.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.units.len() {
            self.units.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.units.len());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.units.len();
    }

    /// What was typed after an open `$(` just before the cursor, while it can still be a
    /// reference name, with where the `$` is. `$$(` is a literal and is never completed.
    pub fn completing(&self) -> Option<(usize, String)> {
        let mut at = self.cursor;
        while at > 0 {
            match &self.units[at - 1] {
                Unit::Char(c) if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') => at -= 1,
                _ => break,
            }
        }
        if at < 2 || self.units[at - 1] != Unit::Char('(') || self.units[at - 2] != Unit::Char('$') {
            return None;
        }
        let dollar = at - 2;
        if dollar > 0 && self.units[dollar - 1] == Unit::Char('$') {
            return None;
        }
        let query = self.units[at..self.cursor]
            .iter()
            .map(|unit| match unit {
                Unit::Char(c) => *c,
                Unit::Ref { .. } => ' ',
            })
            .collect();
        Some((dollar, query))
    }

    /// Replaces the `$(…` being completed with a chip.
    pub fn complete(&mut self, app: &str, setting: &str) {
        if let Some((dollar, _)) = self.completing() {
            self.units.drain(dollar..self.cursor);
            self.cursor = dollar;
        }
        self.insert_ref(app, setting);
    }
}

/// A short name for a kind, as the picker's type column shows it.
pub fn kind_name(kind: &FieldKind) -> &'static str {
    match kind {
        FieldKind::Url => "url",
        FieldKind::Port | FieldKind::Number { .. } => "number",
        FieldKind::Text => "text",
        FieldKind::Choice(_) => "choice",
        FieldKind::MoneroAddress => "address",
        FieldKind::Secret => "secret",
        FieldKind::Toggle => "on/off",
    }
}

fn kind_words(kind: &FieldKind) -> &'static str {
    match kind {
        FieldKind::Url => "an address",
        FieldKind::Port | FieldKind::Number { .. } => "a number",
        FieldKind::Text => "text",
        FieldKind::Choice(_) => "a choice",
        FieldKind::MoneroAddress => "a Monero address",
        FieldKind::Secret => "a secret",
        FieldKind::Toggle => "on or off",
    }
}

/// Whether a reference to `option` can go into a field of kind `field`, and if not, why, in the
/// picker's words. `assembled` is true when the field has other text or references too, which
/// only text and addresses can hold. These are the resolver's rules; the helper checks again.
pub fn fits(field: &FieldKind, option: &ReferenceOption, assembled: bool) -> Result<(), String> {
    let secret = option.locked.is_some_and(|locked| locked.secret) || option.kind == FieldKind::Secret;
    if *field == FieldKind::Secret {
        return if assembled { Err("A secret setting takes one reference on its own.".to_owned()) } else { Ok(()) };
    }
    if secret {
        return Err("A secret can only go into a secret setting.".to_owned());
    }
    if assembled {
        return match field {
            FieldKind::Text | FieldKind::Url => Ok(()),
            _ => Err(format!("This field takes {}, so it can't mix text and references.", kind_words(field))),
        };
    }
    let ok = match field {
        FieldKind::Text => true,
        FieldKind::Port | FieldKind::Number { .. } => matches!(option.kind, FieldKind::Port | FieldKind::Number { .. }),
        other => std::mem::discriminant(other) == std::mem::discriminant(&option.kind),
    };
    if ok {
        Ok(())
    } else {
        let mut what = kind_words(&option.kind).to_owned();
        if let Some(first) = what.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        Err(format!("{what}; the field takes {}.", kind_words(field)))
    }
}

/// Whether every character of `query` appears in `text` in order, ignoring case: "rpcp" finds
/// "rpc_port". An empty query matches everything.
pub fn matches(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    let mut chars = text.chars();
    query.to_lowercase().chars().filter(|c| !c.is_whitespace()).all(|q| chars.any(|c| c == q))
}

/// The options a search shows: those whose app, name, label, description or value match. Names
/// that contain the query come first, then other text that contains it, then looser matches, each
/// in the helper's order (by app). Values of locked options are never known, so never searched.
pub fn search<'a>(options: &'a [ReferenceOption], query: &str) -> Vec<&'a ReferenceOption> {
    let needle = query.trim().to_lowercase();
    let mut found: Vec<(u8, &ReferenceOption)> = options
        .iter()
        .filter_map(|o| {
            let path = o.path().to_lowercase();
            let hay = format!("{path} {} {} {}", o.label, o.help, o.value.as_deref().unwrap_or_default()).to_lowercase();
            let rank = if path.contains(&needle) {
                0
            } else if hay.contains(&needle) {
                1
            } else if matches(&needle, &hay) {
                2
            } else {
                return None;
            };
            Some((rank, o))
        })
        .collect();
    found.sort_by_key(|(rank, _)| *rank);
    found.into_iter().map(|(_, o)| o).collect()
}

/// The order the picker lists options in: those that fit the field, then those that don't
/// (dimmed, with why), then the ones that need approval. Within each group, by app as listed.
pub fn ordered<'a>(field: &FieldKind, options: Vec<&'a ReferenceOption>, assembled: bool) -> Vec<(&'a ReferenceOption, Result<(), String>)> {
    let mut fit = Vec::new();
    let mut misfit = Vec::new();
    let mut locked = Vec::new();
    for option in options {
        let verdict = fits(field, option, assembled);
        if option.locked.is_some_and(|l| !l.granted) {
            locked.push((option, verdict));
        } else if verdict.is_ok() {
            fit.push((option, verdict));
        } else {
            misfit.push((option, verdict));
        }
    }
    fit.into_iter().chain(misfit).chain(locked).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Locked;

    fn option(app: &str, setting: &str, kind: FieldKind) -> ReferenceOption {
        ReferenceOption {
            app: app.into(),
            setting: setting.into(),
            label: String::new(),
            help: String::new(),
            kind,
            value: Some("v".into()),
            locked: None,
        }
    }

    #[test]
    fn a_chip_is_one_step_for_the_cursor_and_backspace() {
        let mut editor = PieceEditor::new(&[
            Piece::Text("http://".into()),
            Piece::Ref { app: "cuprated".into(), setting: "rpc_host".into() },
            Piece::Text(":1".into()),
        ]);
        assert_eq!(editor.units.len(), 10);
        editor.left();
        editor.left();
        editor.backspace();
        assert_eq!(editor.pieces(), vec![Piece::Text("http://:1".into())]);
    }

    #[test]
    fn typing_dollar_paren_completes_into_a_chip() {
        let mut editor = PieceEditor::new(&[Piece::Text("http://".into())]);
        for c in "$(cu".chars() {
            editor.insert_char(c);
        }
        assert_eq!(editor.completing(), Some((7, "cu".into())));
        editor.complete("cuprated", "rpc_host");
        assert_eq!(
            editor.pieces(),
            vec![Piece::Text("http://".into()), Piece::Ref { app: "cuprated".into(), setting: "rpc_host".into() }]
        );
        assert_eq!(editor.completing(), None);
    }

    #[test]
    fn an_escaped_dollar_is_never_completed() {
        let mut editor = PieceEditor::new(&[]);
        for c in "$$(cu".chars() {
            editor.insert_char(c);
        }
        assert_eq!(editor.completing(), None);
        let mut editor = PieceEditor::new(&[]);
        for c in "$(a b".chars() {
            editor.insert_char(c);
        }
        assert_eq!(editor.completing(), None, "a space ends the name");
    }

    #[test]
    fn which_references_fit_follows_the_resolver() {
        let int = FieldKind::Number { min: None, max: None };
        assert!(fits(&int, &option("a", "p", FieldKind::Number { min: Some(1), max: None }), false).is_ok());
        assert_eq!(
            fits(&int, &option("a", "h", FieldKind::Text), false),
            Err("Text; the field takes a number.".into())
        );
        assert!(fits(&FieldKind::Text, &option("a", "p", int.clone()), false).is_ok());
        assert!(fits(&FieldKind::Url, &option("a", "p", int.clone()), true).is_ok(), "text and URLs can assemble");
        assert!(fits(&int, &option("a", "p", int.clone()), true).is_err(), "a number can't assemble");
        let mut secret = option("a", "pw", FieldKind::Secret);
        secret.locked = Some(Locked { secret: true, granted: false });
        assert!(fits(&FieldKind::Text, &secret, false).is_err());
        assert!(fits(&FieldKind::Secret, &secret, false).is_ok());
    }

    #[test]
    fn search_is_fuzzy_and_ordered_by_fit_then_approval() {
        let int = FieldKind::Number { min: None, max: None };
        let mut secret = option("cuprated", "rpc_password", FieldKind::Secret);
        secret.locked = Some(Locked { secret: true, granted: false });
        let options = vec![
            option("cuprated", "rpc_host", FieldKind::Text),
            option("cuprated", "rpc_port", int.clone()),
            secret,
        ];
        assert_eq!(search(&options, "rpcp").len(), 2);
        let order: Vec<_> =
            ordered(&int, search(&options, ""), false).into_iter().map(|(o, v)| (o.setting.as_str(), v.is_ok())).collect();
        assert_eq!(order, [("rpc_port", true), ("rpc_host", false), ("rpc_password", false)]);
    }
}
