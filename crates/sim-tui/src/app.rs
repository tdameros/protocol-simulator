//! What the terminal front end is showing, and what a key does to it.
//!
//! Kept apart from the drawing so that a key press can be tested without a
//! terminal, the same way the panels are tested without a window.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use sim_core::frame::{codec, FieldDef, FieldKind, FrameDef};
use sim_core::scenario::{Action, Expect, Scenario, Step};
use sim_core::{ConnectionStatus, RetryPolicy};

use crate::connection_form::ConnectionForm;
use sim_session::engine_handle::EngineHandle;
use sim_session::hex;
use sim_session::kinds;
use sim_session::project::Project;
use sim_session::reading::{self, Reading};
use sim_session::scenarios::{self, ActionKind};
use sim_session::state::{ConnectionEntry, LogEntry, MonitorState, Session, TrafficFilter};

/// The same five views the window has, in the same order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Connections,
    Traffic,
    HexInject,
    Frames,
    Scenarios,
}

impl Tab {
    pub const ALL: [Self; 5] = [
        Self::Connections,
        Self::Traffic,
        Self::HexInject,
        Self::Frames,
        Self::Scenarios,
    ];

    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Connections => "Connections",
            Self::Traffic => "Traffic",
            Self::HexInject => "Hex",
            Self::Frames => "Frames",
            Self::Scenarios => "Scenarios",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    fn at(index: usize) -> Self {
        Self::ALL[index % Self::ALL.len()]
    }
}

/// Moves a cursor by `delta`, clamped to a list of `len` rows, starting from
/// row zero when nothing is chosen yet. `None` when there is nothing to put
/// the cursor on.
///
/// Written once because every list in this front end -- connections, frames,
/// fields, scenarios -- moves its cursor the same way, and a rule that
/// changes (wrapping at the ends, say) should not have to be found and
/// changed four times.
fn moved(current: Option<usize>, delta: isize, len: usize) -> Option<usize> {
    let last = len.checked_sub(1)?;
    Some(current.unwrap_or(0).saturating_add_signed(delta).min(last))
}

/// Which pane of the Frames view a key acts on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FramesFocus {
    Library,
    Fields,
}

/// Which pane of the Traffic view a key acts on, the same split as
/// `FramesFocus` and for the same reason: up/down means one thing in the row
/// list and another once it has moved into the decoded fields underneath it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TrafficFocus {
    Rows,
    Fields,
}

/// One line of a frame's detail: a field on its own, or one flag inside a
/// bitfield.
///
/// Flattened so navigation is one dimension: a bitfield's flags are things to
/// move onto and edit exactly as a plain field is.
#[derive(Clone, Copy)]
pub enum FieldRow {
    Field(usize),
    Bit { field: usize, bit: usize },
}

/// Every row a frame's detail draws, in the order it draws them.
pub fn field_rows(frame: &sim_core::frame::FrameDef) -> Vec<FieldRow> {
    let mut rows = Vec::new();
    for (index, field) in frame.fields.iter().enumerate() {
        rows.push(FieldRow::Field(index));
        if let sim_core::frame::FieldKind::Bits { bits, .. } = &field.kind {
            rows.extend((0..bits.len()).map(|bit| FieldRow::Bit { field: index, bit }));
        }
    }
    rows
}

/// What is laid over the view, taking the keys the view would otherwise get.
/// What choosing an answer in [`Overlay::Pick`] does with it.
pub enum PickPurpose {
    /// Settles which definition a captured row is read through.
    DecodeAs,
    /// Settles the value of one enum field, by variant name.
    EnumField { field: usize },
    /// Settles which connection a frame is sent on.
    FrameTarget,
    /// Settles which connection hand-typed hex bytes are sent on.
    HexTarget,
}

/// One value typed as text, and what it belongs to.
pub struct EditBox {
    pub title: String,
    pub text: String,
    target: EditTarget,
}

#[derive(Clone)]
enum EditTarget {
    Field(usize),
    Bit {
        field: usize,
        bit: usize,
    },
    ScenarioName,
    ScenarioDescription,
    RepeatEvery,
    RepeatTimes,
    /// The folder to save into, chosen; the name is what is typed here.
    SaveFileName(PathBuf),
    FrameName,
    FrameFieldName(usize),
    FrameFieldLength(usize),
    /// `NAME = VALUE`, typed as one line.
    FrameVariant {
        field: usize,
        variant: usize,
    },
    /// `NAME WIDTH`, typed as one line.
    FrameBit {
        field: usize,
        bit: usize,
    },
}

pub enum Overlay {
    /// The key map.
    Keys,
    /// One answer to be chosen from a list.
    Pick(Picker, PickPurpose),
    /// One value typed as text: a number, some bytes, a run of characters.
    EditText(EditBox),
    /// A traffic view's name and filter, changed live as each field is
    /// touched: there is nothing to submit.
    Filter(FilterEdit),
    /// One scenario step, opened for its own editor.
    Step(StepEdit),
    /// One frame field, opened for its own editor.
    FrameField(FrameFieldEdit),
    /// A file to be found on this machine.
    Browse(Browser),
    /// A connection being described before it exists.
    NewConnection(ConnectionForm),
}

/// Walking the disk to reach a file.
///
/// The window opens a desktop file dialog. A board reached over ssh has no
/// desktop to put one on, so the walk is here: a folder at a time, with the
/// same keys as every other list.
/// One field of the filter editor: what it changes, and how a key changes it.
enum FilterField {
    Title,
    /// One row per connection the project knows, toggled on or off the
    /// filter's set. Empty means every connection, so this is the only field
    /// whose absence still means something.
    Connection(usize),
    Direction,
    Hex,
    Anchored,
    Offset,
    Source,
    Text,
    MinLen,
    MaxLen,
    Invert,
}

/// A traffic view's name and filter, edited live.
///
/// Unlike the connection form, there is nothing to validate and nothing to
/// submit: every field the window offers here applies to the buffer the
/// moment it changes, so the terminal does the same.
pub struct FilterEdit {
    focus: usize,
    /// Typed text for the three numeric fields, kept apart from the model
    /// because a person mid-keystroke on "12" is not yet the number 12 or
    /// the number 1.
    min_len_text: String,
    max_len_text: String,
    offset_text: String,
}

impl FilterEdit {
    fn new(filter: &TrafficFilter) -> Self {
        Self {
            focus: 0,
            min_len_text: filter.min_len.map_or_else(String::new, |n| n.to_string()),
            max_len_text: filter.max_len.map_or_else(String::new, |n| n.to_string()),
            offset_text: match filter.anchor {
                sim_session::state::HexAnchor::At(offset) => offset.to_string(),
                sim_session::state::HexAnchor::Anywhere => String::new(),
            },
        }
    }

    fn fields(names: &[String], filter: &TrafficFilter) -> Vec<FilterField> {
        let mut fields = vec![FilterField::Title];
        fields.extend((0..names.len()).map(FilterField::Connection));
        fields.push(FilterField::Direction);
        fields.push(FilterField::Hex);
        fields.push(FilterField::Anchored);
        if matches!(filter.anchor, sim_session::state::HexAnchor::At(_)) {
            fields.push(FilterField::Offset);
        }
        fields.push(FilterField::Source);
        fields.push(FilterField::Text);
        fields.push(FilterField::MinLen);
        fields.push(FilterField::MaxLen);
        fields.push(FilterField::Invert);
        fields
    }

    /// Each field as a line: its label, what it holds, and whether it is the
    /// one a key would change.
    #[must_use]
    pub fn lines(
        &self,
        title: &str,
        filter: &TrafficFilter,
        names: &[String],
    ) -> Vec<(String, String, bool)> {
        let fields = Self::fields(names, filter);
        let focus = self.focus.min(fields.len().saturating_sub(1));
        fields
            .iter()
            .enumerate()
            .map(|(at, field)| {
                let label = match field {
                    FilterField::Title => "Tab name".to_owned(),
                    FilterField::Connection(i) => format!("  {}", names[*i]),
                    FilterField::Direction => "Direction".to_owned(),
                    FilterField::Hex => "Hex pattern".to_owned(),
                    FilterField::Anchored => "At offset".to_owned(),
                    FilterField::Offset => "  Offset".to_owned(),
                    FilterField::Source => "Source contains".to_owned(),
                    FilterField::Text => "Text contains".to_owned(),
                    FilterField::MinLen => "Min length".to_owned(),
                    FilterField::MaxLen => "Max length".to_owned(),
                    FilterField::Invert => "Hide matches".to_owned(),
                };
                let value = match field {
                    FilterField::Title => title.to_owned(),
                    FilterField::Connection(i) => {
                        yes_no(filter.connections.contains(&names[*i])).to_owned()
                    }
                    FilterField::Direction => filter.direction.label().to_owned(),
                    FilterField::Hex => filter.hex.clone(),
                    FilterField::Anchored => yes_no(matches!(
                        filter.anchor,
                        sim_session::state::HexAnchor::At(_)
                    ))
                    .to_owned(),
                    FilterField::Offset => self.offset_text.clone(),
                    FilterField::Source => filter.source.clone(),
                    FilterField::Text => filter.text.clone(),
                    FilterField::MinLen => self.min_len_text.clone(),
                    FilterField::MaxLen => self.max_len_text.clone(),
                    FilterField::Invert => yes_no(filter.invert).to_owned(),
                };
                (label, value, at == focus)
            })
            .collect()
    }

    /// What a key does to the field under focus. Moving between fields never
    /// touches the model; everything else does, straight away.
    fn handle(
        &mut self,
        code: KeyCode,
        names: &[String],
        title: &mut String,
        filter: &mut TrafficFilter,
        close: &mut bool,
    ) {
        let held = Self::fields(names, filter).len().max(1);
        match code {
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % held,
            KeyCode::BackTab | KeyCode::Up => self.focus = (self.focus + held - 1) % held,
            KeyCode::Esc | KeyCode::Enter => *close = true,
            _ => {
                let fields = Self::fields(names, filter);
                let focus = self.focus.min(fields.len().saturating_sub(1));
                if let Some(field) = fields.into_iter().nth(focus) {
                    self.act(&field, code, names, title, filter);
                }
            }
        }
    }

    fn act(
        &mut self,
        field: &FilterField,
        code: KeyCode,
        names: &[String],
        title: &mut String,
        filter: &mut TrafficFilter,
    ) {
        match field {
            FilterField::Title => edit_text(code, title),
            FilterField::Connection(i) => {
                if matches!(code, KeyCode::Char(' ')) {
                    let name = &names[*i];
                    if filter.connections.contains(name) {
                        filter.connections.remove(name);
                    } else {
                        filter.connections.insert(name.clone());
                    }
                }
            }
            FilterField::Direction => {
                if let Some(delta) = arrow_delta(code) {
                    filter.direction = crate::connection_form::cycle(
                        &sim_session::state::DirectionFilter::ALL,
                        filter.direction,
                        delta,
                    );
                }
            }
            FilterField::Hex => edit_text(code, &mut filter.hex),
            FilterField::Anchored => {
                if matches!(code, KeyCode::Char(' ')) {
                    filter.anchor = if matches!(filter.anchor, sim_session::state::HexAnchor::At(_))
                    {
                        sim_session::state::HexAnchor::Anywhere
                    } else {
                        sim_session::state::HexAnchor::At(self.offset_text.parse().unwrap_or(0))
                    };
                }
            }
            FilterField::Offset => {
                edit_digits(code, &mut self.offset_text);
                filter.anchor =
                    sim_session::state::HexAnchor::At(self.offset_text.parse().unwrap_or(0));
            }
            FilterField::Source => edit_text(code, &mut filter.source),
            FilterField::Text => edit_text(code, &mut filter.text),
            FilterField::MinLen => {
                edit_digits(code, &mut self.min_len_text);
                filter.min_len = self.min_len_text.parse().ok();
            }
            FilterField::MaxLen => {
                edit_digits(code, &mut self.max_len_text);
                filter.max_len = self.max_len_text.parse().ok();
            }
            FilterField::Invert => {
                if matches!(code, KeyCode::Char(' ')) {
                    filter.invert = !filter.invert;
                }
            }
        }
    }
}

/// `Right` steps forward, `Left` steps back, anything else does nothing.
fn arrow_delta(code: KeyCode) -> Option<isize> {
    match code {
        KeyCode::Right => Some(1),
        KeyCode::Left => Some(-1),
        _ => None,
    }
}

fn edit_text(code: KeyCode, text: &mut String) {
    match code {
        KeyCode::Char(letter) => text.push(letter),
        KeyCode::Backspace => {
            text.pop();
        }
        _ => {}
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a typed number is clamped to the field's own width by the encoder, \
              which is a truer check than one done here on the way in"
)]
fn typed_override_value(kind: &FieldKind, text: &str) -> Option<sim_core::frame::value::Value> {
    match kind {
        FieldKind::Bytes { len } => {
            let mut bytes = hex::parse(text).ok()?;
            bytes.resize(*len, 0);
            Some(sim_core::frame::value::Value::Bytes(bytes))
        }
        FieldKind::Text { len } => {
            let mut held = text.to_owned();
            held.truncate(*len);
            Some(sim_core::frame::value::Value::Text(held))
        }
        FieldKind::Scalar(scalar) => {
            let value = hex::read_number(text)?;
            Some(
                if matches!(
                    scalar,
                    sim_core::frame::ScalarType::F32 | sim_core::frame::ScalarType::F64
                ) {
                    sim_core::frame::value::Value::Float(value)
                } else if scalar.is_unsigned_integer() {
                    sim_core::frame::value::Value::Uint(value.max(0.0) as u64)
                } else {
                    sim_core::frame::value::Value::Int(value as i64)
                },
            )
        }
        FieldKind::Enum { .. } => {
            let value = hex::read_number(text)?;
            Some(sim_core::frame::value::Value::Uint(value.max(0.0) as u64))
        }
        FieldKind::Bits { .. } | FieldKind::Checksum { .. } => None,
    }
}

fn edit_digits(code: KeyCode, text: &mut String) {
    match code {
        KeyCode::Char(digit) if digit.is_ascii_digit() => text.push(digit),
        KeyCode::Backspace => {
            text.pop();
        }
        _ => {}
    }
}

/// One row of the scenario editor: the header, or a step.
/// One row of the frame editor: the header, or a field.
pub(crate) enum FrameRow {
    Name,
    Endian,
    Field(usize),
}

pub(crate) fn frame_rows(frame: &FrameDef) -> Vec<FrameRow> {
    let mut rows = vec![FrameRow::Name, FrameRow::Endian];
    rows.extend((0..frame.fields.len()).map(FrameRow::Field));
    rows
}

/// One field, opened for its own editor.
pub struct FrameFieldEdit {
    field: usize,
    focus: usize,
}

/// One field of the field editor: what it is, and how a key changes it.
#[derive(PartialEq, Eq)]
enum FrameFieldRow {
    Name,
    Kind,
    Length,
    Repr,
    Variant(usize),
    Bit(usize),
    CoversFrom,
    CoversTo,
}

impl FrameFieldEdit {
    fn opening(field: usize) -> Self {
        Self { field, focus: 0 }
    }

    #[must_use]
    pub fn field_index(&self) -> usize {
        self.field
    }

    #[must_use]
    fn focus(&self) -> usize {
        self.focus
    }

    fn fields(field: &FieldDef) -> Vec<FrameFieldRow> {
        let mut fields = vec![FrameFieldRow::Name, FrameFieldRow::Kind];
        match &field.kind {
            FieldKind::Bytes { .. } | FieldKind::Text { .. } => fields.push(FrameFieldRow::Length),
            FieldKind::Enum { variants, .. } => {
                fields.push(FrameFieldRow::Repr);
                fields.extend((0..variants.len()).map(FrameFieldRow::Variant));
            }
            FieldKind::Bits { bits, .. } => {
                fields.push(FrameFieldRow::Repr);
                fields.extend((0..bits.len()).map(FrameFieldRow::Bit));
            }
            FieldKind::Checksum { .. } => {
                fields.push(FrameFieldRow::CoversFrom);
                fields.push(FrameFieldRow::CoversTo);
            }
            FieldKind::Scalar(_) => {}
        }
        fields
    }

    #[must_use]
    pub fn lines(&self, field: &FieldDef, frame: &FrameDef) -> Vec<(String, String, bool)> {
        let rows = Self::fields(field);
        let focus = self.focus.min(rows.len().saturating_sub(1));
        rows.iter()
            .enumerate()
            .map(|(at, row)| {
                let (label, value) = Self::render(row, field, frame);
                (label, value, at == focus)
            })
            .collect()
    }

    fn render(row: &FrameFieldRow, field: &FieldDef, frame: &FrameDef) -> (String, String) {
        match row {
            FrameFieldRow::Name => ("Name".to_owned(), field.name.clone()),
            FrameFieldRow::Kind => ("Kind".to_owned(), kinds::label_of(&field.kind)),
            FrameFieldRow::Length => {
                let len = match &field.kind {
                    FieldKind::Bytes { len } | FieldKind::Text { len } => *len,
                    _ => 0,
                };
                ("Length".to_owned(), len.to_string())
            }
            FrameFieldRow::Repr => {
                let repr = match &field.kind {
                    FieldKind::Enum { repr, .. } | FieldKind::Bits { repr, .. } => Some(*repr),
                    _ => None,
                };
                (
                    "Repr".to_owned(),
                    repr.map_or_else(String::new, |repr| repr.name().to_owned()),
                )
            }
            FrameFieldRow::Variant(i) => {
                let FieldKind::Enum { variants, .. } = &field.kind else {
                    return (String::new(), String::new());
                };
                let Some(variant) = variants.get(*i) else {
                    return (String::new(), String::new());
                };
                (
                    format!("  Variant {i}"),
                    format!("{} = {}", variant.name, variant.value),
                )
            }
            FrameFieldRow::Bit(i) => {
                let FieldKind::Bits { bits, .. } = &field.kind else {
                    return (String::new(), String::new());
                };
                let Some(bit) = bits.get(*i) else {
                    return (String::new(), String::new());
                };
                (
                    format!("  Bit {i}"),
                    format!("{} ({})", bit.name, bit.width),
                )
            }
            FrameFieldRow::CoversFrom => {
                let FieldKind::Checksum { covers, .. } = &field.kind else {
                    return (String::new(), String::new());
                };
                (
                    "Covers from".to_owned(),
                    frame
                        .fields
                        .get(covers.from)
                        .map_or_else(String::new, |f| f.name.clone()),
                )
            }
            FrameFieldRow::CoversTo => {
                let FieldKind::Checksum { covers, .. } = &field.kind else {
                    return (String::new(), String::new());
                };
                (
                    "Covers to".to_owned(),
                    frame
                        .fields
                        .get(covers.to)
                        .map_or_else(String::new, |f| f.name.clone()),
                )
            }
        }
    }
}

pub(crate) enum ScenarioRow {
    Name,
    Description,
    Repeat,
    RepeatEvery,
    RepeatTimes,
    Step(usize),
}

pub(crate) fn scenario_rows(scenario: &Scenario) -> Vec<ScenarioRow> {
    let mut rows = vec![
        ScenarioRow::Name,
        ScenarioRow::Description,
        ScenarioRow::Repeat,
    ];
    if scenario.repeat.is_some() {
        rows.push(ScenarioRow::RepeatEvery);
        rows.push(ScenarioRow::RepeatTimes);
    }
    rows.extend((0..scenario.steps.len()).map(ScenarioRow::Step));
    rows
}

/// One field of the step editor: what it is, and how a key changes it.
enum StepField {
    Kind,
    /// One row per connection known to the project, ticked on or off the
    /// step's targets.
    Target(usize),
    Delay,
    Bytes,
    SendFrame,
    /// Index into the chosen frame's fields, checksums already filtered out.
    SendField(usize),
    WaitByFrame,
    WaitPattern,
    WaitAnchored,
    WaitOffset,
    WaitFrame,
    WaitField(usize),
    Limited,
    TimeoutMs,
}

/// One step, opened for its own editor.
///
/// Text fields share one scratch buffer rather than one each: only the field
/// under the cursor is ever being typed into, and reseeding it from the model
/// when the cursor moves is simpler than keeping a buffer per field that
/// mostly sits unused.
pub struct StepEdit {
    step: usize,
    focus: usize,
    text: String,
}

impl StepEdit {
    fn opening(step: usize) -> Self {
        Self {
            step,
            focus: 0,
            text: String::new(),
        }
    }

    /// The fields worth asking for, given the step's own action kind, and
    /// whether waiting is by pattern or by frame.
    fn fields(step: &Step, names: &[String]) -> Vec<StepField> {
        let kind = ActionKind::of(&step.action);
        let mut fields = vec![StepField::Kind];
        if kind.needs_a_connection() {
            fields.extend((0..names.len()).map(StepField::Target));
        }
        match &step.action {
            Action::Wait { .. } => fields.push(StepField::Delay),
            Action::Raw { .. } => fields.push(StepField::Bytes),
            Action::Send { with, .. } => {
                fields.push(StepField::SendFrame);
                // The frame's own field count is not known here without the
                // library; the caller expands `SendField` once it has one.
                let _ = with;
            }
            Action::WaitFor { expect, timeout } => {
                fields.push(StepField::WaitByFrame);
                match expect {
                    Expect::Pattern { anchor, .. } => {
                        fields.push(StepField::WaitPattern);
                        fields.push(StepField::WaitAnchored);
                        if matches!(anchor, sim_core::pattern::Anchor::At(_)) {
                            fields.push(StepField::WaitOffset);
                        }
                    }
                    Expect::Frame { .. } => fields.push(StepField::WaitFrame),
                }
                fields.push(StepField::Limited);
                if timeout.is_some() {
                    fields.push(StepField::TimeoutMs);
                }
            }
        }
        fields
    }

    /// The same list, with `SendField`/`WaitField` rows expanded once the
    /// chosen frame is known.
    fn fields_with_frame(step: &Step, names: &[String], frames: &[FrameDef]) -> Vec<StepField> {
        let mut fields = Self::fields(step, names);
        let expand = |name: &str| -> Vec<usize> {
            frames
                .iter()
                .find(|frame| frame.name == name)
                .map(|frame| {
                    (0..frame.fields.len())
                        .filter(|&i| !matches!(frame.fields[i].kind, FieldKind::Checksum { .. }))
                        .collect()
                })
                .unwrap_or_default()
        };
        match &step.action {
            Action::Send { frame, .. } => {
                let at = fields
                    .iter()
                    .position(|f| matches!(f, StepField::SendFrame));
                if let Some(at) = at {
                    let indices = expand(frame);
                    fields.splice((at + 1)..=at, indices.into_iter().map(StepField::SendField));
                }
            }
            Action::WaitFor {
                expect: Expect::Frame { frame, .. },
                ..
            } => {
                let at = fields
                    .iter()
                    .position(|f| matches!(f, StepField::WaitFrame));
                if let Some(at) = at {
                    let indices = expand(frame);
                    fields.splice((at + 1)..=at, indices.into_iter().map(StepField::WaitField));
                }
            }
            _ => {}
        }
        fields
    }
}

impl StepEdit {
    /// Which step of the draft this is open on.
    #[must_use]
    pub fn step_index(&self) -> usize {
        self.step
    }

    #[must_use]
    pub fn lines(
        &self,
        step: &Step,
        names: &[String],
        frames: &[FrameDef],
    ) -> Vec<(String, String, bool)> {
        let fields = Self::fields_with_frame(step, names, frames);
        let focus = self.focus.min(fields.len().saturating_sub(1));
        fields
            .iter()
            .enumerate()
            .map(|(at, field)| {
                let (label, value) = Self::render(field, step, names, frames);
                (label, value, at == focus)
            })
            .collect()
    }

    fn render(
        field: &StepField,
        step: &Step,
        names: &[String],
        frames: &[FrameDef],
    ) -> (String, String) {
        match field {
            StepField::Kind
            | StepField::Target(_)
            | StepField::Delay
            | StepField::Bytes
            | StepField::SendFrame
            | StepField::SendField(_) => Self::render_send_side(field, step, names, frames),
            _ => Self::render_wait_side(field, step, frames),
        }
    }

    fn render_send_side(
        field: &StepField,
        step: &Step,
        names: &[String],
        frames: &[FrameDef],
    ) -> (String, String) {
        match field {
            StepField::Kind => (
                "Action".to_owned(),
                ActionKind::of(&step.action).label().to_owned(),
            ),
            StepField::Target(i) => (
                format!("  {}", names[*i]),
                yes_no(step.targets.iter().any(|id| id.0 == names[*i])).to_owned(),
            ),
            StepField::Delay => {
                let Action::Wait { delay } = &step.action else {
                    return (String::new(), String::new());
                };
                ("Delay (ms)".to_owned(), delay.as_millis().to_string())
            }
            StepField::Bytes => {
                let Action::Raw { bytes } = &step.action else {
                    return (String::new(), String::new());
                };
                ("Bytes".to_owned(), hex::spaced(bytes))
            }
            StepField::SendFrame => {
                let Action::Send { frame, .. } = &step.action else {
                    return (String::new(), String::new());
                };
                (
                    "Frame".to_owned(),
                    if frame.is_empty() {
                        "none".to_owned()
                    } else {
                        frame.clone()
                    },
                )
            }
            StepField::SendField(i) => {
                let Action::Send {
                    frame,
                    with,
                    counters,
                    from_capture,
                } = &step.action
                else {
                    return (String::new(), String::new());
                };
                let definition = frames.iter().find(|f| &f.name == frame);
                let Some(field_def) = definition.and_then(|d| d.fields.get(*i)) else {
                    return (String::new(), String::new());
                };
                let overridden = with.get(&field_def.name);
                let counted = counters.contains_key(&field_def.name);
                let value = if let Some(value) = overridden {
                    reading::describe(field_def, value, false)
                } else if let Some(variable) = from_capture.get(&field_def.name) {
                    format!("from capture: {variable}")
                } else if counted {
                    "counted".to_owned()
                } else {
                    "frame default".to_owned()
                };
                (format!("  {}", field_def.name), value)
            }
            _ => (String::new(), String::new()),
        }
    }

    fn render_wait_side(field: &StepField, step: &Step, frames: &[FrameDef]) -> (String, String) {
        match field {
            StepField::WaitByFrame
            | StepField::WaitPattern
            | StepField::WaitAnchored
            | StepField::WaitOffset => Self::render_wait_pattern(field, step),
            _ => Self::render_wait_frame(field, step, frames),
        }
    }

    fn render_wait_pattern(field: &StepField, step: &Step) -> (String, String) {
        match field {
            StepField::WaitByFrame => {
                let Action::WaitFor { expect, .. } = &step.action else {
                    return (String::new(), String::new());
                };
                (
                    "Wait for".to_owned(),
                    if matches!(expect, Expect::Frame { .. }) {
                        "a frame".to_owned()
                    } else {
                        "these bytes".to_owned()
                    },
                )
            }
            StepField::WaitPattern => {
                let Action::WaitFor {
                    expect: Expect::Pattern { pattern, .. },
                    ..
                } = &step.action
                else {
                    return (String::new(), String::new());
                };
                ("  Pattern".to_owned(), pattern.to_hex())
            }
            StepField::WaitAnchored => {
                let Action::WaitFor {
                    expect: Expect::Pattern { anchor, .. },
                    ..
                } = &step.action
                else {
                    return (String::new(), String::new());
                };
                (
                    "  At offset".to_owned(),
                    yes_no(matches!(anchor, sim_core::pattern::Anchor::At(_))).to_owned(),
                )
            }
            StepField::WaitOffset => {
                let Action::WaitFor {
                    expect:
                        Expect::Pattern {
                            anchor: sim_core::pattern::Anchor::At(offset),
                            ..
                        },
                    ..
                } = &step.action
                else {
                    return (String::new(), String::new());
                };
                ("    Offset".to_owned(), offset.to_string())
            }
            _ => (String::new(), String::new()),
        }
    }

    fn render_wait_frame(field: &StepField, step: &Step, frames: &[FrameDef]) -> (String, String) {
        match field {
            StepField::WaitFrame => {
                let Action::WaitFor {
                    expect: Expect::Frame { frame, .. },
                    ..
                } = &step.action
                else {
                    return (String::new(), String::new());
                };
                (
                    "  Frame".to_owned(),
                    if frame.is_empty() {
                        "none".to_owned()
                    } else {
                        frame.clone()
                    },
                )
            }
            StepField::WaitField(i) => {
                let Action::WaitFor {
                    expect:
                        Expect::Frame {
                            frame,
                            values,
                            capture,
                        },
                    ..
                } = &step.action
                else {
                    return (String::new(), String::new());
                };
                let definition = frames.iter().find(|f| &f.name == frame);
                let Some(field_def) = definition.and_then(|d| d.fields.get(*i)) else {
                    return (String::new(), String::new());
                };
                let matched = values.contains_key(&field_def.name);
                let mut value = if matched {
                    values
                        .get(&field_def.name)
                        .map_or_else(String::new, |v| reading::describe(field_def, v, false))
                } else {
                    "any value".to_owned()
                };
                if let Some(variable) = capture.get(&field_def.name) {
                    value = format!("{value} · capture as {variable}");
                }
                (format!("  {}", field_def.name), value)
            }
            StepField::Limited => {
                let Action::WaitFor { timeout, .. } = &step.action else {
                    return (String::new(), String::new());
                };
                (
                    "Give up after".to_owned(),
                    yes_no(timeout.is_some()).to_owned(),
                )
            }
            StepField::TimeoutMs => {
                let Action::WaitFor {
                    timeout: Some(timeout),
                    ..
                } = &step.action
                else {
                    return (String::new(), String::new());
                };
                ("  Timeout (ms)".to_owned(), timeout.as_millis().to_string())
            }
            _ => (String::new(), String::new()),
        }
    }

    /// Reseeds the scratch text from the field under the cursor, when it has
    /// text worth continuing to type into.
    fn reseed(&mut self, step: &Step, names: &[String], frames: &[FrameDef]) {
        let fields = Self::fields_with_frame(step, names, frames);
        let Some(field) = fields.get(self.focus.min(fields.len().saturating_sub(1))) else {
            self.text.clear();
            return;
        };
        self.text = match field {
            StepField::Delay => {
                if let Action::Wait { delay } = &step.action {
                    delay.as_millis().to_string()
                } else {
                    String::new()
                }
            }
            StepField::Bytes => {
                if let Action::Raw { bytes } = &step.action {
                    hex::packed(bytes)
                } else {
                    String::new()
                }
            }
            StepField::WaitPattern => {
                if let Action::WaitFor {
                    expect: Expect::Pattern { pattern, .. },
                    ..
                } = &step.action
                {
                    pattern.to_hex()
                } else {
                    String::new()
                }
            }
            StepField::WaitOffset => {
                if let Action::WaitFor {
                    expect:
                        Expect::Pattern {
                            anchor: sim_core::pattern::Anchor::At(offset),
                            ..
                        },
                    ..
                } = &step.action
                {
                    offset.to_string()
                } else {
                    String::new()
                }
            }
            StepField::TimeoutMs => {
                if let Action::WaitFor {
                    timeout: Some(timeout),
                    ..
                } = &step.action
                {
                    timeout.as_millis().to_string()
                } else {
                    String::new()
                }
            }
            StepField::SendField(i) => Self::reseed_send_field(*i, step, frames),
            StepField::WaitField(i) => Self::reseed_wait_field(*i, step, frames),
            _ => String::new(),
        };
    }

    fn reseed_send_field(i: usize, step: &Step, frames: &[FrameDef]) -> String {
        let Action::Send { frame, with, .. } = &step.action else {
            return String::new();
        };
        let Some(field_def) = frames
            .iter()
            .find(|f| &f.name == frame)
            .and_then(|d| d.fields.get(i))
        else {
            return String::new();
        };
        with.get(&field_def.name)
            .map_or_else(String::new, |value| match &field_def.kind {
                FieldKind::Bytes { .. } => value.as_bytes().map_or_else(String::new, hex::packed),
                FieldKind::Text { .. } => value.as_text().unwrap_or_default().to_owned(),
                _ => reading::describe(field_def, value, false),
            })
    }

    fn reseed_wait_field(i: usize, step: &Step, frames: &[FrameDef]) -> String {
        let Action::WaitFor {
            expect:
                Expect::Frame {
                    frame,
                    values,
                    capture,
                },
            ..
        } = &step.action
        else {
            return String::new();
        };
        let Some(field_def) = frames
            .iter()
            .find(|f| &f.name == frame)
            .and_then(|d| d.fields.get(i))
        else {
            return String::new();
        };
        // A captured name is what free text edits while both could apply, the
        // two never actually meeting: a field worth remembering by name is
        // rarely also one pinned to an exact value.
        if let Some(variable) = capture.get(&field_def.name) {
            return variable.clone();
        }
        values
            .get(&field_def.name)
            .map_or_else(String::new, |value| match &field_def.kind {
                FieldKind::Bytes { .. } => value.as_bytes().map_or_else(String::new, hex::packed),
                FieldKind::Text { .. } => value.as_text().unwrap_or_default().to_owned(),
                _ => reading::describe(field_def, value, false),
            })
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

pub struct Browser {
    at: PathBuf,
    picker: Picker,
    /// What went wrong reading a folder, in place of its contents.
    trouble: Option<String>,
    mode: BrowserMode,
}

/// What choosing an entry does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BrowserMode {
    /// A file to be opened.
    Open,
    /// A folder to save into, named once it is reached.
    Save,
}

/// The entry that goes back up, shown first so it is always in the same place.
const UPWARDS: &str = "..";

impl Browser {
    fn opening(at: PathBuf) -> Self {
        Self::browsing(at, BrowserMode::Open)
    }

    fn saving(at: PathBuf) -> Self {
        Self::browsing(at, BrowserMode::Save)
    }

    fn browsing(at: PathBuf, mode: BrowserMode) -> Self {
        let mut browser = Self {
            at,
            picker: Picker::new(String::new(), Vec::new()),
            trouble: None,
            mode,
        };
        browser.listing();
        browser
    }

    #[must_use]
    pub fn mode(&self) -> BrowserMode {
        self.mode
    }

    /// Where the walk currently is, which is what the popup titles itself with.
    #[must_use]
    pub fn at(&self) -> &Path {
        &self.at
    }

    #[must_use]
    pub fn trouble(&self) -> Option<&str> {
        self.trouble.as_deref()
    }

    #[must_use]
    pub fn picker(&self) -> &Picker {
        &self.picker
    }

    /// Folders first, then files, each in name order.
    ///
    /// A folder that cannot be read says so in place of its contents rather
    /// than showing an empty one, which reads as a folder with nothing in it.
    fn listing(&mut self) {
        let mut folders = Vec::new();
        let mut files = Vec::new();

        match std::fs::read_dir(&self.at) {
            Ok(entries) => {
                self.trouble = None;
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    // Dot files are noise on the way to a project.
                    if name.starts_with('.') {
                        continue;
                    }
                    if entry.path().is_dir() {
                        folders.push(format!("{name}/"));
                    } else {
                        files.push(name);
                    }
                }
            }
            Err(error) => self.trouble = Some(format!("{}: {error}", self.at.display())),
        }

        folders.sort();
        files.sort();

        let mut options = vec![UPWARDS.to_owned()];
        options.append(&mut folders);
        options.append(&mut files);
        self.picker = Picker::new(String::new(), options);
    }

    /// Takes the line under the cursor. A folder is walked into, a file is the
    /// answer.
    fn taken(&mut self) -> Option<PathBuf> {
        let name = self.picker.taken()?;
        if name == UPWARDS {
            if let Some(up) = self.at.parent() {
                self.at = up.to_path_buf();
                self.listing();
            }
            return None;
        }
        let path = self.at.join(name.trim_end_matches('/'));
        if path.is_dir() {
            self.at = path;
            self.listing();
            return None;
        }
        Some(path)
    }
}

/// A list to choose one line from, narrowed by what is typed.
///
/// The window has combo boxes. A terminal has this, and it is the same idea:
/// the choice is offered rather than spelled, so a name that does not exist
/// cannot be given.
pub struct Picker {
    pub title: String,
    options: Vec<String>,
    typed: String,
    at: usize,
}

impl Picker {
    fn new(title: impl Into<String>, options: Vec<String>) -> Self {
        Self {
            title: title.into(),
            options,
            typed: String::new(),
            at: 0,
        }
    }

    /// What is typed so far, shown so that a narrowed list explains itself.
    #[must_use]
    pub fn typed(&self) -> &str {
        &self.typed
    }

    /// The lines still on offer, and which of them is under the cursor.
    #[must_use]
    pub fn shown(&self) -> (Vec<&str>, usize) {
        let wanted = self.typed.to_lowercase();
        let shown: Vec<&str> = self
            .options
            .iter()
            .filter(|option| option.to_lowercase().contains(&wanted))
            .map(String::as_str)
            .collect();
        let at = self.at.min(shown.len().saturating_sub(1));
        (shown, at)
    }

    fn step(&mut self, delta: isize) {
        let (shown, at) = self.shown();
        let Some(last) = shown.len().checked_sub(1) else {
            return;
        };
        self.at = at.saturating_add_signed(delta).min(last);
    }

    fn taken(&self) -> Option<String> {
        let (shown, at) = self.shown();
        shown.get(at).map(|name| (*name).to_owned())
    }

    /// Typing narrows the list and puts the cursor back at the top, since the
    /// line it was on is usually not the line still wanted.
    fn typing(&mut self, letter: char) {
        self.typed.push(letter);
        self.at = 0;
    }

    fn rubbed_out(&mut self) {
        self.typed.pop();
        self.at = 0;
    }
}

pub struct App {
    tab: Tab,
    overlay: Option<Overlay>,
    /// Whether a box has the keyboard.
    ///
    /// A box being typed into takes every letter, digits included, so while one
    /// does the keys that move between views are not the view's to take. Said
    /// as a state rather than left implicit, or the hint line would promise a
    /// way out that types a `q` instead.
    editing: bool,
    running: bool,
    session: Session,
    engine: EngineHandle,
    /// The project this was opened with, for the header to name.
    path: Option<PathBuf>,
    /// The folder last looked in, whether or not it held what was wanted.
    ///
    /// Kept apart from `path`, which is a project that opened. Naming a file
    /// that is not there is usually a typo, and the folder around it is where
    /// the right name is.
    looked_in: Option<PathBuf>,
    /// The project as it was last read from disk, kept so a save can carry
    /// over the sections this front end does not understand: the pane
    /// arrangement and the theme.
    opened_project: Option<Project>,
    /// What the project looked like the last time it was opened or saved,
    /// compared against on every draw to say whether there is anything to
    /// save.
    saved: Option<Project>,
    /// The connection under the cursor in the Connections view.
    connection_at: Option<usize>,
    /// The last thing that went right, until the next key reads it.
    ///
    /// Apart from `session.last_error`, which is for what did not: the two
    /// never compete for the one line reserved above the hints.
    status: Option<String>,
    /// The traffic view on show, when there is more than one.
    current_monitor: Option<sim_session::state::MonitorId>,
    /// The row under the cursor in the scenario editor's header and step
    /// list, while a draft is open.
    scenario_row: Option<usize>,
    /// The row under the cursor in the frame editor's header and field list,
    /// while a draft is open.
    frame_row: Option<usize>,
    /// Which pane of the Frames view a key acts on.
    frame_focus: FramesFocus,
    /// The row under the cursor in the fields pane, and the frame it belongs
    /// to.
    ///
    /// The frame is kept alongside the index so that switching to a
    /// differently shaped frame resets the cursor rather than landing on
    /// whatever row happened to share its number.
    field_at: Option<(String, usize)>,
    /// Which pane of the Traffic view up/down acts on.
    traffic_focus: TrafficFocus,
    /// How far the Traffic tab's decoded fields pane has scrolled, reset
    /// whenever the row it describes changes.
    traffic_field_scroll: usize,
}

impl Default for App {
    fn default() -> Self {
        let mut session = Session::default();
        // A view to show traffic in, before any project says otherwise. Without
        // one there is no filter to pass, so nothing would ever be drawn.
        session.open_monitor();
        let mut app = Self {
            tab: Tab::Connections,
            overlay: None,
            editing: false,
            running: true,
            session,
            engine: EngineHandle::new(),
            path: None,
            looked_in: None,
            opened_project: None,
            saved: None,
            connection_at: None,
            status: None,
            current_monitor: None,
            scenario_row: None,
            frame_row: None,
            frame_focus: FramesFocus::Library,
            field_at: None,
            traffic_focus: TrafficFocus::Rows,
            traffic_field_scroll: 0,
        };
        // Compared against from the first key pressed, so an app that has not
        // been touched yet is never mistaken for one with unsaved work.
        app.saved = Some(app.snapshot());
        app
    }
}

impl App {
    /// Opens what the command line named: a project file, or a folder of frame
    /// definitions.
    ///
    /// A link the project marked to open is opened straight away. A bench is
    /// reached to watch something already happening, and a first keystroke
    /// spent switching links on is a keystroke the operator should not need.
    #[must_use]
    pub fn opening(opened_with: Option<PathBuf>) -> Self {
        let mut app = Self::default();
        match opened_with {
            Some(path) if path.is_dir() => app.session.frames.load_from(path),
            Some(path) => {
                app.looked_in = path.parent().map(Path::to_path_buf);
                app.open(&path);
            }
            None => {}
        }
        // A project without one, or no project at all, still has traffic to
        // show.
        if app.session.monitors.is_empty() {
            app.session.open_monitor();
        }
        // A frames-only folder changed what `Default` captured above without
        // going through `open`, so it needs its own comparison point too.
        app.saved = Some(app.snapshot());
        app
    }

    fn open(&mut self, path: &Path) {
        let loaded = Project::read(path);
        let loaded = loaded.and_then(|read| {
            read.apply(&mut self.session, Some(path))
                .map(|restored| (read, restored))
        });
        match loaded {
            Ok((read, restored)) => {
                self.looked_in = path.parent().map(Path::to_path_buf);
                for (id, config, retry) in restored.connect {
                    self.engine.connect(id, config, retry);
                }
                self.session.restore_monitors(restored.monitors);
                // A project describing none still needs somewhere to show
                // traffic, whether this is the first project this session has
                // opened or the fifth.
                if self.session.monitors.is_empty() {
                    self.session.open_monitor();
                }
                self.path = Some(path.to_path_buf());
                self.opened_project = Some(read);
                self.saved = Some(self.snapshot());
            }
            Err(error) => self.session.last_error = Some(format!("{error:#}")),
        }
    }

    /// Writes to the file the project came from, or asks for one.
    fn save(&mut self) {
        match self.path.clone() {
            Some(path) => self.save_to(&path),
            None => self.save_as(),
        }
    }

    /// Offers the disk to pick a folder to save into, starting where the
    /// project on show sits.
    fn save_as(&mut self) {
        let at = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| self.looked_in.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        self.overlay = Some(Overlay::Browse(Browser::saving(at)));
    }

    /// Asks for the file name to save under, in the folder just chosen.
    fn prompt_save_name(&mut self, directory: PathBuf, seed: Option<String>) {
        self.overlay = Some(Overlay::EditText(EditBox {
            title: "Save as".to_owned(),
            text: seed.unwrap_or_else(|| sim_session::project::DEFAULT_FILE_NAME.to_owned()),
            target: EditTarget::SaveFileName(directory),
        }));
    }

    fn save_to(&mut self, path: &Path) {
        let mut project = Project::capture_settings(&self.session, self.theme(), Some(path));
        if let Some(original) = &self.opened_project {
            project = project.carrying_over(original);
        }
        match project.write(path) {
            Ok(()) => {
                self.path = Some(path.to_path_buf());
                self.saved = Some(project);
                self.status = Some(format!("Saved to {}.", path.display()));
            }
            Err(error) => self.session.last_error = Some(format!("{error:#}")),
        }
    }

    /// The theme a saved project should keep, which is whatever the file
    /// already asked for: this front end has no notion of light or dark of
    /// its own to write in its place.
    fn theme(&self) -> sim_session::project::ThemeSpec {
        self.opened_project
            .as_ref()
            .map_or(sim_session::project::ThemeSpec::Dark, |project| {
                project.ui.theme
            })
    }

    /// Whether the project differs from what was last opened or saved.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.saved.as_ref() != Some(&self.snapshot())
    }

    /// Everything the session currently holds, described the way the file
    /// would be, but without a pane arrangement or a theme opinion of its
    /// own: see [`Project::carrying_over`].
    fn snapshot(&self) -> Project {
        Project::capture_settings(&self.session, self.theme(), self.path.as_deref())
    }

    /// Everything the engine has said since the last pass.
    pub fn take_engine_events(&mut self) {
        self.engine.drain_into(&mut self.session);
    }

    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The connection under the cursor in the Connections view.
    #[must_use]
    pub fn connection_at(&self) -> Option<usize> {
        self.connection_at
    }

    /// The row under the cursor in the scenario editor, while a draft is
    /// open.
    #[must_use]
    pub fn scenario_row(&self) -> Option<usize> {
        self.scenario_row
    }

    /// The row under the cursor in the frame editor, while a draft is open.
    #[must_use]
    pub fn frame_row(&self) -> Option<usize> {
        self.frame_row
    }

    /// Whether a key in the Frames view acts on the frame list or the fields
    /// of the one chosen.
    #[must_use]
    pub fn frame_focus_is_fields(&self) -> bool {
        self.frame_focus == FramesFocus::Fields
    }

    /// How far the Traffic tab's decoded fields pane has scrolled.
    #[must_use]
    pub fn traffic_field_scroll(&self) -> usize {
        self.traffic_field_scroll
    }

    /// Whether up/down in the Traffic tab scrolls the decoded fields pane
    /// rather than moving the row being read.
    #[must_use]
    pub fn traffic_focus_is_fields(&self) -> bool {
        self.traffic_focus == TrafficFocus::Fields
    }

    /// The row under the cursor in the fields pane, for the frame currently
    /// on show.
    #[must_use]
    pub fn field_at(&self, frame: &str) -> Option<usize> {
        match &self.field_at {
            Some((name, at)) if name == frame => Some(*at),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// The traffic view this front end shows.
    ///
    /// The window has a tab per view. A terminal has one screen, so it shows
    /// the first the project described, which is where its filter and its
    /// follow setting come from.
    /// The id of the traffic view on show, falling back to the first one held
    /// once the one last shown has been closed.
    fn monitor_id(&mut self) -> Option<sim_session::state::MonitorId> {
        let live = self
            .current_monitor
            .filter(|id| self.session.monitors.contains_key(id));
        let id = live.or_else(|| self.session.monitors.keys().next().copied());
        self.current_monitor = id;
        id
    }

    #[must_use]
    pub fn monitor(&self) -> Option<&MonitorState> {
        let id = self
            .current_monitor
            .filter(|id| self.session.monitors.contains_key(id))
            .or_else(|| self.session.monitors.keys().next().copied())?;
        self.session.monitors.get(&id)
    }

    /// Where the tab strip says the current view sits, one-based, and how
    /// many there are: `(2, 3)` reads "the second of three".
    #[must_use]
    pub fn monitor_position(&self) -> Option<(usize, usize)> {
        let id = self
            .current_monitor
            .filter(|id| self.session.monitors.contains_key(id))
            .or_else(|| self.session.monitors.keys().next().copied())?;
        let at = self.session.monitors.keys().position(|k| *k == id)?;
        Some((at + 1, self.session.monitors.len()))
    }

    /// The row being read, and what it reads as.
    ///
    /// Settling which definition to use is part of the answer, hence the
    /// mutable borrow: a row with one candidate takes it without asking.
    pub fn selected_reading(&mut self) -> Option<(&LogEntry, Reading<'_>)> {
        let id = self.monitor_id()?;
        let seq = self.session.monitors.get(&id)?.selected?;
        // Binary rather than linear: this runs on every redraw, whether or not
        // a key was pressed, and the log only ever grows at one end, seq
        // ascending, which is exactly what a search needs to stay fast at ten
        // thousand entries and ten redraws a second.
        let at = self
            .session
            .log
            .binary_search_by_key(&seq, |entry| entry.seq)
            .ok()?;
        let entry = self.session.log.get(at)?;
        let decode_as = &mut self.session.monitors.get_mut(&id)?.decode_as;
        let reading = reading::read(&self.session.frames, entry, decode_as);
        Some((entry, reading))
    }

    /// The rows that pass the view's filter, oldest first.
    #[must_use]
    pub fn rows(&self) -> Vec<&LogEntry> {
        let Some(monitor) = self.monitor() else {
            return Vec::new();
        };
        let compiled = monitor.filter.compile();
        self.session
            .log
            .iter()
            .filter(|entry| monitor.in_window(entry) && compiled.keeps(entry))
            .collect()
    }

    /// The project on show, by file name.
    #[must_use]
    pub fn opened(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    #[must_use]
    pub fn tab(&self) -> Tab {
        self.tab
    }

    #[must_use]
    pub fn overlay(&self) -> Option<&Overlay> {
        self.overlay.as_ref()
    }

    #[must_use]
    pub fn running(&self) -> bool {
        self.running
    }

    /// What every view answers to, whichever one is on show.
    pub const KEYS: [(&'static str, &'static str); 5] = [
        ("o", "open"),
        ("w", "save"),
        ("1-5", "go to a view"),
        ("Tab", "next view"),
        ("Shift+Tab", "previous view"),
    ];

    /// The way out, and the way to the rest.
    ///
    /// Kept apart because these are never dropped for want of room: not knowing
    /// how to leave a terminal program is how a session gets killed from
    /// another window.
    pub const ESCAPES: [(&'static str, &'static str); 2] = [("?", "keys"), ("q", "quit")];

    /// The way out of wherever the keyboard currently is.
    ///
    /// A box being typed into swallows `q`, so promising it there would be a
    /// lie. What it does answer to is `Esc`.
    #[must_use]
    pub fn escapes(&self) -> &'static [(&'static str, &'static str)] {
        if self.editing {
            &[("Esc", "done")]
        } else {
            &Self::ESCAPES
        }
    }

    /// The keys an open overlay answers to, which take the room over
    /// whatever the view behind it would otherwise offer.
    fn overlay_keys(&self) -> Option<&'static [(&'static str, &'static str)]> {
        if let Some(Overlay::Pick(_, _)) = self.overlay {
            return Some(&[
                ("up/down", "choose"),
                ("type", "narrow"),
                ("Enter", "take it"),
                ("Esc", "back"),
            ]);
        }
        if let Some(Overlay::Browse(browser)) = &self.overlay {
            return Some(if browser.mode() == BrowserMode::Save {
                &[
                    ("up/down", "choose"),
                    ("s", "save here"),
                    ("Enter", "into folder"),
                    ("Esc", "cancel"),
                ]
            } else {
                &[
                    ("up/down", "choose"),
                    ("type", "narrow"),
                    ("Enter", "open"),
                    ("Esc", "back"),
                ]
            });
        }
        if let Some(Overlay::NewConnection(_)) = self.overlay {
            return Some(&[
                ("Tab", "next field"),
                ("left/right", "change"),
                ("space", "toggle"),
                ("Enter", "create"),
                ("Esc", "cancel"),
            ]);
        }
        if let Some(Overlay::Filter(_)) = self.overlay {
            return Some(&[
                ("Tab", "next field"),
                ("left/right", "change"),
                ("space", "toggle"),
                ("Esc", "done"),
            ]);
        }
        if let Some(Overlay::Step(_)) = self.overlay {
            return Some(&[
                ("Tab", "next field"),
                ("left/right", "change, capture"),
                ("space", "toggle"),
                ("type", "edit"),
                ("Esc", "done"),
            ]);
        }
        if let Some(Overlay::EditText(_)) = self.overlay {
            return Some(&[("Enter", "apply"), ("Esc", "cancel")]);
        }
        if let Some(Overlay::FrameField(_)) = self.overlay {
            return Some(&[
                ("Tab", "next field"),
                ("left/right", "change"),
                ("a", "add"),
                ("x", "remove"),
                ("r", "reverse bits"),
                ("Esc", "done"),
            ]);
        }
        None
    }

    /// What the view on show adds to them.
    ///
    /// Offered without being asked for, since a key nobody can guess is a key
    /// nobody presses.
    #[must_use]
    pub fn view_keys(&self) -> &'static [(&'static str, &'static str)] {
        // An overlay has the keyboard, so it is its keys that are worth the
        // room: the view behind it answers to nothing while it is up.
        if self.editing {
            return &[("Enter", "send")];
        }

        // A list has the keyboard, so its keys are the ones worth the room. The
        // key map does not: it is what you open to read the view's own keys, so
        // it falls through to them.
        if let Some(keys) = self.overlay_keys() {
            return keys;
        }

        match self.tab {
            Tab::Traffic if self.traffic_focus == TrafficFocus::Fields => {
                &[("up/down", "scroll"), ("Left", "row list")]
            }
            Tab::Traffic => &[
                ("up/down", "read a row"),
                ("Right", "fields"),
                ("Enter", "read as"),
                ("p", "pause"),
                ("f", "follow"),
                ("c", "clear"),
                ("/", "filter"),
            ],
            Tab::Scenarios if self.session.scenarios.draft.is_some() => &[
                ("up/down", "choose"),
                ("Enter", "edit"),
                ("a", "add step"),
                ("x", "remove step"),
                ("s", "save"),
                ("Esc", "cancel"),
            ],
            Tab::Scenarios => &[
                ("up/down", "choose"),
                ("Enter", "run"),
                ("x", "stop"),
                ("n", "new"),
                ("e", "edit"),
            ],
            Tab::HexInject => &[("Enter", "type bytes"), ("t", "target"), ("x", "clear")],
            Tab::Frames if self.session.frames.draft.is_some() => &[
                ("up/down", "choose"),
                ("Enter", "edit"),
                ("a", "add field"),
                ("x", "remove field"),
                ("s", "save"),
                ("Esc", "cancel"),
            ],
            Tab::Frames if self.frame_focus == FramesFocus::Fields => &[
                ("up/down", "choose"),
                ("Enter", "edit"),
                ("s", "send"),
                ("Left", "list"),
            ],
            Tab::Frames => &[
                ("up/down", "choose"),
                ("Right", "fields"),
                ("t", "target"),
                ("s", "send"),
            ],
            Tab::Connections => &[
                ("up/down", "choose"),
                ("Enter", "toggle"),
                ("n", "new"),
                ("x", "remove"),
            ],
        }
    }

    /// What went wrong, until the next key says it has been read.
    #[must_use]
    pub fn trouble(&self) -> Option<&str> {
        self.session.last_error.as_deref()
    }

    /// The last thing that went right, until the next key reads it.
    #[must_use]
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn handle(&mut self, key: KeyEvent) {
        // Cleared before the key is acted on, not after: an action that fails
        // again puts its message straight back, and one that succeeds leaves
        // the line to whatever comes next. Anything else would have a stale
        // complaint outlive the thing complained about.
        self.session.last_error = None;
        self.status = None;

        // Ctrl+C is the one key a terminal program may not redefine, whatever
        // else is on screen.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.running = false;
            return;
        }

        // An overlay takes the keys it knows and swallows the rest, so that
        // reading a list cannot change the view behind it.
        if self.overlay.is_some() {
            self.over(key.code);
            return;
        }

        if self.editing {
            self.typing(key.code);
            return;
        }

        // What the view does with a key comes first: a list has to have Up and
        // Down before anything else claims them.
        let taken = match self.tab {
            Tab::Traffic => self.watching(key.code),
            Tab::Scenarios => self.running_scenarios(key.code),
            Tab::HexInject => self.injecting(key.code),
            Tab::Frames => self.framing(key.code),
            Tab::Connections => self.connecting(key.code),
        };
        if taken {
            return;
        }

        match key.code {
            KeyCode::Char('q') => self.running = false,
            KeyCode::Char('?') => self.overlay = Some(Overlay::Keys),
            KeyCode::Char('o') => self.browse(),
            KeyCode::Char('w') => self.save(),
            KeyCode::Char('W') => self.save_as(),
            KeyCode::Char('n') if self.tab == Tab::Connections => {
                self.overlay = Some(Overlay::NewConnection(ConnectionForm::default()));
            }
            KeyCode::Tab => self.tab = Tab::at(self.tab.index() + 1),
            KeyCode::BackTab => self.tab = Tab::at(self.tab.index() + Tab::ALL.len() - 1),
            KeyCode::Char(digit @ '1'..='5') => {
                let wanted = digit as usize - '1' as usize;
                self.tab = Tab::at(wanted);
            }
            _ => {}
        }
    }

    /// What an overlay does with a key. Everything else it swallows.
    fn over(&mut self, code: KeyCode) {
        if matches!(self.overlay, Some(Overlay::Filter(_))) {
            self.filter_key(code);
            return;
        }
        if matches!(self.overlay, Some(Overlay::Step(_))) {
            self.step_key(code);
            return;
        }
        if matches!(self.overlay, Some(Overlay::FrameField(_))) {
            self.frame_field_key(code);
            return;
        }
        let Some(overlay) = &mut self.overlay else {
            return;
        };
        match overlay {
            Overlay::Keys => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('?')) {
                    self.overlay = None;
                }
            }
            Overlay::NewConnection(form) => match code {
                KeyCode::Tab | KeyCode::Down => form.next(),
                KeyCode::BackTab | KeyCode::Up => form.previous(),
                KeyCode::Left => form.cycle(-1),
                KeyCode::Right => form.cycle(1),
                KeyCode::Char(' ') => form.toggle(),
                KeyCode::Backspace => form.backspace(),
                KeyCode::Char(letter) => form.type_char(letter),
                KeyCode::Esc => self.overlay = None,
                KeyCode::Enter => self.submit_connection(),
                _ => {}
            },
            Overlay::Browse(browser) if browser.mode() == BrowserMode::Save => match code {
                KeyCode::Down => browser.picker.step(1),
                KeyCode::Up => browser.picker.step(-1),
                KeyCode::Backspace => browser.picker.rubbed_out(),
                KeyCode::Esc => self.overlay = None,
                // A folder walks; a file offers itself to save over.
                KeyCode::Enter => {
                    if let Some(path) = browser.taken() {
                        self.prompt_save_name(
                            path.parent().map_or_else(PathBuf::new, Path::to_path_buf),
                            path.file_name()
                                .map(|name| name.to_string_lossy().into_owned()),
                        );
                    }
                }
                KeyCode::Char('s') => {
                    let at = browser.at().to_path_buf();
                    self.prompt_save_name(at, None);
                }
                KeyCode::Char(letter) => browser.picker.typing(letter),
                _ => {}
            },
            Overlay::Browse(browser) => match code {
                KeyCode::Down => browser.picker.step(1),
                KeyCode::Up => browser.picker.step(-1),
                KeyCode::Backspace => browser.picker.rubbed_out(),
                KeyCode::Esc => self.overlay = None,
                KeyCode::Enter => {
                    // A folder walks, and only a file ends the walk.
                    if let Some(path) = browser.taken() {
                        self.overlay = None;
                        self.open(&path);
                    }
                }
                KeyCode::Char(letter) => browser.picker.typing(letter),
                _ => {}
            },
            Overlay::Pick(picker, purpose) => match code {
                KeyCode::Down => picker.step(1),
                KeyCode::Up => picker.step(-1),
                KeyCode::Backspace => picker.rubbed_out(),
                KeyCode::Esc => self.overlay = None,
                KeyCode::Enter => {
                    let taken = picker.taken();
                    let purpose = std::mem::replace(purpose, PickPurpose::DecodeAs);
                    self.overlay = None;
                    if let Some(name) = taken {
                        self.take_picked(purpose, name);
                    }
                }
                KeyCode::Char(letter) => picker.typing(letter),
                _ => {}
            },
            Overlay::EditText(edit) => match code {
                KeyCode::Backspace => {
                    edit.text.pop();
                }
                KeyCode::Esc => self.overlay = None,
                KeyCode::Enter => self.submit_edit(),
                KeyCode::Char(letter) => edit.text.push(letter),
                _ => {}
            },
            // Handled apart, before this match: it changes the model as it
            // goes, which reads awkwardly from inside a match already holding
            // the overlay itself borrowed.
            Overlay::Filter(_) | Overlay::Step(_) | Overlay::FrameField(_) => {}
        }
    }

    /// Applies a key to the filter editor, mutating the view's title and
    /// filter directly rather than through the overlay: the two live in
    /// different fields of `self`, and both need touching here.
    fn filter_key(&mut self, code: KeyCode) {
        let names: Vec<String> = self
            .session
            .connections
            .iter()
            .map(|(id, _)| id.0.clone())
            .collect();
        let Some(id) = self.monitor_id() else {
            self.overlay = None;
            return;
        };
        let Some(monitor) = self.session.monitors.get(&id) else {
            self.overlay = None;
            return;
        };
        let mut title = monitor.title.clone();
        let mut filter = monitor.filter.clone();

        let mut close = false;
        if let Some(Overlay::Filter(edit)) = &mut self.overlay {
            edit.handle(code, &names, &mut title, &mut filter, &mut close);
        }

        if let Some(monitor) = self.session.monitors.get_mut(&id) {
            monitor.title = title;
            monitor.filter = filter;
        }
        if close {
            self.overlay = None;
        }
    }

    /// Offers the definitions the read row could be, when there is a choice.
    fn pick_frame(&mut self) {
        let Some(seq) = self.monitor().and_then(|monitor| monitor.selected) else {
            return;
        };
        let Some(entry) = self.session.log.iter().find(|entry| entry.seq == seq) else {
            return;
        };
        let candidates: Vec<String> = self
            .session
            .frames
            .frames()
            .filter(|frame| frame.size() == entry.bytes.len())
            .map(|frame| frame.name.clone())
            .collect();
        if !candidates.is_empty() {
            self.overlay = Some(Overlay::Pick(
                Picker::new("Read as", candidates),
                PickPurpose::DecodeAs,
            ));
        }
    }

    /// Offers the disk, starting where the project on show sits.
    fn browse(&mut self) {
        let at = self
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .or_else(|| self.looked_in.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"));
        self.overlay = Some(Overlay::Browse(Browser::opening(at)));
    }

    /// Validates the connection form under focus and, on success, opens it.
    ///
    /// Left open on failure, with the reason in the form's own trouble line:
    /// the fields typed so far are worth keeping while it is fixed.
    fn submit_connection(&mut self) {
        let Some(Overlay::NewConnection(form)) = &mut self.overlay else {
            return;
        };
        let Some((id, config)) = form.submit(&self.session.connections) else {
            return;
        };
        let retry = form.auto_reconnect().then(RetryPolicy::standard);
        let autoconnect = form.autoconnect();
        self.engine.connect(id.clone(), config.clone(), retry);
        self.session.connections.push((
            id,
            ConnectionEntry {
                config,
                status: ConnectionStatus::Connecting,
                retry,
                autoconnect,
            },
        ));
        self.overlay = None;
    }

    /// The keys the connection list answers to, and whether it took this one.
    fn connecting(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Down | KeyCode::Char('j') => self.pick_connection(1),
            KeyCode::Up | KeyCode::Char('k') => self.pick_connection(-1),
            KeyCode::Enter => self.toggle_selected_connection(),
            KeyCode::Char('a') => self.toggle_selected_autoconnect(),
            KeyCode::Char('x') => self.remove_selected_connection(),
            _ => return false,
        }
        true
    }

    fn pick_connection(&mut self, delta: isize) {
        self.connection_at = moved(self.connection_at, delta, self.session.connections.len());
    }

    /// The connection a key would act on: the one under the cursor, or the
    /// first one when nothing has been chosen yet.
    fn selected_connection(&self) -> Option<sim_core::ConnectionId> {
        let at = self.connection_at.unwrap_or(0);
        self.session.connections.get(at).map(|(id, _)| id.clone())
    }

    /// Connects a disconnected link with the settings it already has, or
    /// disconnects one that is up.
    fn toggle_selected_connection(&mut self) {
        let Some(id) = self.selected_connection() else {
            return;
        };
        match self.session.status_of(&id) {
            Some(ConnectionStatus::Disconnected) => {
                if let Some((config, retry)) = self.session.begin_reconnect(&id) {
                    self.engine.connect(id, config, retry);
                }
            }
            Some(_) => self.engine.disconnect(id),
            None => {}
        }
    }

    fn toggle_selected_autoconnect(&mut self) {
        let Some(id) = self.selected_connection() else {
            return;
        };
        if let Some(entry) = self.session.connection_mut(&id) {
            entry.autoconnect = !entry.autoconnect;
        }
    }

    /// Removal only reaches a link that is down, the same as the window: one
    /// still up has to be told to stop before it can be forgotten.
    fn remove_selected_connection(&mut self) {
        let Some(id) = self.selected_connection() else {
            return;
        };
        match self.session.status_of(&id) {
            Some(ConnectionStatus::Disconnected) => self.session.remove_connection(&id),
            Some(_) => {
                self.session.last_error = Some(format!(
                    "{}: disconnect it first (Enter), then remove it.",
                    id.0
                ));
            }
            None => {}
        }
    }

    /// The keys the Frames view answers to, and whether it took this one.
    fn framing(&mut self, code: KeyCode) -> bool {
        if self.session.frames.draft.is_some() {
            return self.editing_frame(code);
        }
        match code {
            KeyCode::Char('n') => {
                let name = self.session.frames.unused_frame_name("New frame");
                self.session
                    .frames
                    .begin_new(sim_session::frames::blank_frame(&name));
                self.frame_row = Some(0);
            }
            KeyCode::Char('e') => {
                self.session.frames.begin_edit();
                self.frame_row = Some(0);
            }
            KeyCode::Char('d') => {
                if let Err(error) = self.session.frames.delete_selected() {
                    self.session.last_error = Some(format!("{error:#}"));
                }
            }
            KeyCode::Char('r') => self.session.frames.reload(),
            KeyCode::Left => self.frame_focus = FramesFocus::Library,
            KeyCode::Right if self.session.frames.selected_frame().is_some() => {
                self.frame_focus = FramesFocus::Fields;
            }
            KeyCode::Down | KeyCode::Char('j') => self.frame_move(1),
            KeyCode::Up | KeyCode::Char('k') => self.frame_move(-1),
            KeyCode::Char(' ') if self.frame_focus == FramesFocus::Fields => self.toggle_bit(),
            KeyCode::Enter => match self.frame_focus {
                FramesFocus::Library => {
                    if self.session.frames.selected_frame().is_some() {
                        self.frame_focus = FramesFocus::Fields;
                    }
                }
                FramesFocus::Fields => self.edit_selected_field(),
            },
            KeyCode::Char('s') => self.send_selected_frame(),
            KeyCode::Char('t') => self.pick_frame_target(),
            _ => return false,
        }
        true
    }

    /// The keys the frame structure editor answers to while a draft is open.
    fn editing_frame(&mut self, code: KeyCode) -> bool {
        if matches!(self.overlay, Some(Overlay::FrameField(_))) {
            self.frame_field_key(code);
            return true;
        }
        match code {
            KeyCode::Down | KeyCode::Char('j') => self.move_frame_row(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_frame_row(-1),
            KeyCode::Enter => self.enter_frame_row(),
            KeyCode::Left => self.cycle_frame_row(-1),
            KeyCode::Right => self.cycle_frame_row(1),
            KeyCode::Char('a') => self.add_frame_field(),
            KeyCode::Char('x') => self.remove_frame_field(),
            KeyCode::Char('[') => self.move_frame_field(false),
            KeyCode::Char(']') => self.move_frame_field(true),
            KeyCode::Char('s') => sim_session::frames::save_draft(&mut self.session),
            KeyCode::Esc => self.session.frames.cancel_edit(),
            _ => return false,
        }
        true
    }

    fn frame_row_count(&self) -> usize {
        self.session
            .frames
            .draft
            .as_ref()
            .map_or(0, |draft| frame_rows(&draft.frame).len())
    }

    fn move_frame_row(&mut self, delta: isize) {
        self.frame_row = moved(self.frame_row, delta, self.frame_row_count());
    }

    fn enter_frame_row(&mut self) {
        let Some(draft) = &self.session.frames.draft else {
            return;
        };
        let rows = frame_rows(&draft.frame);
        let Some(row) = self.frame_row.and_then(|at| rows.get(at)) else {
            return;
        };
        match row {
            FrameRow::Name => {
                self.overlay = Some(Overlay::EditText(EditBox {
                    title: "Name".to_owned(),
                    text: draft.frame.name.clone(),
                    target: EditTarget::FrameName,
                }));
            }
            FrameRow::Endian => {}
            FrameRow::Field(index) => {
                self.overlay = Some(Overlay::FrameField(FrameFieldEdit::opening(*index)));
            }
        }
    }

    /// Changes the field under the cursor: the frame's byte order, when it is
    /// the header row selected.
    fn cycle_frame_row(&mut self, delta: isize) {
        let Some(draft) = &self.session.frames.draft else {
            return;
        };
        let rows = frame_rows(&draft.frame);
        if !matches!(
            self.frame_row.and_then(|at| rows.get(at)),
            Some(FrameRow::Endian)
        ) {
            return;
        }
        let Some(draft) = self.session.frames.draft.as_mut() else {
            return;
        };
        let next = crate::connection_form::cycle(
            &[
                sim_core::frame::Endianness::Big,
                sim_core::frame::Endianness::Little,
            ],
            draft.frame.endian,
            delta,
        );
        sim_session::layout::set_endian(&mut draft.frame, next);
    }

    fn add_frame_field(&mut self) {
        let Some(draft) = self.session.frames.draft.as_mut() else {
            return;
        };
        let endian = draft.frame.endian;
        sim_session::layout::add_field(&mut draft.frame, None, kinds::blank_field(endian));
        self.frame_row = Some(frame_rows(&draft.frame).len() - 1);
    }

    fn selected_frame_field_index(&self) -> Option<usize> {
        let draft = self.session.frames.draft.as_ref()?;
        let rows = frame_rows(&draft.frame);
        match rows.get(self.frame_row?)? {
            FrameRow::Field(index) => Some(*index),
            _ => None,
        }
    }

    fn remove_frame_field(&mut self) {
        let Some(index) = self.selected_frame_field_index() else {
            return;
        };
        let Some(draft) = self.session.frames.draft.as_mut() else {
            return;
        };
        if !sim_session::layout::may_remove(&draft.frame, index) {
            self.session.last_error =
                Some("Removing this field would leave a checksum covering nothing.".to_owned());
            return;
        }
        sim_session::layout::remove_field(&mut draft.frame, index);
    }

    fn move_frame_field(&mut self, down: bool) {
        let Some(index) = self.selected_frame_field_index() else {
            return;
        };
        if let Some(draft) = self.session.frames.draft.as_mut() {
            sim_session::layout::move_field(&mut draft.frame, index, down);
        }
        let at = self.frame_row.unwrap_or(0);
        self.frame_row = Some(if down { at + 1 } else { at.saturating_sub(1) });
    }

    /// Applies a key to the field editor, mutating the live draft directly:
    /// the same reasoning as [`Self::filter_key`], since the overlay and the
    /// session are different fields of `self`.
    fn frame_field_key(&mut self, code: KeyCode) {
        let Some(Overlay::FrameField(edit)) = &self.overlay else {
            return;
        };
        let field_index = edit.field_index();
        let mut focus = edit.focus();

        let Some(draft) = self.session.frames.draft.as_ref() else {
            self.overlay = None;
            return;
        };
        let Some(field) = draft.frame.fields.get(field_index).cloned() else {
            self.overlay = None;
            return;
        };
        let held = FrameFieldEdit::fields(&field).len().max(1);

        let mut close = false;
        match code {
            KeyCode::Esc => close = true,
            KeyCode::Tab | KeyCode::Down => focus = (focus + 1) % held,
            KeyCode::BackTab | KeyCode::Up => focus = (focus + held - 1) % held,
            _ => self.act_on_frame_field(field_index, focus, code),
        }

        if close {
            self.overlay = None;
        } else if matches!(self.overlay, Some(Overlay::FrameField(_))) {
            // Left as it was unless the action above opened its own overlay
            // (a rename or a variant/bit box), which must not be clobbered.
            self.overlay = Some(Overlay::FrameField(FrameFieldEdit {
                field: field_index,
                focus,
            }));
        }
    }

    fn act_on_frame_field(&mut self, field_index: usize, focus: usize, code: KeyCode) {
        let Some(draft) = self.session.frames.draft.as_ref() else {
            return;
        };
        let Some(field) = draft.frame.fields.get(field_index).cloned() else {
            return;
        };
        let rows = FrameFieldEdit::fields(&field);
        let Some(row) = rows.get(focus.min(rows.len().saturating_sub(1))) else {
            return;
        };

        match row {
            FrameFieldRow::Name => {
                if matches!(code, KeyCode::Enter) {
                    self.overlay_edit_frame_field_name(field_index, &field.name);
                }
            }
            FrameFieldRow::Kind => {
                let Some(delta) = arrow_delta(code) else {
                    return;
                };
                self.cycle_frame_field_kind(field_index, &field, delta);
            }
            FrameFieldRow::Length => {
                if matches!(code, KeyCode::Enter) {
                    self.overlay_edit_frame_field_length(field_index, &field.kind);
                }
            }
            FrameFieldRow::Repr => {
                let Some(delta) = arrow_delta(code) else {
                    return;
                };
                self.cycle_frame_field_repr(field_index, &field.kind, delta);
            }
            FrameFieldRow::Variant(i) => match code {
                KeyCode::Enter => self.overlay_edit_frame_variant(field_index, *i, &field.kind),
                KeyCode::Char('a') => self.add_frame_variant(field_index),
                KeyCode::Char('x') => self.remove_frame_variant(field_index, *i),
                _ => {}
            },
            FrameFieldRow::Bit(i) => match code {
                KeyCode::Enter => self.overlay_edit_frame_bit(field_index, *i, &field.kind),
                KeyCode::Char('a') => self.add_frame_bit(field_index),
                KeyCode::Char('x') => self.remove_frame_bit(field_index, *i),
                KeyCode::Char('r') => self.reverse_frame_bits(field_index),
                _ => {}
            },
            FrameFieldRow::CoversFrom | FrameFieldRow::CoversTo => {
                let Some(delta) = arrow_delta(code) else {
                    return;
                };
                self.cycle_frame_coverage(
                    field_index,
                    matches!(row, FrameFieldRow::CoversFrom),
                    delta,
                );
            }
        }
    }
    fn overlay_edit_frame_field_name(&mut self, field_index: usize, current: &str) {
        self.overlay = Some(Overlay::EditText(EditBox {
            title: "Field name".to_owned(),
            text: current.to_owned(),
            target: EditTarget::FrameFieldName(field_index),
        }));
    }

    fn overlay_edit_frame_field_length(&mut self, field_index: usize, kind: &FieldKind) {
        let len = match kind {
            FieldKind::Bytes { len } | FieldKind::Text { len } => *len,
            _ => return,
        };
        self.overlay = Some(Overlay::EditText(EditBox {
            title: "Length".to_owned(),
            text: len.to_string(),
            target: EditTarget::FrameFieldLength(field_index),
        }));
    }

    fn overlay_edit_frame_variant(&mut self, field_index: usize, variant: usize, kind: &FieldKind) {
        let FieldKind::Enum { variants, .. } = kind else {
            return;
        };
        let Some(current) = variants.get(variant) else {
            return;
        };
        self.overlay = Some(Overlay::EditText(EditBox {
            title: "Variant: NAME = VALUE".to_owned(),
            text: format!("{} = {}", current.name, current.value),
            target: EditTarget::FrameVariant {
                field: field_index,
                variant,
            },
        }));
    }

    fn overlay_edit_frame_bit(&mut self, field_index: usize, bit: usize, kind: &FieldKind) {
        let FieldKind::Bits { bits, .. } = kind else {
            return;
        };
        let Some(current) = bits.get(bit) else {
            return;
        };
        self.overlay = Some(Overlay::EditText(EditBox {
            title: "Bit: NAME WIDTH".to_owned(),
            text: format!("{} {}", current.name, current.width),
            target: EditTarget::FrameBit {
                field: field_index,
                bit,
            },
        }));
    }

    fn cycle_frame_field_kind(&mut self, field_index: usize, field: &FieldDef, delta: isize) {
        let labels = kinds::labels();
        let current = kinds::label_of(&field.kind);
        let Some(at) = labels.iter().position(|label| *label == current) else {
            return;
        };
        let next = crate::connection_form::cycle(&(0..labels.len()).collect::<Vec<_>>(), at, delta);
        let Some(draft) = self.session.frames.draft.as_ref() else {
            return;
        };
        let Some(kind) = kinds::named(&labels[next], &draft.frame, field_index) else {
            return;
        };
        let Some(draft) = self.session.frames.draft.as_mut() else {
            return;
        };
        if let Some(field) = sim_session::layout::plain_field_mut(&mut draft.frame, field_index) {
            field.kind = kind;
        }
    }

    fn cycle_frame_field_repr(&mut self, field_index: usize, kind: &FieldKind, delta: isize) {
        let repr = match kind {
            FieldKind::Enum { repr, .. } | FieldKind::Bits { repr, .. } => *repr,
            _ => return,
        };
        let next =
            crate::connection_form::cycle(&sim_core::frame::ScalarType::UNSIGNED, repr, delta);
        let Some(draft) = self.session.frames.draft.as_mut() else {
            return;
        };
        let Some(field) = sim_session::layout::plain_field_mut(&mut draft.frame, field_index)
        else {
            return;
        };
        match &mut field.kind {
            FieldKind::Enum { repr, .. } | FieldKind::Bits { repr, .. } => *repr = next,
            _ => {}
        }
    }

    fn add_frame_variant(&mut self, field_index: usize) {
        let Some(draft) = self.session.frames.draft.as_mut() else {
            return;
        };
        let Some(field) = sim_session::layout::plain_field_mut(&mut draft.frame, field_index)
        else {
            return;
        };
        if let FieldKind::Enum { variants, .. } = &mut field.kind {
            let next = variants.iter().map(|v| v.value).max().map_or(0, |v| v + 1);
            variants.push(sim_core::frame::EnumVariant {
                name: format!("VALUE{next}"),
                value: next,
            });
        }
    }

    fn remove_frame_variant(&mut self, field_index: usize, variant: usize) {
        let Some(draft) = self.session.frames.draft.as_mut() else {
            return;
        };
        let Some(field) = sim_session::layout::plain_field_mut(&mut draft.frame, field_index)
        else {
            return;
        };
        if let FieldKind::Enum { variants, .. } = &mut field.kind {
            if variants.len() > 1 && variant < variants.len() {
                variants.remove(variant);
            }
        }
    }

    fn add_frame_bit(&mut self, field_index: usize) {
        let Some(draft) = self.session.frames.draft.as_mut() else {
            return;
        };
        let Some(field) = sim_session::layout::plain_field_mut(&mut draft.frame, field_index)
        else {
            return;
        };
        if let FieldKind::Bits { bits, .. } = &mut field.kind {
            bits.push(sim_core::frame::BitDef {
                name: format!("bit{}", bits.len()),
                width: 1,
            });
        }
    }

    fn remove_frame_bit(&mut self, field_index: usize, bit: usize) {
        let Some(draft) = self.session.frames.draft.as_mut() else {
            return;
        };
        let Some(field) = sim_session::layout::plain_field_mut(&mut draft.frame, field_index)
        else {
            return;
        };
        if let FieldKind::Bits { bits, .. } = &mut field.kind {
            if bits.len() > 1 && bit < bits.len() {
                bits.remove(bit);
            }
        }
    }

    /// Swaps most-significant-first for least-significant-first, so a bitfield
    /// entered backwards can be fixed in place rather than retyped bit by bit.
    fn reverse_frame_bits(&mut self, field_index: usize) {
        let Some(draft) = self.session.frames.draft.as_mut() else {
            return;
        };
        let Some(field) = sim_session::layout::plain_field_mut(&mut draft.frame, field_index)
        else {
            return;
        };
        if let FieldKind::Bits { bits, .. } = &mut field.kind {
            bits.reverse();
        }
    }

    fn cycle_frame_coverage(&mut self, field_index: usize, from_end: bool, delta: isize) {
        let Some(draft) = self.session.frames.draft.as_ref() else {
            return;
        };
        let Some(field) = draft.frame.fields.get(field_index) else {
            return;
        };
        let FieldKind::Checksum { covers, .. } = &field.kind else {
            return;
        };
        let names: Vec<String> = draft.frame.fields.iter().map(|f| f.name.clone()).collect();
        if names.is_empty() {
            return;
        }
        let current_at = if from_end { covers.from } else { covers.to };
        let current_name = names.get(current_at).cloned().unwrap_or_default();
        let indices: Vec<usize> = (0..names.len()).collect();
        let current_index = indices
            .iter()
            .position(|i| names[*i] == current_name)
            .unwrap_or(0);
        let next_index = crate::connection_form::cycle(&indices, current_index, delta);
        let next_name = names[next_index].clone();
        let other_name = if from_end {
            names.get(covers.to).cloned().unwrap_or_default()
        } else {
            names.get(covers.from).cloned().unwrap_or_default()
        };

        let Some(draft) = self.session.frames.draft.as_mut() else {
            return;
        };
        let (from, to) = if from_end {
            (next_name.as_str(), other_name.as_str())
        } else {
            (other_name.as_str(), next_name.as_str())
        };
        sim_session::layout::set_coverage(&mut draft.frame, field_index, from, to);
    }

    /// Offers the connections currently up, to choose which one a frame goes
    /// out on.
    fn pick_frame_target(&mut self) {
        let connected: Vec<String> = self
            .session
            .connections
            .iter()
            .filter(|(_, entry)| entry.status == ConnectionStatus::Connected)
            .map(|(id, _)| id.0.clone())
            .collect();
        if connected.is_empty() {
            self.session.last_error = Some("No connected link to send to.".to_owned());
            return;
        }
        self.overlay = Some(Overlay::Pick(
            Picker::new("Target connection", connected),
            PickPurpose::FrameTarget,
        ));
    }

    /// Offers the connections currently up, to choose which one hand-typed
    /// hex bytes go out on.
    fn pick_hex_target(&mut self) {
        let connected: Vec<String> = self
            .session
            .connections
            .iter()
            .filter(|(_, entry)| entry.status == ConnectionStatus::Connected)
            .map(|(id, _)| id.0.clone())
            .collect();
        if connected.is_empty() {
            self.session.last_error = Some("No connected link to send to.".to_owned());
            return;
        }
        self.overlay = Some(Overlay::Pick(
            Picker::new("Target connection", connected),
            PickPurpose::HexTarget,
        ));
    }

    fn frame_move(&mut self, delta: isize) {
        match self.frame_focus {
            FramesFocus::Library => self.pick_frame_in_library(delta),
            FramesFocus::Fields => self.pick_field(delta),
        }
    }

    fn pick_frame_in_library(&mut self, delta: isize) {
        self.session.frames.selected = moved(
            self.session.frames.selected,
            delta,
            self.session.frames.entries.len(),
        );
        // Whatever the note said, it said it about another frame.
        self.session.frame_hex_note = None;
    }

    /// The rows of the frame on show, resetting the cursor when it is not the
    /// frame the cursor was last on.
    fn field_frame_rows(&mut self) -> Option<(sim_core::frame::FrameDef, Vec<FieldRow>)> {
        let frame = self.session.frames.selected_frame()?.clone();
        let rows = field_rows(&frame);
        let fresh = self
            .field_at
            .as_ref()
            .is_none_or(|(name, _)| *name != frame.name);
        if fresh {
            self.field_at = Some((frame.name.clone(), 0));
        }
        Some((frame, rows))
    }

    fn pick_field(&mut self, delta: isize) {
        let Some((frame, rows)) = self.field_frame_rows() else {
            return;
        };
        let current = self.field_at.as_ref().map(|(_, at)| *at);
        let Some(at) = moved(current, delta, rows.len()) else {
            return;
        };
        self.field_at = Some((frame.name, at));
    }

    /// Flips a single-bit flag under the cursor without going through an
    /// editor: there is nothing to type for a value that is only ever 0 or 1.
    fn toggle_bit(&mut self) {
        let Some((frame, rows)) = self.field_frame_rows() else {
            return;
        };
        let Some(&FieldRow::Bit { field, bit }) =
            self.field_at.as_ref().and_then(|(_, at)| rows.get(*at))
        else {
            return;
        };
        let sim_core::frame::FieldKind::Bits { bits, .. } = &frame.fields[field].kind else {
            return;
        };
        if bits[bit].width != 1 {
            return;
        }
        let name = bits[bit].name.clone();
        let values = self.session.frames.values_mut(&frame);
        let held = values
            .entry(frame.fields[field].name.clone())
            .or_insert_with(|| {
                sim_core::frame::value::Value::Bits(std::collections::BTreeMap::new())
            });
        if let sim_core::frame::value::Value::Bits(set) = held {
            let slot = set.entry(name).or_insert(0);
            *slot = u64::from(*slot == 0);
        }
    }

    /// Opens the right editor for the row under the cursor: a list for an
    /// enum, a toggle already done for a one-bit flag, a box to type into for
    /// everything else. A checksum is computed, not edited.
    fn edit_selected_field(&mut self) {
        let Some((frame, rows)) = self.field_frame_rows() else {
            return;
        };
        let Some(&row) = self.field_at.as_ref().and_then(|(_, at)| rows.get(*at)) else {
            return;
        };

        match row {
            FieldRow::Field(index) => {
                let field = &frame.fields[index];
                match &field.kind {
                    sim_core::frame::FieldKind::Checksum { .. } => {
                        self.session.last_error =
                            Some("Computed automatically, on send.".to_owned());
                    }
                    sim_core::frame::FieldKind::Enum { variants, .. } => {
                        let options: Vec<String> = variants
                            .iter()
                            .map(|variant| format!("{} = {}", variant.name, variant.value))
                            .collect();
                        self.overlay = Some(Overlay::Pick(
                            Picker::new(field.name.clone(), options),
                            PickPurpose::EnumField { field: index },
                        ));
                    }
                    _ => {
                        let values = self.session.frames.values_mut(&frame);
                        let current = values.get(&field.name).cloned();
                        let text = current.map_or_else(String::new, |value| match &field.kind {
                            sim_core::frame::FieldKind::Bytes { .. } => {
                                value.as_bytes().map_or_else(String::new, hex::packed)
                            }
                            sim_core::frame::FieldKind::Text { .. } => {
                                value.as_text().unwrap_or_default().to_owned()
                            }
                            _ => reading::describe(field, &value, false),
                        });
                        self.overlay = Some(Overlay::EditText(EditBox {
                            title: field.name.clone(),
                            text,
                            target: EditTarget::Field(index),
                        }));
                    }
                }
            }
            FieldRow::Bit { field, bit } => {
                let sim_core::frame::FieldKind::Bits { bits, .. } = &frame.fields[field].kind
                else {
                    return;
                };
                if bits[bit].width == 1 {
                    self.toggle_bit();
                    return;
                }
                let values = self.session.frames.values_mut(&frame);
                let held = values
                    .get(&frame.fields[field].name)
                    .and_then(sim_core::frame::value::Value::as_bits)
                    .and_then(|set| set.get(&bits[bit].name))
                    .copied()
                    .unwrap_or(0);
                self.overlay = Some(Overlay::EditText(EditBox {
                    title: bits[bit].name.clone(),
                    text: held.to_string(),
                    target: EditTarget::Bit { field, bit },
                }));
            }
        }
    }

    /// Applies what a picker settled: which frame a row reads as, or which
    /// enum variant a field now holds.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "consumed by one arm and not the other, moving it in either way"
    )]
    fn take_picked(&mut self, purpose: PickPurpose, taken: String) {
        match purpose {
            PickPurpose::DecodeAs => {
                if let Some(id) = self.monitor_id() {
                    if let Some(monitor) = self.session.monitors.get_mut(&id) {
                        monitor.decode_as = Some(taken);
                    }
                }
                self.traffic_field_scroll = 0;
            }
            PickPurpose::EnumField { field } => {
                let Some(frame) = self.session.frames.selected_frame().cloned() else {
                    return;
                };
                // The label carries the value after " = ", which is the part
                // that means something to the encoder.
                let Some(value) = taken.rsplit(" = ").next().and_then(|v| v.parse().ok()) else {
                    return;
                };
                self.session.frames.values_mut(&frame).insert(
                    frame.fields[field].name.clone(),
                    sim_core::frame::value::Value::Uint(value),
                );
            }
            PickPurpose::FrameTarget => {
                self.session.frame_target = Some(sim_core::ConnectionId(taken));
            }
            PickPurpose::HexTarget => {
                self.session.hex_target = Some(sim_core::ConnectionId(taken));
            }
        }
    }

    /// Applies an edit that belongs to the scenario header rather than to a
    /// frame field. Returns whether the target was one of those.
    /// Where a text edit nested inside the field editor should land once it
    /// is applied: back on the same field's popup, at the row it came from,
    /// rather than all the way out to the frame's own field list.
    fn reopened_frame_field(&self, target: &EditTarget) -> Option<Overlay> {
        let (field_index, focus) = match *target {
            EditTarget::FrameFieldName(index) => (index, Some(FrameFieldRow::Name)),
            EditTarget::FrameFieldLength(index) => (index, Some(FrameFieldRow::Length)),
            EditTarget::FrameVariant { field, variant } => {
                (field, Some(FrameFieldRow::Variant(variant)))
            }
            EditTarget::FrameBit { field, bit } => (field, Some(FrameFieldRow::Bit(bit))),
            _ => return None,
        };
        let draft = self.session.frames.draft.as_ref()?;
        let field = draft.frame.fields.get(field_index)?;
        let rows = FrameFieldEdit::fields(field);
        let focus = focus.and_then(|row| rows.iter().position(|held| *held == row))?;
        Some(Overlay::FrameField(FrameFieldEdit {
            field: field_index,
            focus,
        }))
    }

    /// Applies an edit that belongs to a frame definition rather than to a
    /// value being sent. Returns whether the target was one of those.
    fn apply_frame_edit(&mut self, target: &EditTarget, text: &str) -> bool {
        let Some(draft) = self.session.frames.draft.as_mut() else {
            return matches!(
                target,
                EditTarget::FrameName
                    | EditTarget::FrameFieldName(_)
                    | EditTarget::FrameFieldLength(_)
                    | EditTarget::FrameVariant { .. }
                    | EditTarget::FrameBit { .. }
            );
        };
        match target {
            EditTarget::FrameName => text.clone_into(&mut draft.frame.name),
            EditTarget::FrameFieldName(index) => {
                sim_session::layout::rename_field(&mut draft.frame, *index, text.trim());
            }
            EditTarget::FrameFieldLength(index) => {
                let Ok(len) = text.trim().parse::<usize>() else {
                    return true;
                };
                if let Some(field) = sim_session::layout::plain_field_mut(&mut draft.frame, *index)
                {
                    match &mut field.kind {
                        FieldKind::Bytes { len: held } | FieldKind::Text { len: held } => {
                            *held = len.max(1);
                        }
                        _ => {}
                    }
                }
            }
            EditTarget::FrameVariant { field, variant } => {
                let Some((name, value)) = text.split_once('=') else {
                    return true;
                };
                let Ok(value) = value.trim().parse::<u64>() else {
                    return true;
                };
                if let Some(field) = sim_session::layout::plain_field_mut(&mut draft.frame, *field)
                {
                    if let FieldKind::Enum { variants, .. } = &mut field.kind {
                        if let Some(held) = variants.get_mut(*variant) {
                            name.trim().clone_into(&mut held.name);
                            held.value = value;
                        }
                    }
                }
            }
            EditTarget::FrameBit { field, bit } => {
                let Some((name, width)) = text.trim().rsplit_once(' ') else {
                    return true;
                };
                let Ok(width) = width.trim().parse::<u32>() else {
                    return true;
                };
                if let Some(field) = sim_session::layout::plain_field_mut(&mut draft.frame, *field)
                {
                    if let FieldKind::Bits { bits, .. } = &mut field.kind {
                        if let Some(held) = bits.get_mut(*bit) {
                            name.trim().clone_into(&mut held.name);
                            held.width = width.max(1);
                        }
                    }
                }
            }
            EditTarget::Field(_)
            | EditTarget::Bit { .. }
            | EditTarget::ScenarioName
            | EditTarget::ScenarioDescription
            | EditTarget::RepeatEvery
            | EditTarget::RepeatTimes
            | EditTarget::SaveFileName(_) => return false,
        }
        true
    }

    fn apply_scenario_edit(&mut self, target: &EditTarget, text: &str) -> bool {
        let Some(draft) = self.session.scenarios.draft.as_mut() else {
            return matches!(
                target,
                EditTarget::ScenarioName
                    | EditTarget::ScenarioDescription
                    | EditTarget::RepeatEvery
                    | EditTarget::RepeatTimes
            );
        };
        match target {
            EditTarget::ScenarioName => text.clone_into(&mut draft.scenario.name),
            EditTarget::ScenarioDescription => {
                draft.scenario.description = (!text.trim().is_empty()).then(|| text.to_owned());
            }
            EditTarget::RepeatEvery => {
                let millis: u64 = text.parse().unwrap_or(100).max(1);
                let repeat = draft
                    .scenario
                    .repeat
                    .get_or_insert(sim_core::scenario::Repeat {
                        every: Duration::from_millis(100),
                        times: None,
                    });
                repeat.every = Duration::from_millis(millis);
            }
            EditTarget::RepeatTimes => {
                if let Some(repeat) = &mut draft.scenario.repeat {
                    repeat.times = text.parse().ok();
                }
            }
            EditTarget::Field(_)
            | EditTarget::Bit { .. }
            | EditTarget::SaveFileName(_)
            | EditTarget::FrameName
            | EditTarget::FrameFieldName(_)
            | EditTarget::FrameFieldLength(_)
            | EditTarget::FrameVariant { .. }
            | EditTarget::FrameBit { .. } => return false,
        }
        true
    }

    /// Reads what was typed into the box on show, and writes it into the
    /// field or flag it belongs to.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a typed number is clamped to the field's own width by the encoder, \
                  which is a truer check than one done here on the way in"
    )]
    fn submit_edit(&mut self) {
        let Some(Overlay::EditText(edit)) = &self.overlay else {
            return;
        };
        let text = edit.text.clone();
        let target = edit.target.clone();

        if let EditTarget::SaveFileName(directory) = &target {
            self.overlay = None;
            if !text.trim().is_empty() {
                self.save_to(&directory.join(text.trim()));
            }
            return;
        }

        if self.apply_frame_edit(&target, &text) {
            self.overlay = self.reopened_frame_field(&target);
            return;
        }

        if self.apply_scenario_edit(&target, &text) {
            self.overlay = None;
            return;
        }

        let Some(frame) = self.session.frames.selected_frame().cloned() else {
            self.overlay = None;
            return;
        };

        match target {
            EditTarget::Field(index) => {
                let field = frame.fields[index].clone();
                let written = match &field.kind {
                    sim_core::frame::FieldKind::Scalar(scalar) if scalar.is_unsigned_integer() => {
                        hex::read_number(&text)
                            .map(|value| sim_core::frame::value::Value::Uint(value.max(0.0) as u64))
                    }
                    sim_core::frame::FieldKind::Scalar(
                        sim_core::frame::ScalarType::F32 | sim_core::frame::ScalarType::F64,
                    ) => hex::read_number(&text).map(sim_core::frame::value::Value::Float),
                    sim_core::frame::FieldKind::Scalar(_) => hex::read_number(&text)
                        .map(|value| sim_core::frame::value::Value::Int(value as i64)),
                    sim_core::frame::FieldKind::Bytes { len } => {
                        hex::parse(&text).ok().map(|mut bytes| {
                            bytes.resize(*len, 0);
                            sim_core::frame::value::Value::Bytes(bytes)
                        })
                    }
                    sim_core::frame::FieldKind::Text { len } => {
                        let mut text = text.clone();
                        text.truncate(*len);
                        Some(sim_core::frame::value::Value::Text(text))
                    }
                    sim_core::frame::FieldKind::Enum { .. }
                    | sim_core::frame::FieldKind::Bits { .. }
                    | sim_core::frame::FieldKind::Checksum { .. } => None,
                };
                if let Some(value) = written {
                    self.session
                        .frames
                        .values_mut(&frame)
                        .insert(field.name, value);
                }
            }
            EditTarget::Bit { field, bit } => {
                let sim_core::frame::FieldKind::Bits { bits, .. } = &frame.fields[field].kind
                else {
                    self.overlay = None;
                    return;
                };
                if let Some(value) = hex::read_number(&text) {
                    let name = bits[bit].name.clone();
                    let values = self.session.frames.values_mut(&frame);
                    let held = values
                        .entry(frame.fields[field].name.clone())
                        .or_insert_with(|| {
                            sim_core::frame::value::Value::Bits(std::collections::BTreeMap::new())
                        });
                    if let sim_core::frame::value::Value::Bits(set) = held {
                        set.insert(name, value.max(0.0) as u64);
                    }
                }
            }
            EditTarget::ScenarioName
            | EditTarget::ScenarioDescription
            | EditTarget::RepeatEvery
            | EditTarget::RepeatTimes
            | EditTarget::SaveFileName(_)
            | EditTarget::FrameName
            | EditTarget::FrameFieldName(_)
            | EditTarget::FrameFieldLength(_)
            | EditTarget::FrameVariant { .. }
            | EditTarget::FrameBit { .. } => unreachable!("handled and returned above"),
        }
        self.overlay = None;
    }

    /// Encodes the chosen frame from the values on show and sends it.
    ///
    /// Failing to encode is reported rather than sent as whatever fell out: a
    /// frame the definition refuses is not a frame the receiver asked for.
    fn send_selected_frame(&mut self) {
        let Some(frame) = self.session.frames.selected_frame().cloned() else {
            return;
        };
        let Some(id) = self.session.frame_target.clone() else {
            self.session.last_error = Some("No target chosen. Press t to pick one.".to_owned());
            return;
        };
        if self.session.status_of(&id) != Some(ConnectionStatus::Connected) {
            self.session.last_error = Some(format!("{} is not connected.", id.0));
            return;
        }

        let values = self.session.frames.values_mut(&frame).clone();
        match codec::encode(&frame, &values) {
            Ok(bytes) => {
                self.status = Some(format!("Sent {} byte(s) to {}.", bytes.len(), id.0));
                self.engine.send_raw(id, bytes);
            }
            Err(error) => self.session.last_error = Some(error.to_string()),
        }
    }

    /// Whether a box has the keyboard.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// What the focused box does with a key. It takes all of them.
    fn typing(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char(letter) => self.session.hex_input.push(letter),
            KeyCode::Backspace => {
                self.session.hex_input.pop();
            }
            KeyCode::Esc => self.editing = false,
            KeyCode::Enter => {
                self.inject();
                self.editing = false;
            }
            _ => {}
        }
    }

    /// The keys the injection view answers to while nothing is being typed.
    fn injecting(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Enter | KeyCode::Char('i') => self.editing = true,
            KeyCode::Char('x') => self.session.hex_input.clear(),
            KeyCode::Char('t') => self.pick_hex_target(),
            _ => return false,
        }
        true
    }

    /// Sends what is typed, to the link the project named.
    fn inject(&mut self) {
        let Ok(bytes) = hex::parse(&self.session.hex_input) else {
            return;
        };
        let Some(id) = self
            .session
            .hex_target
            .clone()
            .or_else(|| self.session.connections.first().map(|(id, _)| id.clone()))
        else {
            self.session.last_error = Some("No link to send on.".to_owned());
            return;
        };
        self.engine.send_raw(id, bytes);
    }

    /// The keys the scenario list answers to, and whether it took this one.
    /// The keys the scenario view answers to, and whether it took this one.
    fn running_scenarios(&mut self, code: KeyCode) -> bool {
        if self.session.scenarios.draft.is_some() {
            return self.editing_scenario(code);
        }
        match code {
            KeyCode::Down | KeyCode::Char('j') => self.pick_scenario(1),
            KeyCode::Up | KeyCode::Char('k') => self.pick_scenario(-1),
            KeyCode::Enter => self.start_selected(),
            KeyCode::Char('x') => self.stop_selected(),
            KeyCode::Char('n') => {
                self.session.scenarios.begin_new(scenarios::blank());
                self.scenario_row = Some(0);
            }
            KeyCode::Char('e') => {
                let running = self.selected_scenario_running();
                if running {
                    self.session.last_error = Some("Stop it before editing it.".to_owned());
                } else {
                    self.session.scenarios.begin_edit();
                    self.scenario_row = Some(0);
                }
            }
            KeyCode::Char('d') => {
                if self.selected_scenario_running() {
                    self.session.last_error = Some("Stop it before deleting it.".to_owned());
                } else if let Err(error) = self.session.scenarios.delete_selected() {
                    self.session.last_error = Some(format!("{error:#}"));
                }
            }
            KeyCode::Char('r') => self.session.scenarios.reload(),
            _ => return false,
        }
        true
    }

    fn selected_scenario_running(&self) -> bool {
        self.session
            .scenarios
            .selected_scenario()
            .is_some_and(|scenario| self.session.running.contains_key(&scenario.name))
    }

    /// The keys the scenario editor answers to while a draft is open.
    fn editing_scenario(&mut self, code: KeyCode) -> bool {
        if matches!(self.overlay, Some(Overlay::Step(_))) {
            self.step_key(code);
            return true;
        }
        match code {
            KeyCode::Down | KeyCode::Char('j') => self.move_scenario_row(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_scenario_row(-1),
            KeyCode::Enter => self.enter_scenario_row(),
            KeyCode::Char(' ') => self.toggle_scenario_row(),
            KeyCode::Char('a') => self.add_step(),
            KeyCode::Char('x') => self.remove_scenario_step(),
            KeyCode::Char('[') => self.move_scenario_step(false),
            KeyCode::Char(']') => self.move_scenario_step(true),
            KeyCode::Char('s') => scenarios::save(&mut self.session),
            KeyCode::Esc => self.session.scenarios.cancel_edit(),
            _ => return false,
        }
        true
    }

    fn scenario_row_count(&self) -> usize {
        self.session
            .scenarios
            .draft
            .as_ref()
            .map_or(0, |draft| scenario_rows(&draft.scenario).len())
    }

    fn move_scenario_row(&mut self, delta: isize) {
        self.scenario_row = moved(self.scenario_row, delta, self.scenario_row_count());
    }

    fn enter_scenario_row(&mut self) {
        let Some(draft) = &self.session.scenarios.draft else {
            return;
        };
        let rows = scenario_rows(&draft.scenario);
        let Some(row) = self.scenario_row.and_then(|at| rows.get(at)) else {
            return;
        };
        match row {
            ScenarioRow::Name => {
                self.overlay = Some(Overlay::EditText(EditBox {
                    title: "Name".to_owned(),
                    text: draft.scenario.name.clone(),
                    target: EditTarget::ScenarioName,
                }));
            }
            ScenarioRow::Description => {
                self.overlay = Some(Overlay::EditText(EditBox {
                    title: "Description".to_owned(),
                    text: draft.scenario.description.clone().unwrap_or_default(),
                    target: EditTarget::ScenarioDescription,
                }));
            }
            ScenarioRow::RepeatEvery => {
                let text = draft
                    .scenario
                    .repeat
                    .map_or_else(String::new, |repeat| repeat.every.as_millis().to_string());
                self.overlay = Some(Overlay::EditText(EditBox {
                    title: "Repeat every (ms)".to_owned(),
                    text,
                    target: EditTarget::RepeatEvery,
                }));
            }
            ScenarioRow::RepeatTimes => {
                let text = draft
                    .scenario
                    .repeat
                    .and_then(|repeat| repeat.times)
                    .map_or_else(String::new, |times| times.to_string());
                self.overlay = Some(Overlay::EditText(EditBox {
                    title: "Repeat times (blank: forever)".to_owned(),
                    text,
                    target: EditTarget::RepeatTimes,
                }));
            }
            ScenarioRow::Repeat => {}
            ScenarioRow::Step(index) => {
                self.overlay = Some(Overlay::Step(StepEdit::opening(*index)));
            }
        }
    }

    fn toggle_scenario_row(&mut self) {
        let Some(draft) = self.session.scenarios.draft.as_mut() else {
            return;
        };
        let rows = scenario_rows(&draft.scenario);
        if matches!(
            self.scenario_row.and_then(|at| rows.get(at)),
            Some(ScenarioRow::Repeat)
        ) {
            draft.scenario.repeat =
                draft
                    .scenario
                    .repeat
                    .is_none()
                    .then(|| sim_core::scenario::Repeat {
                        every: Duration::from_millis(100),
                        times: None,
                    });
        }
    }

    fn add_step(&mut self) {
        let links: Vec<sim_core::ConnectionId> = self
            .session
            .connections
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        let first_frame: Option<String> =
            self.session.frames.frames().next().map(|f| f.name.clone());
        if let Some(draft) = self.session.scenarios.draft.as_mut() {
            draft.add_step(&links, first_frame.as_deref());
            self.scenario_row = Some(scenario_rows(&draft.scenario).len() - 1);
        }
    }

    fn selected_step_index(&self) -> Option<usize> {
        let draft = self.session.scenarios.draft.as_ref()?;
        let rows = scenario_rows(&draft.scenario);
        match rows.get(self.scenario_row?)? {
            ScenarioRow::Step(index) => Some(*index),
            _ => None,
        }
    }

    fn remove_scenario_step(&mut self) {
        let Some(index) = self.selected_step_index() else {
            return;
        };
        if let Some(draft) = self.session.scenarios.draft.as_mut() {
            draft.remove_step(index);
        }
    }

    /// Moves the step under the cursor one place earlier or later.
    fn move_scenario_step(&mut self, down: bool) {
        let Some(index) = self.selected_step_index() else {
            return;
        };
        if let Some(draft) = self.session.scenarios.draft.as_mut() {
            draft.move_step(index, down);
        }
        let at = self.scenario_row.unwrap_or(0);
        self.scenario_row = Some(if down { at + 1 } else { at.saturating_sub(1) });
    }

    /// Applies a key to the step editor.
    ///
    /// Rewritten against the live draft each time, rather than through an
    /// owned copy: `self.overlay` and `self.session` are different fields of
    /// `self`, and the only thing worth extracting first is the editor's own
    /// cursor and scratch text.
    fn step_key(&mut self, code: KeyCode) {
        let Some(Overlay::Step(edit)) = &self.overlay else {
            return;
        };
        let index = edit.step;
        let mut focus = edit.focus;
        let mut text = edit.text.clone();

        let names: Vec<String> = self
            .session
            .connections
            .iter()
            .map(|(id, _)| id.0.clone())
            .collect();
        let frames: Vec<FrameDef> = self.session.frames.frames().cloned().collect();
        let links: Vec<sim_core::ConnectionId> = self
            .session
            .connections
            .iter()
            .map(|(id, _)| id.clone())
            .collect();

        let Some(step) = self
            .session
            .scenarios
            .draft
            .as_ref()
            .and_then(|draft| draft.scenario.steps.get(index).cloned())
        else {
            self.overlay = None;
            return;
        };

        let mut close = false;
        match code {
            KeyCode::Esc | KeyCode::Enter => close = true,
            KeyCode::Tab | KeyCode::Down => {
                let held = StepEdit::fields_with_frame(&step, &names, &frames)
                    .len()
                    .max(1);
                focus = (focus + 1) % held;
            }
            KeyCode::BackTab | KeyCode::Up => {
                let held = StepEdit::fields_with_frame(&step, &names, &frames)
                    .len()
                    .max(1);
                focus = (focus + held - 1) % held;
            }
            _ => {
                let fields = StepEdit::fields_with_frame(&step, &names, &frames);
                let at = focus.min(fields.len().saturating_sub(1));
                if let Some(field) = fields.into_iter().nth(at) {
                    self.act_on_step(index, &field, code, &names, &frames, &links, &mut text);
                }
            }
        }

        // The field under the cursor may have changed shape (a toggle just
        // flipped, say), so the scratch text is refreshed from whatever is
        // there now rather than carried over from before the key.
        if !matches!(code, KeyCode::Esc | KeyCode::Enter) {
            if let Some(step) = self
                .session
                .scenarios
                .draft
                .as_ref()
                .and_then(|draft| draft.scenario.steps.get(index).cloned())
            {
                let fields = StepEdit::fields_with_frame(&step, &names, &frames);
                let at = focus.min(fields.len().saturating_sub(1));
                let reseed_kinds = matches!(
                    fields.get(at),
                    Some(
                        StepField::Delay
                            | StepField::Bytes
                            | StepField::WaitPattern
                            | StepField::WaitOffset
                            | StepField::TimeoutMs
                            | StepField::SendField(_)
                            | StepField::WaitField(_)
                    )
                );
                if reseed_kinds
                    && matches!(
                        code,
                        KeyCode::Tab | KeyCode::Down | KeyCode::BackTab | KeyCode::Up
                    )
                {
                    let mut fresh = StepEdit {
                        step: index,
                        focus,
                        text: String::new(),
                    };
                    fresh.reseed(&step, &names, &frames);
                    text = fresh.text;
                }
            }
        }

        if close {
            self.overlay = None;
        } else {
            self.overlay = Some(Overlay::Step(StepEdit {
                step: index,
                focus,
                text,
            }));
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "a step touches the project, the draft and its own scratch text at once"
    )]
    fn act_on_step(
        &mut self,
        index: usize,
        field: &StepField,
        code: KeyCode,
        names: &[String],
        frames: &[FrameDef],
        links: &[sim_core::ConnectionId],
        text: &mut String,
    ) {
        match field {
            StepField::Kind | StepField::Target(_) => {
                self.act_on_kind_or_target(index, field, code, names, links);
            }
            StepField::Delay
            | StepField::Bytes
            | StepField::SendFrame
            | StepField::SendField(_) => {
                self.act_on_send(index, field, code, frames, text);
            }
            _ => self.act_on_wait(index, field, code, frames, text),
        }
    }

    fn act_on_kind_or_target(
        &mut self,
        index: usize,
        field: &StepField,
        code: KeyCode,
        names: &[String],
        links: &[sim_core::ConnectionId],
    ) {
        let Some(draft) = self.session.scenarios.draft.as_mut() else {
            return;
        };
        match field {
            StepField::Kind => {
                if let Some(delta) = arrow_delta(code) {
                    if let Some(step) = draft.scenario.steps.get(index) {
                        let current = ActionKind::of(&step.action);
                        let next = crate::connection_form::cycle(&ActionKind::ALL, current, delta);
                        if next.needs_a_connection() && links.is_empty() {
                            return;
                        }
                        draft.set_action(index, next, links);
                    }
                }
            }
            StepField::Target(i) => {
                if matches!(code, KeyCode::Char(' ')) {
                    if let Some(step) = draft.scenario.steps.get_mut(index) {
                        let id = sim_core::ConnectionId(names[*i].clone());
                        if step.targets.contains(&id) {
                            // The last target cannot be unticked: a step aimed
                            // at nothing is a step the loader refuses.
                            if step.targets.len() > 1 {
                                step.targets.retain(|held| held != &id);
                            }
                        } else {
                            step.targets.push(id);
                        }
                    }
                }
            }

            _ => {}
        }
    }

    fn act_on_send_field(
        &mut self,
        index: usize,
        i: usize,
        code: KeyCode,
        frames: &[FrameDef],
        text: &mut String,
    ) {
        let Some(draft) = self.session.scenarios.draft.as_mut() else {
            return;
        };
        let Some(step) = draft.scenario.steps.get(index).cloned() else {
            return;
        };
        let Action::Send { frame, .. } = &step.action else {
            return;
        };
        let Some(field_def) = frames
            .iter()
            .find(|f| &f.name == frame)
            .and_then(|d| d.fields.get(i))
            .cloned()
        else {
            return;
        };
        if matches!(code, KeyCode::Char(' ')) {
            let Action::Send { with, .. } = &step.action else {
                return;
            };
            let on = !with.contains_key(&field_def.name);
            let Some(definition) = frames.iter().find(|f| f.name == *frame) else {
                return;
            };
            scenarios::set_override(
                draft.scenario.steps.get_mut(index).expect("just read"),
                definition,
                &field_def.name,
                on,
            );
            return;
        }
        if matches!(code, KeyCode::Char('c')) {
            let Action::Send { from_capture, .. } = &step.action else {
                return;
            };
            let on = !from_capture.contains_key(&field_def.name);
            // Turning it off is always allowed, a step moved or removed out
            // from under a capture being the one way this field's own
            // variable can already be gone from `available`. Turning it on
            // needs something to turn it on to.
            let available = scenarios::captured_before(&draft.scenario, index);
            if on && available.is_empty() {
                return;
            }
            scenarios::set_from_capture(
                draft.scenario.steps.get_mut(index).expect("just read"),
                &field_def.name,
                on.then(|| available[0].as_str()),
            );
            return;
        }
        let Action::Send { from_capture, .. } = &step.action else {
            return;
        };
        if let Some(variable) = from_capture.get(&field_def.name) {
            let Some(delta) = arrow_delta(code) else {
                return;
            };
            let available = scenarios::captured_before(&draft.scenario, index);
            let choices: Vec<&String> = available.iter().collect();
            let next = crate::connection_form::cycle(&choices, variable, delta).clone();
            scenarios::set_from_capture(
                draft.scenario.steps.get_mut(index).expect("just read"),
                &field_def.name,
                Some(&next),
            );
            return;
        }
        // Editing only reaches a value already ticked in: an
        // untouched field still means the frame's own default.
        let Action::Send { with, .. } = &step.action else {
            return;
        };
        if !with.contains_key(&field_def.name) {
            return;
        }
        edit_text(code, text);
        let Some(value) = typed_override_value(&field_def.kind, text) else {
            return;
        };
        if let Some(Step {
            action: Action::Send { with, .. },
            ..
        }) = draft.scenario.steps.get_mut(index)
        {
            with.insert(field_def.name.clone(), value);
        }
    }

    fn act_on_send(
        &mut self,
        index: usize,
        field: &StepField,
        code: KeyCode,
        frames: &[FrameDef],
        text: &mut String,
    ) {
        if let StepField::SendField(i) = field {
            self.act_on_send_field(index, *i, code, frames, text);
            return;
        }
        let Some(draft) = self.session.scenarios.draft.as_mut() else {
            return;
        };
        match field {
            StepField::Delay => {
                edit_digits(code, text);
                if let Some(Step {
                    action: Action::Wait { delay },
                    ..
                }) = draft.scenario.steps.get_mut(index)
                {
                    *delay = Duration::from_millis(text.parse().unwrap_or(0));
                }
            }
            StepField::Bytes => {
                edit_text(code, text);
                if let Ok(bytes) = hex::parse(text) {
                    if let Some(Step {
                        action: Action::Raw { bytes: held },
                        ..
                    }) = draft.scenario.steps.get_mut(index)
                    {
                        *held = bytes;
                    }
                }
            }
            StepField::SendFrame => {
                if let Some(delta) = arrow_delta(code) {
                    if !frames.is_empty() {
                        if let Some(Step {
                            action: Action::Send { frame, .. },
                            ..
                        }) = draft.scenario.steps.get_mut(index)
                        {
                            let names: Vec<String> =
                                frames.iter().map(|f| f.name.clone()).collect();
                            let current = names.iter().position(|n| n == frame).unwrap_or(0);
                            let at = i32::try_from(current).unwrap_or(0)
                                + i32::try_from(delta).unwrap_or(0);
                            let at = at.rem_euclid(i32::try_from(names.len()).unwrap_or(1));
                            let picked = names[usize::try_from(at).unwrap_or(0)].clone();
                            frame.clone_from(&picked);
                        }
                        if let Some(step) = draft.scenario.steps.get(index).cloned() {
                            if let Action::Send { frame, .. } = &step.action {
                                scenarios::set_frame(
                                    draft.scenario.steps.get_mut(index).expect("just read"),
                                    frame,
                                );
                            }
                        }
                    }
                }
            }

            _ => {}
        }
    }

    fn act_on_wait(
        &mut self,
        index: usize,
        field: &StepField,
        code: KeyCode,
        frames: &[FrameDef],
        text: &mut String,
    ) {
        match field {
            StepField::WaitByFrame
            | StepField::WaitPattern
            | StepField::WaitAnchored
            | StepField::WaitOffset => {
                self.act_on_wait_pattern(index, field, code, frames, text);
            }
            _ => self.act_on_wait_frame(index, field, code, frames, text),
        }
    }

    fn act_on_wait_pattern(
        &mut self,
        index: usize,
        field: &StepField,
        code: KeyCode,
        frames: &[FrameDef],
        text: &mut String,
    ) {
        let Some(draft) = self.session.scenarios.draft.as_mut() else {
            return;
        };
        match field {
            StepField::WaitByFrame => {
                if matches!(code, KeyCode::Char(' ')) {
                    if let Some(step) = draft.scenario.steps.get(index) {
                        let by_frame = !matches!(
                            step.action,
                            Action::WaitFor {
                                expect: Expect::Frame { .. },
                                ..
                            }
                        );
                        scenarios::set_wait_by_frame(
                            draft.scenario.steps.get_mut(index).expect("just read"),
                            by_frame,
                            frames.first().map(|f| f.name.as_str()),
                        );
                    }
                }
            }
            StepField::WaitPattern => {
                edit_text(code, text);
                if let Some(parsed) = sim_core::HexPattern::parse(text) {
                    if let Some(Step {
                        action:
                            Action::WaitFor {
                                expect: Expect::Pattern { pattern, .. },
                                ..
                            },
                        ..
                    }) = draft.scenario.steps.get_mut(index)
                    {
                        *pattern = parsed;
                    }
                }
            }
            StepField::WaitAnchored => {
                if matches!(code, KeyCode::Char(' ')) {
                    if let Some(Step {
                        action:
                            Action::WaitFor {
                                expect: Expect::Pattern { anchor, .. },
                                ..
                            },
                        ..
                    }) = draft.scenario.steps.get_mut(index)
                    {
                        *anchor = if matches!(anchor, sim_core::pattern::Anchor::At(_)) {
                            sim_core::pattern::Anchor::Anywhere
                        } else {
                            sim_core::pattern::Anchor::At(0)
                        };
                    }
                }
            }
            StepField::WaitOffset => {
                edit_digits(code, text);
                if let Some(Step {
                    action:
                        Action::WaitFor {
                            expect: Expect::Pattern { anchor, .. },
                            ..
                        },
                    ..
                }) = draft.scenario.steps.get_mut(index)
                {
                    *anchor = sim_core::pattern::Anchor::At(text.parse().unwrap_or(0));
                }
            }
            _ => {}
        }
    }

    fn act_on_wait_frame(
        &mut self,
        index: usize,
        field: &StepField,
        code: KeyCode,
        frames: &[FrameDef],
        text: &mut String,
    ) {
        let Some(draft) = self.session.scenarios.draft.as_mut() else {
            return;
        };
        match field {
            StepField::WaitFrame => {
                if let Some(delta) = arrow_delta(code) {
                    if !frames.is_empty() {
                        if let Some(Step {
                            action:
                                Action::WaitFor {
                                    expect:
                                        Expect::Frame {
                                            frame,
                                            values,
                                            capture,
                                        },
                                    ..
                                },
                            ..
                        }) = draft.scenario.steps.get_mut(index)
                        {
                            let names: Vec<String> =
                                frames.iter().map(|f| f.name.clone()).collect();
                            let current = names.iter().position(|n| n == frame).unwrap_or(0);
                            let at = i32::try_from(current).unwrap_or(0)
                                + i32::try_from(delta).unwrap_or(0);
                            let at = at.rem_euclid(i32::try_from(names.len()).unwrap_or(1));
                            frame.clone_from(&names[usize::try_from(at).unwrap_or(0)]);
                            values.clear();
                            capture.clear();
                        }
                    }
                }
            }
            StepField::WaitField(i) => self.act_on_wait_field(index, *i, code, frames, text),
            StepField::Limited => {
                if matches!(code, KeyCode::Char(' ')) {
                    if let Some(Step {
                        action: Action::WaitFor { timeout, .. },
                        ..
                    }) = draft.scenario.steps.get_mut(index)
                    {
                        *timeout = timeout.is_none().then(|| Duration::from_millis(500));
                    }
                }
            }
            StepField::TimeoutMs => {
                edit_digits(code, text);
                if let Some(Step {
                    action:
                        Action::WaitFor {
                            timeout: Some(timeout),
                            ..
                        },
                    ..
                }) = draft.scenario.steps.get_mut(index)
                {
                    *timeout = Duration::from_millis(text.parse().unwrap_or(1));
                }
            }
            _ => {}
        }
    }

    fn act_on_wait_field(
        &mut self,
        index: usize,
        i: usize,
        code: KeyCode,
        frames: &[FrameDef],
        text: &mut String,
    ) {
        let Some(draft) = self.session.scenarios.draft.as_mut() else {
            return;
        };
        let Some(step) = draft.scenario.steps.get(index).cloned() else {
            return;
        };
        let Action::WaitFor {
            expect:
                Expect::Frame {
                    frame,
                    values,
                    capture,
                },
            ..
        } = &step.action
        else {
            return;
        };
        let Some(definition) = frames.iter().find(|f| f.name == *frame) else {
            return;
        };
        let Some(field_def) = definition.fields.get(i) else {
            return;
        };
        if matches!(code, KeyCode::Char(' ')) {
            let on = !values.contains_key(&field_def.name);
            scenarios::set_match(
                draft.scenario.steps.get_mut(index).expect("just read"),
                definition,
                &field_def.name,
                on,
            );
            return;
        }
        // Left/right rather than a letter key: a variable name is free text,
        // and no letter is safe to reserve as its own toggle once the name
        // being typed might contain that very letter.
        if arrow_delta(code).is_some() {
            let on = !capture.contains_key(&field_def.name);
            scenarios::set_capture(
                draft.scenario.steps.get_mut(index).expect("just read"),
                &field_def.name,
                on,
            );
            // Turning it on seeds the variable's name from the field's own,
            // and the scratch buffer has to agree before the next key is
            // read as extending it rather than starting over from nothing.
            if on {
                field_def.name.clone_into(text);
            }
            return;
        }
        if capture.contains_key(&field_def.name) {
            edit_text(code, text);
            scenarios::rename_capture(
                draft.scenario.steps.get_mut(index).expect("just read"),
                &field_def.name,
                text,
            );
            return;
        }
        // Editing only reaches a value already ticked in: an untouched field
        // still means any value at all is accepted.
        if !values.contains_key(&field_def.name) {
            return;
        }
        edit_text(code, text);
        let Some(value) = typed_override_value(&field_def.kind, text) else {
            return;
        };
        if let Some(Step {
            action:
                Action::WaitFor {
                    expect: Expect::Frame { values, .. },
                    ..
                },
            ..
        }) = draft.scenario.steps.get_mut(index)
        {
            values.insert(field_def.name.clone(), value);
        }
    }

    fn pick_scenario(&mut self, delta: isize) {
        let at = moved(
            self.session.scenarios.selected,
            delta,
            self.session.scenarios.entries.len(),
        );
        let Some(at) = at else {
            return;
        };
        self.session.scenarios.selected = Some(at);
    }

    fn start_selected(&mut self) {
        let Some(scenario) = self.session.scenarios.selected_scenario().cloned() else {
            return;
        };
        scenarios::start(&mut self.session, &self.engine, &scenario);
    }

    /// Stopping is by name, which is what the engine answers to.
    fn stop_selected(&mut self) {
        let Some(name) = self
            .session
            .scenarios
            .selected_scenario()
            .map(|scenario| scenario.name.clone())
        else {
            return;
        };
        if self.session.running.contains_key(&name) {
            self.engine.stop_scenario(name);
        }
    }

    /// The keys the traffic list answers to, and whether it took this one.
    fn watching(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Down | KeyCode::Char('j') => self.traffic_move(1),
            KeyCode::Up | KeyCode::Char('k') => self.traffic_move(-1),
            KeyCode::Left => self.traffic_focus = TrafficFocus::Rows,
            KeyCode::Right
                if self
                    .monitor()
                    .is_some_and(|monitor| monitor.selected.is_some()) =>
            {
                self.traffic_focus = TrafficFocus::Fields;
            }
            KeyCode::Esc => {
                self.traffic_focus = TrafficFocus::Rows;
                self.with_monitor(|monitor| monitor.selected = None);
            }
            KeyCode::Enter | KeyCode::Char('d') => self.pick_frame(),
            KeyCode::Char('f') => self.with_monitor(|monitor| monitor.follow = !monitor.follow),
            KeyCode::Char('p') => self.toggle_paused(),
            KeyCode::Char('c') => self.clear_monitor(),
            KeyCode::Char('/') => self.open_filter(),
            KeyCode::Char('m') => {
                let id = self.session.open_monitor();
                self.current_monitor = Some(id);
            }
            KeyCode::Char('x') => self.close_current_monitor(),
            KeyCode::Char('[') => self.switch_monitor(-1),
            KeyCode::Char(']') => self.switch_monitor(1),
            KeyCode::Char('h') => self.send_row_to_hex(),
            KeyCode::Char('F') => self.open_row_in_frames(),
            _ => return false,
        }
        true
    }

    /// Up/down, aimed at whichever pane has the keyboard: the row list, or
    /// the decoded fields underneath the row currently being read.
    fn traffic_move(&mut self, delta: isize) {
        match self.traffic_focus {
            TrafficFocus::Rows => self.step(delta),
            TrafficFocus::Fields => {
                self.traffic_field_scroll = self.traffic_field_scroll.saturating_add_signed(delta);
            }
        }
    }

    /// Applies `change` to the view on show, if there is one.
    fn with_monitor(&mut self, change: impl FnOnce(&mut MonitorState)) {
        if let Some(id) = self.monitor_id() {
            if let Some(monitor) = self.session.monitors.get_mut(&id) {
                change(monitor);
            }
        }
    }

    /// Freezes the view on the newest row admitted so far, or lets it run
    /// again. Frames keep arriving in the shared buffer either way.
    fn toggle_paused(&mut self) {
        let next_seq = self.session.next_seq();
        self.with_monitor(|monitor| {
            monitor.paused_at = monitor
                .paused_at
                .is_none()
                .then(|| next_seq.saturating_sub(1));
        });
    }

    /// Hides everything logged so far, in this view only.
    fn clear_monitor(&mut self) {
        let next_seq = self.session.next_seq();
        self.with_monitor(|monitor| {
            monitor.since = next_seq;
            monitor.paused_at = None;
        });
    }

    /// Moves the tab strip by `delta`, wrapping at either end.
    fn switch_monitor(&mut self, delta: isize) {
        let ids: Vec<sim_session::state::MonitorId> =
            self.session.monitors.keys().copied().collect();
        if ids.is_empty() {
            return;
        }
        let Some(current) = self.monitor_id() else {
            return;
        };
        self.current_monitor = Some(crate::connection_form::cycle(&ids, current, delta));
        self.traffic_focus = TrafficFocus::Rows;
    }

    /// Closes the view on show. Refused on the last one: a bench with no
    /// traffic view at all has nowhere for a captured frame to land.
    fn close_current_monitor(&mut self) {
        if self.session.monitors.len() <= 1 {
            self.session.last_error = Some("The last traffic view cannot be closed.".to_owned());
            return;
        }
        if let Some(id) = self.monitor_id() {
            self.session.close_monitor(id);
            self.current_monitor = None;
            self.traffic_focus = TrafficFocus::Rows;
        }
    }

    /// Opens the filter editor for the view on show.
    fn open_filter(&mut self) {
        if self.monitor_id().is_some() {
            let filter = self.monitor().map(|monitor| monitor.filter.clone());
            self.overlay = filter.map(|filter| Overlay::Filter(FilterEdit::new(&filter)));
        }
    }

    /// Copies the selected row's bytes into the hex injection box and
    /// switches to it, ready to be sent back or tweaked first.
    fn send_row_to_hex(&mut self) {
        let Some((entry, _)) = self.selected_reading() else {
            return;
        };
        self.session.hex_input = hex::spaced(&entry.bytes);
        self.tab = Tab::HexInject;
    }

    /// Hands the selected row's bytes to the frame editor, decoded into
    /// whichever definition is chosen there.
    fn open_row_in_frames(&mut self) {
        let Some((entry, _)) = self.selected_reading() else {
            return;
        };
        self.session.pending_frame_hex = Some(entry.bytes.clone());
        self.tab = Tab::Frames;
        self.apply_pending_frame_hex();
    }

    /// Decodes bytes handed over from Traffic into the frame currently
    /// selected in the Frames view, the same way the window does.
    fn apply_pending_frame_hex(&mut self) {
        let Some(bytes) = self.session.pending_frame_hex.take() else {
            return;
        };
        let Some(frame) = self.session.frames.selected_frame().cloned() else {
            self.session.last_error = Some("Choose a frame first.".to_owned());
            return;
        };
        let typed = hex::spaced(&bytes);
        self.session.frame_hex_note =
            sim_session::frames::apply_hex(&mut self.session, &frame, &typed);
    }

    /// Moves the read row by `delta`, stopping at either end.
    ///
    /// Reading a row and following the newest frame are opposite things, so the
    /// first stops the second: a list that keeps scrolling moves the row being
    /// read out from under you.
    fn step(&mut self, delta: isize) {
        let seqs: Vec<u64> = self.rows().iter().map(|entry| entry.seq).collect();
        let Some(last) = seqs.len().checked_sub(1) else {
            return;
        };
        let Some(id) = self.monitor_id() else {
            return;
        };
        let Some(monitor) = self.session.monitors.get_mut(&id) else {
            return;
        };

        let at = match monitor
            .selected
            .and_then(|seq| seqs.iter().position(|s| *s == seq))
        {
            Some(at) => at.saturating_add_signed(delta).min(last),
            // Nothing read yet: start at the newest, which is what a bench is
            // looking at when it reaches for the keyboard.
            None => last,
        };
        monitor.selected = Some(seqs[at]);
        monitor.follow = false;
        self.traffic_field_scroll = 0;
    }
}
