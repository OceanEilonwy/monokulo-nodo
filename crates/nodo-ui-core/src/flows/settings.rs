//! An app's settings: the form and its draft, what each field accepts, references to other apps'
//! settings, what saving does, and what a save that was rolled back says.
//!
//! [`Settings`] is the form. A field is named by its place in `form.fields` (the helper's order,
//! which never changes while the form is open); [`Settings::index_of`] finds it by key for a
//! frontend that names fields that way. Every change to the draft goes through an action here,
//! which also asks the helper what a value with references resolves to (`Effect::PreviewSetting`).
//!
//! [`Picker`] is the one list of what other apps offer to reference. Choosing from it may need
//! the owner's approval first ([`Picker::approve`]); the reference chosen is then put into the
//! field with [`Settings::place`].
//!
//! [`ApplyFailed`] is the page after a save the helper took back.

use super::{Confirm, Dest, Notice, Question, Step};
use crate::model::{
    refs, Field, FieldKind, FieldPreview, FieldValue, Piece, ReferenceList, ReferenceOption, RollbackExplanation, SettingsForm,
};
use crate::refs::PieceEditor;
use crate::secret::{Input, STORED_MASK};
use crate::{Effect, Outcome};

/// Opens an app's settings: the helper answers with its form, or with none. See [`apply`].
pub fn open(package: &str) -> Vec<Effect> {
    vec![Effect::OpenSettings(package.to_owned())]
}

/// What the helper answered to [`open`]: the form's page with what it loads first, or a remark
/// for an app that has nothing to set. A frontend whose page shows a finished install replaces
/// that page instead of going on from it.
pub fn apply(outcome: &Outcome) -> Step {
    match outcome {
        Outcome::SettingsLoaded { form: Some(form), .. } => {
            let effects = Settings::new(form.clone()).load();
            Step::All(vec![Step::Go(Dest::Settings(form.clone())), Step::run(effects)])
        }
        Outcome::SettingsLoaded { package, form: None } => {
            Step::Done(Notice::info(format!("{package} has no settings to configure.")))
        }
        // The owner left the picker before the helper answered: the approval stands, and is said.
        Outcome::Granted { provider, setting, result: Ok(()) } => {
            Step::Done(Notice::ok(format!("Approved: {provider}.{setting} can be used.")))
        }
        Outcome::Granted { result: Err(error), .. } => Step::Failed(Notice::error(format!("Not approved: {error}"))),
        _ => Step::Stay,
    }
}

/// Field validation, kept pure so that it can be tested and so save can refuse early.
pub fn validate(kind: &FieldKind, value: &FieldValue) -> Option<String> {
    // A value with references is checked by the helper's preview, against what it resolves to.
    let FieldValue::Text(text) = value else {
        return None;
    };
    match kind {
        FieldKind::Port => match text.parse::<u16>() {
            Ok(port) if port > 0 => None,
            _ => Some("Port must be a number from 1 to 65535.".to_owned()),
        },
        FieldKind::Number { min, max } => match text.parse::<i64>() {
            Ok(n) if min.is_none_or(|min| n >= min) && max.is_none_or(|max| n <= max) => None,
            Ok(_) => Some(format!(
                "Must be from {} to {}.",
                min.map_or("any".to_owned(), |n| n.to_string()),
                max.map_or("any".to_owned(), |n| n.to_string())
            )),
            Err(_) => Some("Must be a whole number.".to_owned()),
        },
        FieldKind::Url => (!(text.starts_with("http://") || text.starts_with("https://")))
            .then(|| "Must start with http:// or https://.".to_owned()),
        FieldKind::MoneroAddress => {
            const BASE58: &str = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
            match text.chars().count() {
                n if n < 95 => Some("Address is too short. A Monero address has 95 characters.".to_owned()),
                n if n > 95 => Some("Address is too long. A Monero address has 95 characters.".to_owned()),
                _ if !text.chars().all(|c| BASE58.contains(c)) => {
                    Some("Address contains characters a Monero address cannot have.".to_owned())
                }
                _ => None,
            }
        }
        FieldKind::Choice(_) | FieldKind::Secret | FieldKind::Toggle | FieldKind::Text => None,
    }
}

/// One step back in a field, offered while the field still holds what the step made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Undo {
    pub field: usize,
    /// The value before the step, and the value right after it: the step is offered only while
    /// the field still holds what it made, so it never undoes something else.
    pub before: FieldValue,
    pub after: FieldValue,
    pub label: String,
}

/// Where a picked reference goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    /// As the field's whole value. A frontend with a text cursor in the field puts the reference
    /// at that cursor instead, and gives the pieces to [`Settings::set_pieces`] when typing ends.
    Insert,
    /// In place of this reference of the field, counting from 0.
    Replace(usize),
    /// After the text and references the field holds, for a frontend without a text cursor.
    Append,
}

/// An app's settings form and the owner's unsaved changes to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub form: SettingsForm,
    /// One value per field of the form: what Save would store.
    pub draft: Vec<FieldValue>,
    /// What the other apps offer to reference, once loaded.
    pub catalog: Option<ReferenceList>,
    /// The latest preview of each field's draft, from the helper.
    pub previews: Vec<Option<FieldPreview>>,
    /// One step back; see [`Settings::undo_for`].
    pub undo: Option<Undo>,
    /// Save stopped to say what the changed references allow, a line each. It goes ahead with
    /// [`Settings::confirm_save`].
    pub confirm: Option<Vec<String>>,
}

impl Settings {
    pub fn new(form: SettingsForm) -> Self {
        let draft: Vec<FieldValue> = form.fields.iter().map(|field| field.value.clone()).collect();
        let previews = vec![None; draft.len()];
        Self { form, draft, catalog: None, previews, undo: None, confirm: None }
    }

    /// What the form loads when it opens: what can be referenced, and what each value that has
    /// references resolves to.
    pub fn load(&self) -> Vec<Effect> {
        let mut effects = vec![Effect::LoadReferences { package: self.form.package.clone() }];
        effects.extend((0..self.draft.len()).flat_map(|index| self.preview_effects(index)));
        effects
    }

    // What the form shows

    /// The field with this key.
    pub fn index_of(&self, key: &str) -> Option<usize> {
        self.form.fields.iter().position(|field| field.key == key)
    }

    /// The fields whose draft differs from what is stored.
    pub fn changed_indices(&self) -> Vec<usize> {
        (0..self.draft.len()).filter(|&i| self.draft[i] != self.form.fields[i].value).collect()
    }

    /// Why a field's draft cannot be saved, in words for beside the field.
    pub fn error_for(&self, index: usize) -> Option<String> {
        let field = &self.form.fields[index];
        let empty = self.draft[index] == FieldValue::Text(String::new());
        // An optional field left empty has no value, which is always allowed; only what is typed
        // has to fit the kind.
        if !field.required && empty {
            return None;
        }
        // A required one must have a value. Only a number's parse caught this before, so a fresh
        // install with an empty required name or choice saved, and the app could not start. An
        // unset secret is left to the helper: it may be one the helper makes when it saves.
        if empty {
            return Some("Required: give it a value.".to_owned());
        }
        validate(&field.kind, &self.draft[index])
    }

    /// The preview of a field, if it is for the value the field holds now.
    pub fn preview(&self, index: usize) -> Option<&FieldPreview> {
        self.previews.get(index)?.as_ref().filter(|p| p.for_value.as_ref() == Some(&self.draft[index]))
    }

    /// References that do not resolve, over every field, as far as the previews know.
    pub fn broken(&self) -> usize {
        (0..self.draft.len()).filter_map(|i| self.preview(i)).map(FieldPreview::broken).sum()
    }

    /// The first reference of a field that does not resolve, counting the field's references
    /// from 0, as far as its preview knows.
    pub fn first_broken(&self, index: usize) -> Option<usize> {
        self.preview(index)?.chips.iter().position(Result::is_err)
    }

    /// The field Save would stop at: the first that does not fit its kind, else the first with a
    /// broken reference.
    pub fn blocking_field(&self) -> Option<usize> {
        let count = self.draft.len();
        (0..count).find(|&i| self.error_for(i).is_some()).or_else(|| (0..count).find(|&i| self.first_broken(i).is_some()))
    }

    /// The undo step for a field, while the field still holds what that step made.
    pub fn undo_for(&self, index: usize) -> Option<&Undo> {
        self.undo.as_ref().filter(|u| u.field == index && self.draft[index] == u.after)
    }

    /// The references in a field's draft, in order, as (app, setting).
    pub fn references(&self, index: usize) -> Vec<(String, String)> {
        let pieces = self.draft[index].pieces().unwrap_or_default();
        refs(&pieces).into_iter().map(|(app, setting)| (app.to_owned(), setting.to_owned())).collect()
    }

    /// What `$(…` before the cursor of `editor`, typing in field `index`, suggests: up to five
    /// references that fit and need no approval, by app and name.
    pub fn suggestions(&self, index: usize, editor: &PieceEditor) -> Vec<(String, String)> {
        let Some(catalog) = &self.catalog else {
            return Vec::new();
        };
        let Some((_, query)) = editor.completing() else {
            return Vec::new();
        };
        let kind = &self.form.fields[index].kind;
        let assembled = editor.units.len() > query.chars().count() + 2;
        crate::refs::ordered(kind, crate::refs::search(&catalog.options, &query), assembled)
            .into_iter()
            .filter(|(option, verdict)| verdict.is_ok() && option.locked.is_none_or(|l| l.granted))
            .take(5)
            .map(|(option, _)| (option.app.clone(), option.setting.clone()))
            .collect()
    }

    // Changing a field

    /// Asks the helper what a field's draft resolves to, when it has references.
    fn preview_effects(&self, index: usize) -> Vec<Effect> {
        if !matches!(self.draft[index], FieldValue::Pieces(_)) {
            return Vec::new();
        }
        vec![Effect::PreviewSetting {
            package: self.form.package.clone(),
            field_index: index,
            key: self.form.fields[index].key.clone(),
            value: self.draft[index].clone(),
        }]
    }

    /// Puts a value into a field's draft. Nothing happens when the field already holds it.
    pub fn set_value(&mut self, index: usize, value: FieldValue) -> Vec<Effect> {
        if value == self.draft[index] {
            return Vec::new();
        }
        self.draft[index] = value;
        self.preview_effects(index)
    }

    /// Puts typed text and references into a field's draft: plain text when no reference is left.
    pub fn set_pieces(&mut self, index: usize, pieces: Vec<Piece>) -> Vec<Effect> {
        self.set_value(index, FieldValue::from_pieces(pieces))
    }

    /// Takes a newly typed secret out of the box into the field's draft. Leaving the box empty
    /// keeps the stored secret instead of erasing it.
    pub fn set_secret(&mut self, index: usize, typed: &mut Input) {
        if !typed.is_empty() {
            self.draft[index] = FieldValue::NewSecret(typed.take_secret());
        }
    }

    /// Flips a switch. Nothing for any other kind of field.
    pub fn toggle(&mut self, index: usize) {
        if let (FieldKind::Toggle, FieldValue::Bool(value)) = (&self.form.fields[index].kind, &self.draft[index]) {
            self.draft[index] = FieldValue::Bool(!*value);
        }
    }

    /// Steps a choice to the next option, or the one before, going round at the ends. Nothing for
    /// any other kind of field.
    pub fn step_choice(&mut self, index: usize, forward: bool) {
        let (FieldKind::Choice(options), FieldValue::Text(current)) = (&self.form.fields[index].kind, &self.draft[index]) else {
            return;
        };
        if options.is_empty() {
            return;
        }
        // An optional choice with no default has no value yet: the first step lands on the first
        // choice (or, backwards, the last), not one past it.
        let next = match options.iter().position(|option| option == current) {
            Some(position) if forward => (position + 1) % options.len(),
            Some(position) => (position + options.len() - 1) % options.len(),
            None if forward => 0,
            None => options.len() - 1,
        };
        self.draft[index] = FieldValue::Text(options[next].clone());
    }

    /// Puts a field back to what is stored.
    pub fn reset(&mut self, index: usize) -> Vec<Effect> {
        self.draft[index] = self.form.fields[index].value.clone();
        self.preview_effects(index)
    }

    /// Takes back the last step in a field, if that is the field the step was in and it still
    /// holds what the step made. See [`Settings::undo_for`].
    pub fn undo(&mut self, index: usize) -> Vec<Effect> {
        if self.undo.as_ref().is_none_or(|undo| undo.field != index) {
            return Vec::new();
        }
        let Some(undo) = self.undo.take().filter(|u| self.draft[u.field] == u.after) else {
            return Vec::new();
        };
        self.draft[undo.field] = undo.before;
        self.preview_effects(undo.field)
    }

    // References

    /// Opens the picker for a field. `beside_typed` says the owner is typing in the field and
    /// has typed something, so the reference would sit beside it. Refused, with the reason, for a
    /// field that takes no reference. The effects load what can be referenced, when that is not
    /// known yet; hand their outcome to both the form and the picker.
    pub fn pick(&self, index: usize, mode: PickMode, beside_typed: bool) -> Result<(Picker, Vec<Effect>), String> {
        let field = &self.form.fields[index];
        if !field.kind.takes_reference() {
            return Err("This setting cannot take a value from another app.".to_owned());
        }
        let assembled = match mode {
            PickMode::Insert => beside_typed,
            PickMode::Replace(_) => self.draft[index].pieces().is_some_and(|p| p.len() > 1),
            PickMode::Append => true,
        };
        let picker = Picker {
            package: self.form.package.clone(),
            field_index: index,
            field_label: field.label.clone(),
            field_kind: field.kind.clone(),
            mode,
            assembled,
            list: self.catalog.clone().unwrap_or_default(),
            granting: None,
        };
        let load = if self.catalog.is_none() {
            vec![Effect::LoadReferences { package: self.form.package.clone() }]
        } else {
            Vec::new()
        };
        Ok((picker, load))
    }

    /// Puts a picked reference into a field: as its whole value, or in place of one of its
    /// references. The step can be taken back.
    pub fn place(&mut self, index: usize, mode: PickMode, app: &str, setting: &str) -> Vec<Effect> {
        let before = self.draft[index].clone();
        match mode {
            PickMode::Insert => self.draft[index] = FieldValue::reference(app, setting),
            PickMode::Replace(chip) => {
                let mut pieces = before.pieces().unwrap_or_default();
                if let Some((position, ..)) = nth_ref(&pieces, chip) {
                    pieces[position] = Piece::Ref { app: app.to_owned(), setting: setting.to_owned() };
                }
                self.draft[index] = FieldValue::from_pieces(pieces);
            }
            PickMode::Append => return self.insert_reference(index, usize::MAX, app, setting),
        }
        self.placed(index, before)
    }

    /// Puts a reference among a field's text and references, before piece `at` (the number of
    /// pieces puts it last), for a frontend without a text cursor. The step can be taken back.
    pub fn insert_reference(&mut self, index: usize, at: usize, app: &str, setting: &str) -> Vec<Effect> {
        let before = self.draft[index].clone();
        let Some(mut pieces) = before.pieces() else {
            return Vec::new();
        };
        pieces.insert(at.min(pieces.len()), Piece::Ref { app: app.to_owned(), setting: setting.to_owned() });
        self.draft[index] = FieldValue::from_pieces(pieces);
        self.placed(index, before)
    }

    /// Notes how to take a placed reference back, and asks what the field now resolves to.
    fn placed(&mut self, index: usize, before: FieldValue) -> Vec<Effect> {
        let label = match &before {
            FieldValue::Text(text) if !text.is_empty() => format!("Undo: back to {text}"),
            FieldValue::Pieces(_) => "Undo".to_owned(),
            _ => String::new(),
        };
        if !label.is_empty() && before != self.draft[index] {
            let after = self.draft[index].clone();
            self.undo = Some(Undo { field: index, before, after, label });
        }
        self.preview_effects(index)
    }

    /// Turns one reference of a field into the text it resolves to now, ready to be edited. The
    /// step can be taken back. Nothing when the field has no such reference.
    pub fn type_instead(&mut self, index: usize, chip: usize) -> Vec<Effect> {
        let Some(mut pieces) = self.draft[index].pieces() else {
            return Vec::new();
        };
        let Some((position, app, setting)) = nth_ref(&pieces, chip) else {
            return Vec::new();
        };
        let before = self.draft[index].clone();
        let value = self.preview(index).and_then(|p| p.chips.get(chip).cloned()).and_then(Result::ok).flatten();
        pieces[position] = Piece::Text(value.unwrap_or_default());
        let after = FieldValue::from_pieces(pieces);
        self.draft[index] = after.clone();
        self.undo = Some(Undo { field: index, before, after, label: format!("Use {app}.{setting} again") });
        self.preview_effects(index)
    }

    /// Takes one reference out of a field. The step can be taken back. Nothing when the field
    /// has no such reference.
    pub fn remove_reference(&mut self, index: usize, chip: usize) -> Vec<Effect> {
        let Some(mut pieces) = self.draft[index].pieces() else {
            return Vec::new();
        };
        let Some((position, app, setting)) = nth_ref(&pieces, chip) else {
            return Vec::new();
        };
        let before = self.draft[index].clone();
        pieces.remove(position);
        let after = FieldValue::from_pieces(pieces);
        self.draft[index] = after.clone();
        self.undo = Some(Undo { field: index, before, after, label: format!("Put {app}.{setting} back") });
        self.preview_effects(index)
    }

    // Saving and leaving

    /// Save. Refused, with nothing changed, while there is nothing to save, a field does not fit
    /// its kind or a reference is broken ([`Settings::blocking_field`] names the field). When the
    /// changed references allow something new, `confirm` is set to say what and nothing is sent
    /// until [`Settings::confirm_save`]. Otherwise the changes go to the helper.
    pub fn save(&mut self) -> Step {
        if self.changed_indices().is_empty() {
            return Step::Done(Notice::info("There are no changes to save."));
        }
        if (0..self.draft.len()).any(|index| self.error_for(index).is_some()) {
            return Step::Failed(Notice::error("Fix the marked field before saving. Nothing was changed."));
        }
        let broken = self.broken();
        if broken > 0 {
            let plural = if broken == 1 { "reference" } else { "references" };
            return Step::Failed(Notice::error(format!("Fix {broken} broken {plural} to save. Nothing was changed.")));
        }
        let bullets = self.save_consequences();
        if bullets.is_empty() {
            return Step::Run(self.apply_draft());
        }
        self.confirm = Some(bullets);
        Step::Stay
    }

    /// The owner read what the references allow and saves.
    pub fn confirm_save(&mut self) -> Vec<Effect> {
        self.confirm = None;
        self.apply_draft()
    }

    /// The owner read what the references allow and does not save. The draft stays.
    pub fn cancel_save(&mut self) {
        self.confirm = None;
    }

    /// What the changed references allow, in plain words, for the save confirmation.
    pub fn save_consequences(&self) -> Vec<String> {
        let package = &self.form.package;
        let mut access = Vec::new();
        let mut dependencies = Vec::new();
        let mut follows = Vec::new();
        for index in self.changed_indices() {
            let Some(pieces) = self.draft[index].pieces() else { continue };
            let was = self.form.fields[index].value.pieces().unwrap_or_default();
            for (app_id, setting) in refs(&pieces) {
                if !refs(&was).contains(&(app_id, setting)) {
                    let line = format!("{package} follows {app_id}'s {setting} when it changes.");
                    if !follows.contains(&line) {
                        follows.push(line);
                    }
                }
            }
            if let Some(preview) = self.preview(index) {
                for (app_id, what) in &preview.new_access {
                    let line = format!("{package} will be able to reach {app_id} ({what}).");
                    if !access.contains(&line) {
                        access.push(line);
                    }
                }
                for app_id in &preview.new_dependencies {
                    let line = format!("{app_id} becomes a dependency: it can't be removed while {package} uses it.");
                    if !dependencies.contains(&line) {
                        dependencies.push(line);
                    }
                }
            }
        }
        access.into_iter().chain(dependencies).chain(follows).collect()
    }

    /// The changed fields, as the helper is asked to store them. An emptied optional field is
    /// reset to having no value.
    fn apply_draft(&self) -> Vec<Effect> {
        let values = self
            .changed_indices()
            .into_iter()
            .map(|index| {
                let (field, value) = (&self.form.fields[index], &self.draft[index]);
                let reset = field.input_for(value) == Some(nodo_api::SettingInput::Reset);
                (field.key.clone(), (!reset).then(|| value.clone()))
            })
            .collect();
        vec![Effect::ApplySettings { package: self.form.package.clone(), values }]
    }

    /// Leaving the form: at once when nothing was changed, after a question otherwise.
    pub fn leave(&self) -> Step {
        if self.changed_indices().is_empty() { Step::Go(Dest::Back) } else { Step::Ask(ask_discard(&self.form.package)) }
    }

    /// Drops every unsaved change, and with it what was loaded for them.
    pub fn discard(&mut self) {
        *self = Self::new(self.form.clone());
    }

    /// What a result means for the form: what can be referenced, a field's preview, a save that
    /// went through (the draft becomes what is stored) or failed, and a save the helper took
    /// back, which leads to [`ApplyFailed`] with the draft kept. Results about another app's form
    /// change nothing.
    pub fn apply(&mut self, outcome: &Outcome) -> Step {
        match outcome {
            Outcome::ReferencesLoaded { package, list } if *package == self.form.package => {
                self.catalog = Some(list.clone());
                Step::Stay
            }
            Outcome::SettingPreviewed { package, field_index, preview }
                if *package == self.form.package && *field_index < self.previews.len() =>
            {
                self.previews[*field_index] = Some(preview.clone());
                Step::Stay
            }
            Outcome::SettingsApplied(Ok(())) => {
                for (field, value) in self.form.fields.iter_mut().zip(self.draft.iter_mut()) {
                    // The typed secret is dropped here. From now on it is only known to be set.
                    if matches!(value, FieldValue::NewSecret(_)) {
                        *value = FieldValue::SecretSet;
                    }
                    field.value = value.clone();
                }
                Step::Done(Notice::ok(format!("Saved. {} restarted and passed its health check.", self.form.package)))
            }
            Outcome::SettingsApplied(Err(error)) => Step::Failed(Notice::error(format!("Not saved: {error}"))),
            Outcome::SettingsRolledBack { package, explanation } => {
                let rows = self
                    .changed_indices()
                    .into_iter()
                    .map(|index| {
                        let field = &self.form.fields[index];
                        DraftRow {
                            label: field.label.clone(),
                            from: masked(&field.value),
                            to: masked(&self.draft[index]),
                            blamed: explanation.blamed_field.as_deref() == Some(field.key.as_str()),
                        }
                    })
                    .collect();
                Step::Go(Dest::ApplyFailed(ApplyFailed { package: package.clone(), explanation: explanation.clone(), rows }))
            }
            _ => Step::Stay,
        }
    }
}

/// The question before leaving a form with unsaved changes.
pub fn ask_discard(package: &str) -> Question {
    Question::careful(
        "Discard your changes?",
        vec!["Your unsaved settings will be lost.".to_owned()],
        "Discard",
        Confirm::DiscardSettings(package.to_owned()),
    )
}

/// Where the `n`th reference is among the pieces, with its app and setting.
fn nth_ref(pieces: &[Piece], n: usize) -> Option<(usize, String, String)> {
    pieces
        .iter()
        .enumerate()
        .filter_map(|(at, piece)| match piece {
            Piece::Ref { app, setting } => Some((at, app.clone(), setting.clone())),
            Piece::Text(_) => None,
        })
        .nth(n)
}

/// A value as a failed save's page shows it: short, and never a secret.
fn masked(value: &FieldValue) -> String {
    match value {
        FieldValue::Text(text) => {
            let count = text.chars().count();
            if count > 18 {
                format!("{}…", text.chars().take(14).collect::<String>())
            } else {
                text.clone()
            }
        }
        FieldValue::Pieces(pieces) => pieces
            .iter()
            .map(|piece| match piece {
                Piece::Text(text) => text.clone(),
                Piece::Ref { app, setting } => format!("{app}·{setting}"),
            })
            .collect(),
        FieldValue::Bool(true) => "on".to_owned(),
        FieldValue::Bool(false) => "off".to_owned(),
        FieldValue::SecretUnset => "not set".to_owned(),
        FieldValue::SecretSet => STORED_MASK.to_owned(),
        FieldValue::NewSecret(_) => "new secret".to_owned(),
    }
}

// The picker

/// The one reference picker, for every field: everything the other apps offer, searched by what
/// the owner types. The words typed are the frontend's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    /// The app whose field is being set.
    pub package: String,
    pub field_index: usize,
    pub field_label: String,
    pub field_kind: FieldKind,
    pub mode: PickMode,
    /// Whether the reference would sit beside other text or references.
    pub assembled: bool,
    pub list: ReferenceList,
    /// A locked setting the owner chose, waiting for their approval.
    pub granting: Option<ReferenceOption>,
}

/// What a choice in the picker, or a result there, came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    /// No reference to place: follow the step, which says why it does not fit or what failed, or
    /// is `Stay` while the owner's approval is asked (`Picker::granting`).
    Nothing(Step),
    /// This reference goes into the field: leave the picker, place it ([`Settings::place`] with
    /// the picker's field and mode), then follow `then`.
    Reference { app: String, setting: String, then: Step },
}

impl Picker {
    /// The options a search for `query` shows, in the order they are listed, each with whether
    /// it fits the field.
    pub fn visible(&self, query: &str) -> Vec<(&ReferenceOption, Result<(), String>)> {
        crate::refs::ordered(&self.field_kind, crate::refs::search(&self.list.options, query), self.assembled)
    }

    /// The owner chooses a setting of another app. One that does not fit the field is refused
    /// with the reason. One that is locked is asked about first (`granting`). Any other is the
    /// reference to place.
    pub fn choose(&mut self, app: &str, setting: &str) -> Picked {
        let Some(option) = self.list.options.iter().find(|o| o.app == app && o.setting == setting).cloned() else {
            return Picked::Nothing(Step::Stay);
        };
        if let Err(why) = crate::refs::fits(&self.field_kind, &option, self.assembled) {
            return Picked::Nothing(Step::Failed(Notice::error(why)));
        }
        if option.locked.is_some_and(|l| !l.granted) {
            self.granting = Some(option);
            return Picked::Nothing(Step::Stay);
        }
        Picked::Reference { app: option.app, setting: option.setting, then: Step::Stay }
    }

    /// The owner approves that this app reads the locked setting chosen. Nothing when none is
    /// being asked about.
    pub fn approve(&self) -> Vec<Effect> {
        self.granting
            .as_ref()
            .map(|option| Effect::Grant {
                consumer: self.package.clone(),
                provider: option.app.clone(),
                setting: option.setting.clone(),
            })
            .into_iter()
            .collect()
    }

    /// The owner does not approve. The picker stays.
    pub fn decline(&mut self) {
        self.granting = None;
    }

    /// What a result means for the picker: the list that loaded, and an approval, after which the
    /// approved reference is placed and the list read again.
    pub fn apply(&mut self, outcome: &Outcome) -> Picked {
        match outcome {
            Outcome::ReferencesLoaded { package, list } if *package == self.package => {
                self.list = list.clone();
                Picked::Nothing(Step::Stay)
            }
            Outcome::Granted { provider, setting, result: Ok(()) } => {
                self.granting = None;
                let package = self.package.clone();
                Picked::Reference {
                    app: provider.clone(),
                    setting: setting.clone(),
                    then: Step::All(vec![
                        Step::Done(Notice::ok(format!("Approved: {package} can use {provider}.{setting}."))),
                        Step::Run(vec![Effect::LoadReferences { package }]),
                    ]),
                }
            }
            Outcome::Granted { result: Err(error), .. } => {
                self.granting = None;
                Picked::Nothing(Step::Failed(Notice::error(format!("Not approved: {error}"))))
            }
            _ => Picked::Nothing(Step::Stay),
        }
    }
}

// A save that was rolled back

/// One changed field on the page of a save that was rolled back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRow {
    pub label: String,
    pub from: String,
    pub to: String,
    /// The helper names this field as the cause.
    pub blamed: bool,
}

/// A save the helper took back: why, and what the owner had changed. The form keeps its draft,
/// so the owner can go back and edit it, or drop it ([`Settings::discard`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyFailed {
    pub package: String,
    pub explanation: RollbackExplanation,
    pub rows: Vec<DraftRow>,
}

impl ApplyFailed {
    /// The field of the form the helper blames, for a frontend to bring into view when the owner
    /// goes back to edit.
    pub fn blamed_index(&self, form: &SettingsForm) -> Option<usize> {
        let blamed = self.explanation.blamed_field.as_deref()?;
        form.fields.iter().position(|field: &Field| field.key == blamed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::execute_effect;
    use crate::mock::{sample_address, MockBackend};
    use crate::model::ScreenData;

    const DAEMON_RPC: usize = 0;
    const API_PORT: usize = 1;
    const LOG_LEVEL: usize = 2;
    const SECRET: usize = 4;
    const ALLOW_LAN: usize = 5;

    /// The moneropay form, opened as a frontend opens it.
    fn opened(mock: &mut MockBackend) -> Settings {
        let outcome = execute_effect(mock, open("moneropay").remove(0)).remove(0);
        let Step::All(steps) = apply(&outcome) else { panic!("a form opens its page") };
        let [Step::Go(Dest::Settings(form)), Step::Run(effects)] = <[Step; 2]>::try_from(steps).expect("two steps") else {
            panic!("the page, then what it loads")
        };
        let mut settings = Settings::new(form);
        assert_eq!(effects, settings.load());
        run(mock, &mut settings, effects);
        settings
    }

    /// Performs effects and hands every outcome to the form, returning the steps that said
    /// something.
    fn run(mock: &mut MockBackend, settings: &mut Settings, effects: Vec<Effect>) -> Vec<Step> {
        effects
            .into_iter()
            .flat_map(|effect| execute_effect(mock, effect))
            .map(|outcome| settings.apply(&outcome))
            .filter(|step| *step != Step::Stay)
            .collect()
    }

    fn text(value: &str) -> FieldValue {
        FieldValue::Text(value.to_owned())
    }

    fn form_of(kind: FieldKind, required: bool, value: &str) -> Settings {
        Settings::new(SettingsForm {
            package: "shop".to_owned(),
            fields: vec![Field {
                key: "it".to_owned(),
                label: "It".to_owned(),
                help: String::new(),
                requirement: String::new(),
                required,
                kind,
                value: text(value),
            }],
            public_settings: Vec::new(),
        })
    }

    #[test]
    fn an_app_without_settings_says_so() {
        let mut mock = MockBackend::new();
        let outcome = execute_effect(&mut mock, open("tor").remove(0)).remove(0);
        assert_eq!(apply(&outcome), Step::Done(Notice::info("tor has no settings to configure.")));
    }

    #[test]
    fn opening_loads_what_can_be_referenced() {
        let mut mock = MockBackend::new();
        let settings = opened(&mut mock);
        assert!(settings.catalog.as_ref().is_some_and(|list| !list.options.is_empty()));
        assert!(settings.changed_indices().is_empty());
        assert_eq!(settings.index_of("api_port"), Some(API_PORT));
        assert_eq!(settings.leave(), Step::Go(Dest::Back));
    }

    #[test]
    fn each_kind_of_field_checks_what_is_typed() {
        let port = FieldKind::Port;
        assert_eq!(validate(&port, &text("5000")), None);
        for bad in ["0", "65536", "http"] {
            assert_eq!(validate(&port, &text(bad)).as_deref(), Some("Port must be a number from 1 to 65535."));
        }
        let number = FieldKind::Number { min: Some(1), max: None };
        assert_eq!(validate(&number, &text("7")), None);
        assert_eq!(validate(&number, &text("0")).as_deref(), Some("Must be from 1 to any."));
        assert_eq!(validate(&number, &text("seven")).as_deref(), Some("Must be a whole number."));
        assert_eq!(validate(&FieldKind::Url, &text("https://shop.example")), None);
        assert_eq!(validate(&FieldKind::Url, &text("shop.example")).as_deref(), Some("Must start with http:// or https://."));
        // A value with a reference is the helper's to check, against what it resolves to.
        assert_eq!(validate(&FieldKind::Url, &FieldValue::reference("cuprated", "rpc_url")), None);
        for free in [FieldKind::Text, FieldKind::Secret, FieldKind::Toggle, FieldKind::Choice(vec!["a".to_owned()])] {
            assert_eq!(validate(&free, &text("anything")), None);
        }
    }

    #[test]
    fn address_validation_matches_the_design_copy() {
        let short = text("48edf");
        assert_eq!(
            validate(&FieldKind::MoneroAddress, &short).as_deref(),
            Some("Address is too short. A Monero address has 95 characters.")
        );
        assert_eq!(validate(&FieldKind::MoneroAddress, &text(&sample_address())), None);
        let long = format!("{}4", sample_address());
        assert_eq!(
            validate(&FieldKind::MoneroAddress, &text(&long)).as_deref(),
            Some("Address is too long. A Monero address has 95 characters.")
        );
        let odd = sample_address().replacen(|c: char| c.is_ascii_alphanumeric(), "0", 1);
        assert_eq!(
            validate(&FieldKind::MoneroAddress, &text(&odd)).as_deref(),
            Some("Address contains characters a Monero address cannot have.")
        );
    }

    #[test]
    fn an_empty_field_is_fine_only_when_it_is_optional() {
        let optional = form_of(FieldKind::Number { min: Some(1), max: Some(65535) }, false, "");
        assert_eq!(optional.error_for(0), None);
        let required = form_of(FieldKind::Text, true, "");
        assert_eq!(required.error_for(0).as_deref(), Some("Required: give it a value."));
        assert_eq!(required.blocking_field(), Some(0));
    }

    #[test]
    fn an_emptied_optional_field_is_reset_and_a_required_one_is_refused() {
        let mut optional = form_of(FieldKind::Text, false, "smtp.example.com");
        assert!(optional.set_value(0, text("")).is_empty(), "plain text needs no preview");
        assert_eq!(
            optional.save(),
            Step::Run(vec![Effect::ApplySettings { package: "shop".to_owned(), values: vec![("it".to_owned(), None)] }])
        );
        let mut required = form_of(FieldKind::Url, true, "https://shop.example");
        assert!(required.set_value(0, text("")).is_empty());
        assert_eq!(required.save(), Step::Failed(Notice::error("Fix the marked field before saving. Nothing was changed.")));
        assert_eq!(required.blocking_field(), Some(0));
    }

    #[test]
    fn saving_nothing_only_says_so() {
        let mut mock = MockBackend::new();
        let mut settings = opened(&mut mock);
        assert_eq!(settings.save(), Step::Done(Notice::info("There are no changes to save.")));
    }

    #[test]
    fn switches_choices_and_secrets_change_the_draft() {
        let mut mock = MockBackend::new();
        let mut settings = opened(&mut mock);
        settings.toggle(ALLOW_LAN);
        assert_eq!(settings.draft[ALLOW_LAN], FieldValue::Bool(false));
        settings.step_choice(LOG_LEVEL, true);
        assert_eq!(settings.draft[LOG_LEVEL], text("debug"));
        settings.step_choice(LOG_LEVEL, true);
        assert_eq!(settings.draft[LOG_LEVEL], text("error"), "the choices go round");
        settings.step_choice(LOG_LEVEL, false);
        assert_eq!(settings.draft[LOG_LEVEL], text("debug"));
        settings.toggle(LOG_LEVEL);
        settings.step_choice(ALLOW_LAN, true);
        assert_eq!((&settings.draft[LOG_LEVEL], &settings.draft[ALLOW_LAN]), (&text("debug"), &FieldValue::Bool(false)));

        let mut empty = Input::secret();
        settings.set_secret(SECRET, &mut empty);
        assert_eq!(settings.draft[SECRET], FieldValue::SecretSet, "an empty box keeps the stored secret");
        let mut typed = Input::secret();
        "s3cret".chars().for_each(|c| typed.push(c));
        settings.set_secret(SECRET, &mut typed);
        assert!(matches!(&settings.draft[SECRET], FieldValue::NewSecret(secret) if secret.expose() == "s3cret"));
        assert!(typed.is_empty(), "the secret was taken out of the box");

        assert_eq!(settings.changed_indices(), [LOG_LEVEL, SECRET, ALLOW_LAN]);
        assert!(matches!(settings.leave(), Step::Ask(question) if question.confirm == Confirm::DiscardSettings(settings.form.package.clone()) && question.cancel_is_default));
        assert!(settings.reset(ALLOW_LAN).is_empty());
        assert_eq!(settings.changed_indices(), [LOG_LEVEL, SECRET]);

        let Step::Run(effects) = settings.save() else { panic!("nothing to confirm without references") };
        let said = run(&mut mock, &mut settings, effects);
        assert_eq!(said, [Step::Done(Notice::ok("Saved. moneropay restarted and passed its health check."))]);
        assert!(settings.changed_indices().is_empty(), "the draft is what is stored now");
        assert_eq!(settings.draft[SECRET], FieldValue::SecretSet, "the typed secret is dropped once saved");
        assert_eq!(mock.settings_form("moneropay").expect("installed").fields[LOG_LEVEL].value, text("debug"));
    }

    #[test]
    fn a_reference_fits_only_a_field_of_its_kind() {
        let mut mock = MockBackend::new();
        let settings = opened(&mut mock);
        assert_eq!(
            settings.pick(SECRET, PickMode::Insert, false).err().as_deref(),
            Some("This setting cannot take a value from another app.")
        );
        let (mut picker, load) = settings.pick(API_PORT, PickMode::Insert, false).expect("a port takes a reference");
        assert!(load.is_empty(), "the list is known already");
        let fitting: Vec<String> =
            picker.visible("").into_iter().filter(|(_, fit)| fit.is_ok()).map(|(option, _)| option.path()).collect();
        assert_eq!(fitting, ["cuprated.rpc_port", "wallet-rpc.port"]);
        assert_eq!(
            picker.choose("cuprated", "rpc_url"),
            Picked::Nothing(Step::Failed(Notice::error("An address; the field takes a number.")))
        );
        assert_eq!(
            picker.choose("cuprated", "rpc_password"),
            Picked::Nothing(Step::Failed(Notice::error("A secret can only go into a secret setting.")))
        );
        assert_eq!(picker.granting, None);
        assert_eq!(picker.choose("nobody", "nothing"), Picked::Nothing(Step::Stay));
        // Beside typed text only text and addresses take a reference.
        let (mut beside, _) = settings.pick(API_PORT, PickMode::Insert, true).expect("a port takes a reference");
        assert!(matches!(beside.choose("cuprated", "rpc_port"), Picked::Nothing(Step::Failed(_))));
    }

    #[test]
    fn a_form_that_does_not_know_the_references_loads_them_for_the_picker() {
        let mut mock = MockBackend::new();
        let mut settings = opened(&mut mock);
        settings.catalog = None;
        let (mut picker, load) = settings.pick(DAEMON_RPC, PickMode::Insert, false).expect("an address takes a reference");
        assert_eq!(load, [Effect::LoadReferences { package: "moneropay".to_owned() }]);
        assert!(picker.visible("").is_empty());
        let outcome = execute_effect(&mut mock, load[0].clone()).remove(0);
        assert_eq!(picker.apply(&outcome), Picked::Nothing(Step::Stay));
        assert_eq!(settings.apply(&outcome), Step::Stay);
        assert!(!picker.visible("rpc").is_empty() && settings.catalog.is_some());
    }

    #[test]
    fn a_placed_reference_is_previewed_can_be_undone_and_is_confirmed_before_saving() {
        let mut mock = MockBackend::new();
        let mut settings = opened(&mut mock);
        let (mut picker, _) = settings.pick(DAEMON_RPC, PickMode::Insert, false).expect("an address takes a reference");
        let Picked::Reference { app, setting, then: Step::Stay } = picker.choose("cuprated", "rpc_url") else {
            panic!("a public address fits")
        };
        let effects = settings.place(picker.field_index, picker.mode, &app, &setting);
        assert!(matches!(effects.as_slice(), [Effect::PreviewSetting { field_index: DAEMON_RPC, .. }]));
        assert!(run(&mut mock, &mut settings, effects).is_empty());
        assert_eq!(settings.draft[DAEMON_RPC], FieldValue::reference("cuprated", "rpc_url"));
        assert_eq!(settings.references(DAEMON_RPC), [("cuprated".to_owned(), "rpc_url".to_owned())]);
        let preview = settings.preview(DAEMON_RPC).expect("previewed");
        assert_eq!(preview.resolved.as_deref(), Some("http://127.0.0.1:18081"));
        assert_eq!(settings.undo_for(DAEMON_RPC).map(|u| u.label.as_str()), Some("Undo: back to http://127.0.0.1:18081"));
        assert_eq!(settings.undo_for(API_PORT), None);

        assert_eq!(settings.save(), Step::Stay, "what the reference allows is said first");
        assert_eq!(
            settings.confirm.clone().expect("asked"),
            [
                "moneropay will be able to reach cuprated (port 18081/tcp).",
                "cuprated becomes a dependency: it can't be removed while moneropay uses it.",
                "moneropay follows cuprated's rpc_url when it changes.",
            ]
        );
        settings.cancel_save();
        assert_eq!(settings.confirm, None);
        assert_eq!(settings.save(), Step::Stay);
        let effects = settings.confirm_save();
        assert_eq!(
            effects,
            [Effect::ApplySettings {
                package: "moneropay".to_owned(),
                values: vec![("daemon_rpc".to_owned(), Some(FieldValue::reference("cuprated", "rpc_url")))],
            }]
        );
        assert_eq!(settings.confirm, None);

        // Undo is only for the field the step was in, and only once.
        assert!(settings.undo(API_PORT).is_empty());
        assert!(settings.undo.is_some());
        assert!(settings.undo(DAEMON_RPC).is_empty(), "plain text needs no preview");
        assert_eq!(settings.draft[DAEMON_RPC], text("http://127.0.0.1:18081"));
        assert_eq!(settings.undo, None);
    }

    #[test]
    fn a_locked_setting_is_approved_before_it_is_placed() {
        let mut mock = MockBackend::new();
        let mut form = mock.settings_form("moneropay").expect("installed");
        form.fields[SECRET].kind = FieldKind::Text;
        form.fields[SECRET].value = text("");
        // A plain setting that is not public, such as another app's internal port.
        mock.references.push(nodo_api::ReferenceOption {
            package: "cuprated".to_owned(),
            reference: "zmq_port".to_owned(),
            label: "ZMQ port".to_owned(),
            help: String::new(),
            kind: nodo_api::SettingKind::Int { min: None, max: None },
            access: nodo_api::ReferenceAccess::NeedsGrant { secret: false, granted: false },
        });
        let mut settings = Settings::new(form);
        let effects = settings.load();
        run(&mut mock, &mut settings, effects);
        let (mut picker, _) = settings.pick(SECRET, PickMode::Insert, false).expect("text takes a reference");
        assert!(picker.approve().is_empty(), "nothing is being asked yet");
        assert_eq!(picker.choose("cuprated", "zmq_port"), Picked::Nothing(Step::Stay));
        assert_eq!(picker.granting.as_ref().map(ReferenceOption::path).as_deref(), Some("cuprated.zmq_port"));
        assert_eq!(settings.draft[SECRET], text(""), "nothing is placed before the approval");
        picker.decline();
        assert_eq!(picker.granting, None);

        assert_eq!(picker.choose("cuprated", "zmq_port"), Picked::Nothing(Step::Stay));
        let grant = picker.approve();
        assert_eq!(
            grant,
            [Effect::Grant { consumer: "moneropay".to_owned(), provider: "cuprated".to_owned(), setting: "zmq_port".to_owned() }]
        );
        let outcome = execute_effect(&mut mock, grant[0].clone()).remove(0);
        let Picked::Reference { app, setting, then } = picker.apply(&outcome) else { panic!("approved, so placed") };
        assert_eq!(picker.granting, None);
        assert_eq!(
            then,
            Step::All(vec![
                Step::Done(Notice::ok("Approved: moneropay can use cuprated.zmq_port.")),
                Step::Run(vec![Effect::LoadReferences { package: "moneropay".to_owned() }]),
            ])
        );
        let effects = settings.place(picker.field_index, picker.mode, &app, &setting);
        run(&mut mock, &mut settings, effects);
        assert_eq!(settings.draft[SECRET], FieldValue::reference("cuprated", "zmq_port"));
        assert_eq!(settings.broken(), 0, "the approved reference resolves");
        assert_eq!(settings.undo_for(SECRET), None, "there was nothing in the field to go back to");
        // The list read again knows the approval, so it is not asked twice.
        for outcome in execute_effect(&mut mock, then.effects().remove(0)) {
            assert_eq!(picker.apply(&outcome), Picked::Nothing(Step::Stay));
        }
        assert!(matches!(picker.choose("cuprated", "zmq_port"), Picked::Reference { .. }));
    }

    #[test]
    fn a_refused_approval_places_nothing() {
        let mut mock = MockBackend::new();
        let settings = opened(&mut mock);
        let (mut picker, _) = settings.pick(DAEMON_RPC, PickMode::Insert, false).expect("an address takes a reference");
        picker.granting = picker.list.options.iter().find(|o| o.setting == "rpc_password").cloned();
        mock.references.retain(|o| o.reference != "rpc_password");
        let outcome = execute_effect(&mut mock, picker.approve().remove(0)).remove(0);
        let Picked::Nothing(Step::Failed(notice)) = picker.apply(&outcome) else { panic!("refused") };
        assert!(notice.text.starts_with("Not approved: "), "{}", notice.text);
        assert_eq!(picker.granting, None);
        // The same results, away from the picker, are only said.
        assert_eq!(apply(&outcome), Step::Failed(notice));
    }

    #[test]
    fn a_broken_reference_blocks_saving_until_it_is_fixed() {
        let mut mock = MockBackend::new();
        let mut settings = opened(&mut mock);
        let pieces = vec![
            Piece::Text("http://".to_owned()),
            Piece::Ref { app: "cuprated".to_owned(), setting: "rpc_host".to_owned() },
            Piece::Text(":".to_owned()),
            Piece::Ref { app: "nobody".to_owned(), setting: "port".to_owned() },
        ];
        let effects = settings.set_pieces(DAEMON_RPC, pieces);
        run(&mut mock, &mut settings, effects);
        assert_eq!((settings.broken(), settings.first_broken(DAEMON_RPC)), (1, Some(1)));
        assert_eq!(settings.blocking_field(), Some(DAEMON_RPC));
        assert_eq!(settings.save(), Step::Failed(Notice::error("Fix 1 broken reference to save. Nothing was changed.")));
        assert_eq!(settings.confirm, None);

        // Replacing the broken one with one that resolves.
        let (mut picker, _) = settings.pick(DAEMON_RPC, PickMode::Replace(1), false).expect("an address takes a reference");
        assert!(picker.assembled, "it sits beside text and another reference");
        let Picked::Reference { app, setting, .. } = picker.choose("cuprated", "rpc_port") else { panic!("an address mixes") };
        let effects = settings.place(DAEMON_RPC, picker.mode, &app, &setting);
        run(&mut mock, &mut settings, effects);
        assert_eq!(settings.broken(), 0);
        assert_eq!(settings.preview(DAEMON_RPC).and_then(|p| p.resolved.clone()).as_deref(), Some("http://127.0.0.1:18081"));
        assert_eq!(settings.undo_for(DAEMON_RPC).map(|u| u.label.as_str()), Some("Undo"));

        // Typing a value instead of the first reference, then taking the second out.
        let effects = settings.type_instead(DAEMON_RPC, 0);
        run(&mut mock, &mut settings, effects);
        assert_eq!(settings.references(DAEMON_RPC), [("cuprated".to_owned(), "rpc_port".to_owned())]);
        assert_eq!(settings.undo_for(DAEMON_RPC).map(|u| u.label.as_str()), Some("Use cuprated.rpc_host again"));
        assert!(matches!(settings.draft[DAEMON_RPC].pieces().as_deref(), Some([Piece::Text(text), _]) if text == "http://127.0.0.1:"));
        assert!(settings.remove_reference(DAEMON_RPC, 0).is_empty(), "plain text needs no preview");
        assert_eq!(settings.draft[DAEMON_RPC], text("http://127.0.0.1:"));
        assert_eq!(settings.undo_for(DAEMON_RPC).map(|u| u.label.as_str()), Some("Put cuprated.rpc_port back"));
        assert!(settings.remove_reference(DAEMON_RPC, 0).is_empty() && settings.type_instead(DAEMON_RPC, 3).is_empty());

        // Without a cursor: a reference goes among the pieces by place.
        let effects = settings.insert_reference(DAEMON_RPC, 9, "cuprated", "rpc_port");
        run(&mut mock, &mut settings, effects);
        assert_eq!(settings.preview(DAEMON_RPC).and_then(|p| p.resolved.clone()).as_deref(), Some("http://127.0.0.1:18081"));
        assert!(settings.insert_reference(ALLOW_LAN, 0, "cuprated", "rpc_port").is_empty(), "a switch has no pieces");
    }

    #[test]
    fn suggestions_offer_what_fits_and_needs_no_approval() {
        let mut mock = MockBackend::new();
        let settings = opened(&mut mock);
        let mut editor = PieceEditor::new(&[]);
        "$(rpc".chars().for_each(|c| editor.insert_char(c));
        let offered = settings.suggestions(DAEMON_RPC, &editor);
        assert_eq!(offered.first(), Some(&("cuprated".to_owned(), "rpc_url".to_owned())));
        assert!(!offered.iter().any(|(_, setting)| setting == "rpc_password"), "a locked setting is never suggested");
        assert_eq!(
            settings.suggestions(API_PORT, &editor),
            [("cuprated".to_owned(), "rpc_port".to_owned()), ("wallet-rpc".to_owned(), "port".to_owned())],
            "only numbers fit a port"
        );
        assert!(settings.suggestions(DAEMON_RPC, &PieceEditor::new(&[Piece::Text("rpc".to_owned())])).is_empty());
    }

    #[test]
    fn a_save_that_is_rolled_back_keeps_the_draft_and_says_what_was_changed() {
        let mut mock = MockBackend::new();
        let mut settings = opened(&mut mock);
        assert!(settings.set_value(API_PORT, text("18081")).is_empty());
        settings.step_choice(LOG_LEVEL, true);
        let mut typed = Input::secret();
        "s3cret".chars().for_each(|c| typed.push(c));
        settings.set_secret(SECRET, &mut typed);
        let Step::Run(effects) = settings.save() else { panic!("nothing to confirm") };
        let said = run(&mut mock, &mut settings, effects);
        let [Step::Go(Dest::ApplyFailed(failed))] = <[Step; 1]>::try_from(said).expect("one step") else { panic!("rolled back") };
        assert_eq!(failed.package, "moneropay");
        let rows: Vec<(&str, &str, &str)> =
            failed.rows.iter().map(|row| (row.label.as_str(), row.from.as_str(), row.to.as_str())).collect();
        assert_eq!(
            rows,
            [("API port", "5000", "18081"), ("Log level", "info", "debug"), ("Webhook secret", STORED_MASK, "new secret")]
        );
        assert_eq!(failed.blamed_index(&settings.form), None);
        assert_eq!(settings.changed_indices(), [API_PORT, LOG_LEVEL, SECRET], "the draft is kept for editing");

        let mut blamed = failed.clone();
        blamed.explanation.blamed_field = Some("api_port".to_owned());
        assert_eq!(blamed.blamed_index(&settings.form), Some(API_PORT));

        settings.discard();
        assert!(settings.changed_indices().is_empty());
    }

    #[test]
    fn a_save_the_helper_refuses_is_said_and_changes_nothing() {
        let mut mock = MockBackend::new();
        let mut settings = opened(&mut mock);
        settings.toggle(ALLOW_LAN);
        let refused = Outcome::SettingsApplied(Err(nodo_api::ApiError::Failed("the drive is full".to_owned())));
        let Step::Failed(notice) = settings.apply(&refused) else { panic!("refused") };
        assert!(notice.text.starts_with("Not saved: ") && notice.text.contains("the drive is full"), "{}", notice.text);
        assert_eq!(settings.changed_indices(), [ALLOW_LAN]);
    }

    #[test]
    fn results_about_another_app_change_nothing() {
        let mut mock = MockBackend::new();
        let mut settings = opened(&mut mock);
        let before = settings.clone();
        let other = Outcome::ReferencesLoaded { package: "wallet-relay".to_owned(), list: ReferenceList::default() };
        assert_eq!(settings.apply(&other), Step::Stay);
        let preview = Outcome::SettingPreviewed { package: "wallet-relay".to_owned(), field_index: 0, preview: FieldPreview::default() };
        assert_eq!(settings.apply(&preview), Step::Stay);
        assert_eq!(settings.apply(&Outcome::Info("hello".to_owned())), Step::Stay);
        assert_eq!(settings, before);
        assert_eq!(apply(&Outcome::Info("hello".to_owned())), Step::Stay);
    }
}
