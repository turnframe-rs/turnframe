//! The corpus: named scenarios loaded from files (spec §27.6).
//!
//! An **item** is one scenario — a starting state, the turn a person takes, and
//! what must be true afterwards. A **suite** is a set of items with tags, so a
//! run can say "only the trip write paths" without a second list of file
//! names.
//!
//! **The loader refuses what it does not understand.** Every structure carries
//! `deny_unknown_fields`, and [`EvalItem::validate`] rejects combinations that
//! parse but cannot mean anything. A loader that skipped a key it did not
//! recognise would quietly turn a typo into a weaker test, and a weaker test
//! into a green build.
//!
//! An item also says where each of its parts came from — `authored`, `derived`
//! or `recorded` ([`PartProvenance`]) — declared per item or per directory
//! ([`SuiteManifest`]). [`EvalItem::fingerprint`] records what was declared, and
//! [`crate::baseline::compare`] uses it to tell an intended change from a broken
//! pairing from a corpus defect. What each name means, and why **no tooling may
//! ever regenerate a recorded part**, is in
//! [`docs/evaluation.md`](https://github.com/turnframe-rs/turnframe/blob/main/docs/evaluation.md).
//!
//! # An item file
//!
//! ```toml
//! id = "trip.set_name"
//! name = "Setting the subject commits exactly one command"
//! tags = ["trip", "write"]
//!
//! [[setup.cases]]
//! workflow = "trip"
//! case_id = "trip-1"
//! label = "Trip 1"
//! revision = 3
//! state = { status = "draft" }
//!
//! [turn]
//! text = "Set the name to Lisbon"
//!
//! [expect]
//! commands = ["trip.set_name"]
//! events = ["trip.name_set"]
//! blocks = ["receipt"]
//!
//! [[expect.acts]]
//! kind = "apply_operation"
//! operation = "trip.set_name"
//!
//! [expect.forbid]
//! commands = ["trip.rebook"]
//!
//! judge = ["language_quality"]
//! ```
//!

use std::fmt;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use turnframe_core::ids::{CaseId, OptionId, WorkflowKey};
use turnframe_core::interaction::InteractionStatus;
use turnframe_core::locale::Locale;
use turnframe_core::replay::TurnPhase;
use turnframe_core::response::ResponseBlock;

use crate::config::SelectionConfig;
use crate::judge::JudgeCriterion;

/// Stable identifier of one corpus item.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(pub String);

impl ItemId {
    /// Wraps a label.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrows the label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ItemId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A label a run can select on.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Tag(pub String);

impl Tag {
    /// Wraps a label.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrows the label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Tag {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<String> for Tag {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A part of an item, named exactly as the item file names it.
///
/// The four parts partition everything about an item that decides what it
/// *measures*. `name`, `description` and `tags` are deliberately outside them:
/// renaming an item or retagging it changes how a report reads, not what the
/// run did, and a comparison that broke its pairing over a reworded sentence
/// would be useless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ItemPart {
    /// `setup` — the world the turn starts from, every seeded case and every
    /// seeded state. This is the part a projector under test usually writes.
    Setup,
    /// `turn` — what the person did.
    Turn,
    /// `expect` — what must be true afterwards.
    Expect,
    /// `judge` — which linguistic criteria are graded.
    Judge,
}

impl ItemPart {
    /// Every part, in the order a fingerprint records them.
    pub const ALL: [Self; 4] = [Self::Setup, Self::Turn, Self::Expect, Self::Judge];

    /// The snake-case name, which is also the key in the item file.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Turn => "turn",
            Self::Expect => "expect",
            Self::Judge => "judge",
        }
    }
}

impl fmt::Display for ItemPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a part of an item came from.
///
/// Three values, not two, and the third is the one that costs money when it is
/// missing. See the module documentation.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PartProvenance {
    /// A person wrote it and a person maintains it. The default for anything a
    /// corpus does not declare.
    ///
    /// It should not change between two runs of the same experiment, and if it
    /// did, the two runs were not the same experiment.
    #[default]
    Authored,
    /// The code under test writes it, so changing that code changes the item.
    ///
    /// Tooling may regenerate it. A change to it is the intended effect of the
    /// change being measured, not a broken pairing.
    Derived,
    /// Lifted from what the system actually emitted, and kept as testimony.
    ///
    /// **No tooling may regenerate it.** A recorded part that differs between
    /// two runs is a defect in the corpus or in whatever touched it — never a
    /// result about the model, and never something to fold into a score.
    Recorded,
}

impl PartProvenance {
    /// The snake-case name used in item files and reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Authored => "authored",
            Self::Derived => "derived",
            Self::Recorded => "recorded",
        }
    }

    /// Returns `true` when tooling is allowed to rewrite this part.
    ///
    /// Only [`Derived`](Self::Derived) is. An `authored` part belongs to the
    /// person who wrote it, and a `recorded` part is evidence.
    #[must_use]
    pub const fn may_be_regenerated(self) -> bool {
        matches!(self, Self::Derived)
    }
}

impl fmt::Display for PartProvenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where each part of an item came from.
///
/// A table in the item file, or in a directory's [`SuiteManifest`]:
///
/// ```toml
/// [provenance]
/// setup = "derived"
/// expect = "recorded"
/// ```
///
/// Every part not named is [`PartProvenance::Authored`], and an unknown key is a
/// parse error rather than a shrug — a corpus that ignored `setpu = "recorded"`
/// would report regenerated testimony as an ordinary result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Provenance {
    /// Where `setup` came from — the part a projector under test usually writes.
    pub setup: PartProvenance,
    /// Where `turn` came from.
    pub turn: PartProvenance,
    /// Where `expect` came from.
    pub expect: PartProvenance,
    /// Where `judge` came from.
    pub judge: PartProvenance,
}

impl Provenance {
    /// Everything authored, which is what a corpus that declares nothing means.
    #[must_use]
    pub const fn authored() -> Self {
        Self {
            setup: PartProvenance::Authored,
            turn: PartProvenance::Authored,
            expect: PartProvenance::Authored,
            judge: PartProvenance::Authored,
        }
    }

    /// Where one part came from.
    #[must_use]
    pub const fn of(&self, part: ItemPart) -> PartProvenance {
        match part {
            ItemPart::Setup => self.setup,
            ItemPart::Turn => self.turn,
            ItemPart::Expect => self.expect,
            ItemPart::Judge => self.judge,
        }
    }

    /// Records where one part came from.
    pub const fn set(&mut self, part: ItemPart, provenance: PartProvenance) {
        match part {
            ItemPart::Setup => self.setup = provenance,
            ItemPart::Turn => self.turn = provenance,
            ItemPart::Expect => self.expect = provenance,
            ItemPart::Judge => self.judge = provenance,
        }
    }

    /// Returns `true` when nothing at all was declared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::authored()
    }

    /// The parts declared as something other than authored, in
    /// [`ItemPart::ALL`] order.
    #[must_use]
    pub fn declared(&self) -> Vec<ItemPart> {
        ItemPart::ALL
            .into_iter()
            .filter(|part| self.of(*part) != PartProvenance::Authored)
            .collect()
    }
}

/// One part's digest, and where the corpus said that part came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartDigest {
    /// Which part.
    pub part: ItemPart,
    /// A digest of its canonical form. Two items with the same digest for a
    /// part carry the same content in it.
    pub digest: String,
    /// Where the corpus said it came from. Absent in a report written before
    /// provenance existed, which reads as [`PartProvenance::Authored`] — the
    /// conservative answer, since an authored part that moved breaks a pairing.
    #[serde(default)]
    pub provenance: PartProvenance,
}

/// What an item contained when a run measured it.
///
/// A report carries one per item so a later comparison can ask *is this still the
/// same experiment?* An
/// [`ItemFingerprint`] with no parts is "unknown" — it comes from a report
/// written before fingerprints existed — and a comparison says so rather than
/// pretending the pairing was checked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ItemFingerprint {
    /// One digest per part, in [`ItemPart::ALL`] order.
    pub parts: Vec<PartDigest>,
}

impl ItemFingerprint {
    /// Returns `true` when nothing was recorded, so no pairing can be checked.
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        self.parts.is_empty()
    }

    /// The digest recorded for `part`.
    #[must_use]
    pub fn digest_of(&self, part: ItemPart) -> Option<&str> {
        self.parts
            .iter()
            .find(|entry| entry.part == part)
            .map(|entry| entry.digest.as_str())
    }

    /// Where the corpus said `part` came from.
    ///
    /// [`PartProvenance::Authored`] for a part nothing was recorded about, which
    /// is the conservative reading: an authored part that moved breaks a
    /// pairing, so an absent declaration never quietly excuses a difference.
    #[must_use]
    pub fn provenance_of(&self, part: ItemPart) -> PartProvenance {
        self.parts
            .iter()
            .find(|entry| entry.part == part)
            .map_or(PartProvenance::Authored, |entry| entry.provenance)
    }

    /// The parts both fingerprints recorded and disagree about.
    ///
    /// Empty when either side is [unknown](Self::is_unknown): nothing can be
    /// concluded from a digest that was never taken.
    #[must_use]
    pub fn differing_parts(&self, other: &Self) -> Vec<ItemPart> {
        if self.is_unknown() || other.is_unknown() {
            return Vec::new();
        }
        ItemPart::ALL
            .into_iter()
            .filter(
                |part| match (self.digest_of(*part), other.digest_of(*part)) {
                    (Some(left), Some(right)) => left != right,
                    _ => false,
                },
            )
            .collect()
    }
}

/// One named scenario.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvalItem {
    /// Stable identifier, unique within a suite. Reports and baselines join on
    /// it, so renaming one is renaming a measurement.
    pub id: ItemId,
    /// One sentence a human reads in a report.
    pub name: String,
    /// Longer prose, when the scenario needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Labels a run selects on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<Tag>,
    /// The state the world is in before the turn.
    #[serde(default)]
    pub setup: Setup,
    /// Turns the person takes first, in the same conversation. Only the last turn is
    /// observed; what the conversation built is read back after it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub before: Vec<TurnSpec>,
    /// The turn the person takes.
    pub turn: TurnSpec,
    /// What must be true afterwards, checked without a model.
    #[serde(default)]
    pub expect: Expectations,
    /// Which linguistic qualities a judge grades. Empty means no judge runs,
    /// which is the right answer for most items.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub judge: Vec<JudgeCriterion>,
    /// Where each part of this item came from: authored, derived or recorded.
    ///
    /// Declared once here, or once for a whole directory in a
    /// [`SuiteManifest`]. It changes nothing about how the item runs and
    /// everything about how [`crate::baseline::compare`] reads a difference — an
    /// intended projection change, a broken pairing, or a corpus defect.
    #[serde(default, skip_serializing_if = "Provenance::is_empty")]
    pub provenance: Provenance,
}

impl EvalItem {
    /// Checks the combinations `serde` cannot.
    ///
    /// # Errors
    ///
    /// [`CorpusError::Invalid`] naming the field and the reason.
    pub fn validate(&self) -> Result<(), CorpusError> {
        if self.id.as_str().trim().is_empty() {
            return Err(CorpusError::invalid("id", "an item needs an identifier"));
        }
        if self.name.trim().is_empty() {
            return Err(CorpusError::invalid("name", "an item needs a name"));
        }
        for turn in &self.before {
            turn.validate()?;
        }
        self.turn.validate()?;
        if self.turn.external.is_some() {
            return Err(CorpusError::invalid(
                "turn",
                "the observed turn is the person's; an outside change goes in `before`",
            ));
        }
        self.expect.validate()?;
        if let Some(understanding) = &self.expect.understanding {
            understanding.validate(self.turn.text.as_deref())?;
        }
        for seed in &self.setup.cases {
            seed.validate()?;
        }
        self.validate_provenance()?;
        Ok(())
    }

    /// Checks the `provenance` declaration is one this item could mean.
    ///
    /// A part declared on an item that does not have it is a typo wearing a
    /// valid name: `setup = "recorded"` on an item with no seeded cases claims
    /// testimony that is not there, and silently accepting it would be exactly
    /// the shrug this loader refuses everywhere else.
    fn validate_provenance(&self) -> Result<(), CorpusError> {
        for part in self.provenance.declared() {
            if !self.carries(part) {
                return Err(CorpusError::Invalid {
                    field: "provenance".to_owned(),
                    reason: format!(
                        "`{part}` is declared `{}`, and this item has no `{part}`",
                        self.provenance.of(part)
                    ),
                });
            }
        }
        Ok(())
    }

    /// Returns `true` when the item actually has something in `part`.
    ///
    /// A turn always has something in it — [`TurnSpec::validate`] refuses one
    /// that does not — so [`ItemPart::Turn`] is always carried.
    #[must_use]
    pub fn carries(&self, part: ItemPart) -> bool {
        match part {
            // Every channel a setup can arrive on, not only the cases. The
            // register and the prior exchanges are seeded the same way and
            // through the same writers, so a scene whose whole world is «this
            // is what had already been said» does have a setup — and declaring
            // how it was produced is exactly as meaningful there. Reading only
            // the cases refused the declaration on a scene that carried
            // twenty-four turns of history.
            ItemPart::Setup => {
                !self.setup.cases.is_empty()
                    || !self.setup.records.is_empty()
                    || !self.setup.history.is_empty()
            }
            ItemPart::Turn => true,
            ItemPart::Expect => !self.expect.is_empty(),
            ItemPart::Judge => !self.judge.is_empty(),
        }
    }

    /// Returns `true` when this item carries `tag`.
    #[must_use]
    pub fn has_tag(&self, tag: &Tag) -> bool {
        self.tags.contains(tag)
    }

    /// Digests the item part by part, recording where the corpus said each part
    /// came from.
    ///
    /// A run stores this on [`ItemReport`](crate::report::ItemReport) so a later
    /// comparison can tell whether the two runs measured the same thing. The
    /// digest is over a canonical rendering with object keys sorted, so an item
    /// whose seeded state was written out with its fields in a different order
    /// still fingerprints the same.
    ///
    /// ```
    /// use turnframe_eval::corpus::{EvalItem, ItemPart, PartProvenance};
    ///
    /// let item: EvalItem = toml::from_str(
    ///     r#"
    ///     id = "a"
    ///     name = "A scenario"
    ///
    ///     [provenance]
    ///     setup = "derived"
    ///
    ///     [turn]
    ///     text = "hello"
    ///
    ///     [[setup.cases]]
    ///     workflow = "trip"
    ///     case_id = "trip-1"
    ///     label = "Trip 1"
    ///     state = { status = "draft" }
    ///     "#,
    /// )?;
    /// item.validate()?;
    ///
    /// let fingerprint = item.fingerprint();
    /// assert_eq!(fingerprint.provenance_of(ItemPart::Setup), PartProvenance::Derived);
    /// assert_eq!(fingerprint.provenance_of(ItemPart::Turn), PartProvenance::Authored);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn fingerprint(&self) -> ItemFingerprint {
        ItemFingerprint {
            parts: ItemPart::ALL
                .into_iter()
                .map(|part| PartDigest {
                    part,
                    digest: self.digest_of(part),
                    provenance: self.provenance.of(part),
                })
                .collect(),
        }
    }

    fn digest_of(&self, part: ItemPart) -> String {
        let rendered = match part {
            ItemPart::Setup => serde_json::to_value(&self.setup),
            ItemPart::Turn => serde_json::to_value(&self.turn),
            ItemPart::Expect => serde_json::to_value(&self.expect),
            ItemPart::Judge => serde_json::to_value(&self.judge),
        };
        // A part of an item is a plain structure of strings, numbers and
        // `serde_json::Value`s, so this cannot fail; if it ever did, the
        // message is still deterministic and still distinct per part, which
        // keeps a fingerprint comparison honest rather than accidentally equal.
        let rendered = rendered.unwrap_or_else(|error| {
            serde_json::Value::String(format!("unserializable {part}: {error}"))
        });
        let mut canonical = String::new();
        write_canonical(&rendered, &mut canonical);
        blake3::hash(canonical.as_bytes()).to_hex().to_string()
    }
}

/// Writes a value in a canonical, injective form: object keys sorted, strings
/// length-prefixed so no content can imitate a delimiter.
fn write_canonical(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Null => out.push('n'),
        serde_json::Value::Bool(true) => out.push('t'),
        serde_json::Value::Bool(false) => out.push('f'),
        serde_json::Value::Number(number) => {
            let _ = write!(out, "#{number};");
        }
        serde_json::Value::String(text) => write_canonical_text(text, out),
        serde_json::Value::Array(items) => {
            out.push('[');
            for item in items {
                write_canonical(item, out);
            }
            out.push(']');
        }
        serde_json::Value::Object(map) => {
            // `serde_json` is built with `preserve_order` in this workspace, so
            // an object's iteration order is the order the file happened to use.
            // Sorting here is what makes the digest a property of the content.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for key in keys {
                write_canonical_text(key, out);
                if let Some(entry) = map.get(key) {
                    write_canonical(entry, out);
                }
            }
            out.push('}');
        }
    }
}

fn write_canonical_text(text: &str, out: &mut String) {
    let _ = write!(out, "s{}:{text}", text.len());
}

/// The world before the turn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Setup {
    /// Cases the harness seeds and the actor is authorized to address.
    pub cases: Vec<CaseSeed>,
    /// Things the world holds that are not cases.
    ///
    /// A case is work in progress; many scenarios need something that is simply
    /// there, a traveler already registered or a past booking, which a case about
    /// it would misdescribe.
    ///
    /// The library knows nothing about what a record IS. It carries a kind and
    /// a payload, and the harness that owns the domain decides what to do with
    /// them — the same bargain as a case's state, which crosses as JSON for the
    /// same reason.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<SeededRecord>,
    /// What was already said in this conversation, oldest first.
    ///
    /// Part of the world and not of the turn, which is why it sits here: the
    /// turn is what the person does next, and this is what they and the
    /// assistant had already said when they did it.
    ///
    /// **It is text, not a replay.** Seeding an exchange does not re-run it:
    /// nothing is journaled, no case moves, no card opens. What a previous turn
    /// DID belongs in [`Self::cases`], and keeping the two consistent is the
    /// author's job — a history saying «done» beside a case that never moved
    /// describes a product that lies, which is a fine scenario to write on
    /// purpose and a bad one to write by accident.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<PriorExchange>,
}

/// Something the world holds that is not a case.
///
/// Opaque on purpose: `kind` is a name the harness recognises and `data` is
/// whatever that harness needs. A library that tried to type these would be a
/// library with an opinion about what a traveler is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeededRecord {
    /// What kind of thing this is, in the harness's own vocabulary.
    pub kind: String,
    /// The record itself, in whatever shape that kind takes.
    pub data: serde_json::Value,
}

/// An exchange that already happened in this conversation.
///
/// The assistant's side is optional because the interesting histories are the
/// ones that end badly: a question nobody answered, a turn that died. A corpus
/// that could only describe well-formed pairs could not seed the conversation a
/// user is actually annoyed about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriorExchange {
    /// What the person said.
    pub user: String,
    /// What the assistant answered, when it answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant: Option<String>,
}

/// What a case's state must say after the turn.
///
/// The path is a JSON Pointer into the workflow's own state, because the state
/// is the workflow's business and this crate cannot know its shape — so an item
/// says `/fields/address_street/value` and the library compares what it finds
/// there, with no opinion about what a field is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateExpectation {
    /// The case this is about.
    pub case_id: CaseId,
    /// JSON Pointer into that case's state, as the workflow serializes it.
    pub path: String,
    /// The value that path must hold afterwards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<serde_json::Value>,
    /// Values any one of which that path may hold afterwards: the user's words read
    /// right in more than one form, such as with or without an article.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub one_of: Vec<serde_json::Value>,
    /// The value that path must still hold, whatever it was.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unchanged: bool,
    /// Nothing must be there: the path holds `null`, or does not resolve.
    ///
    /// # Why this is not `equals: null`
    ///
    /// Because that cannot be written. [`equals`](Self::equals) is an
    /// `Option`, so a JSON `null` deserializes as «no expectation given» and
    /// [`validate`](Self::validate) then refuses the whole entry for asserting
    /// nothing. The absence had no way to be said at all.
    ///
    /// # What could not be measured without it
    ///
    /// «I do not have one» is an answer, not a silence, and a collecting workflow
    /// often turns on it: a person who declines their loyalty number has answered
    /// the question, and the flow moves on.
    /// What the turn must do is record the refusal — a field that ends with no
    /// value and a status that moved — and every expectation this crate had
    /// asserts that a value IS somewhere. A suite measuring those flows could
    /// state the value case and not its opposite, which is the half that goes
    /// wrong: a turn that quietly writes something into a field the user
    /// declined reads, to every other assertion, exactly like a turn that
    /// respected them.
    ///
    /// Resolving to nothing counts as absent on purpose. A workflow may drop
    /// the key instead of nulling it, and an assertion that told those two
    /// apart would be about the serializer rather than about the record.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub absent: bool,
    /// Two strings differing only in case are the same value here.
    ///
    /// Only meaningful beside [`equals`](Self::equals), and only for a field
    /// whose case the application does not decide. That is a real category and
    /// not a loophole: a domain canonicalises what it owns, a booking reference
    /// to upper case or a loyalty number to its letters and digits, and stores a
    /// free-text field the way it was handed it, because nobody can guess how a
    /// person or a place writes its own name.
    ///
    /// On those fields the case is the model's typography, not the record's
    /// content. A person types «lisbon», one turn stores «lisbon» and the next
    /// «Lisbon», and both are the same answer to the same question: an
    /// assertion that told them apart would report the lane as wrong for
    /// capitalising a city. Leave it off — the default — wherever the
    /// application does canonicalise, because there the case IS the content
    /// and a lower-case booking reference is one the airline does not find.
    ///
    /// It never applies to `unchanged` or `absent`, which compare no text.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ignore_case: bool,
}

impl StateExpectation {
    /// Checks the expectation asks exactly one thing.
    ///
    /// # Errors
    ///
    /// [`CorpusError::Invalid`] when it asks more than one thing, or none at
    /// all. None is the dangerous one: it would read as a strict assertion and
    /// check nothing.
    pub fn validate(&self) -> Result<(), CorpusError> {
        if self.ignore_case && self.equals.is_none() && self.one_of.is_empty() {
            return Err(CorpusError::Invalid {
                field: "expect.case_state".to_owned(),
                reason: String::from(
                    "`ignore_case` says how to compare a value, so it needs `equals` or `one_of`: \
                     `unchanged` and `absent` compare no text",
                ),
            });
        }
        match u8::from(self.equals.is_some())
            + u8::from(!self.one_of.is_empty())
            + u8::from(self.unchanged)
            + u8::from(self.absent)
        {
            1 => Ok(()),
            0 => Err(CorpusError::Invalid {
                field: "expect.case_state".to_owned(),
                reason: String::from(
                    "a state expectation with none of `equals`, `one_of`, `unchanged` or \
                     `absent` asserts nothing",
                ),
            }),
            _ => Err(CorpusError::Invalid {
                field: "expect.case_state".to_owned(),
                reason: String::from(
                    "a path is expected to hold a value, one of some values, to be \
                     unchanged, or to hold nothing — exactly one of the four",
                ),
            }),
        }
    }
}

/// One case seeded before the turn.
///
/// The state crosses as JSON because the harness owns the domain types and this
/// crate does not: an item file for a trip and one for a traveler differ
/// only in what this value contains.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseSeed {
    /// Which workflow the case belongs to.
    pub workflow: WorkflowKey,
    /// Identifier within the workflow.
    pub case_id: CaseId,
    /// Server-authored label the model and the cards see instead of the id.
    pub label: String,
    /// Revision the case starts at.
    #[serde(default = "one")]
    pub revision: u64,
    /// The persisted state, as the workflow's own JSON.
    pub state: serde_json::Value,
    /// The conversation that opened this case, named rather than identified.
    ///
    /// Absent means the one the turn happens in, which is what almost every
    /// item wants. A name — any name — means a different one, and the harness
    /// mints an identifier per distinct name: a file cannot know an identifier
    /// that is created while the corpus runs, and asking it to would make every
    /// item unrunnable on a second machine.
    ///
    /// «This record belongs to another conversation» is a scenario of its own: the
    /// record is reachable and nameable there by design, so an old case can be
    /// resumed, and it is not the subject of the turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
}

impl CaseSeed {
    /// Checks the seed is one a harness could apply.
    ///
    /// # Errors
    ///
    /// [`CorpusError::Invalid`] when the state is not a JSON object.
    pub fn validate(&self) -> Result<(), CorpusError> {
        if !self.state.is_object() {
            return Err(CorpusError::invalid(
                "setup.cases.state",
                "a seeded state must be a JSON object",
            ));
        }
        Ok(())
    }
}

const fn one() -> u64 {
    1
}

/// The turn the person takes (spec §9: text and a card answer may coexist).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TurnSpec {
    /// What the person typed.
    pub text: Option<String>,
    /// The card they clicked, named by the case it belongs to rather than by an
    /// identifier no file could know in advance.
    pub reply: Option<CardReplySpec>,
    /// The origin token a surface issued (spec §12.4).
    pub origin: Option<OriginSpec>,
    /// The user within the tenant. The tenant itself is the harness's business.
    pub user_id: Option<String>,
    /// Locale of the person.
    pub locale: Option<Locale>,
    /// A change from outside the conversation, taken in place of a person's turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external: Option<ExternalSpec>,
}

impl TurnSpec {
    /// Checks the turn carries something.
    ///
    /// # Errors
    ///
    /// [`CorpusError::Invalid`] when neither text nor a card answer is present, or
    /// when an outside change carries anything a person would.
    pub fn validate(&self) -> Result<(), CorpusError> {
        if let Some(external) = &self.external {
            if self.text.is_some() || self.reply.is_some() || self.origin.is_some() {
                return Err(CorpusError::invalid(
                    "external",
                    "an outside change is not a person's turn: it carries no text, card or origin",
                ));
            }
            return external.validate();
        }
        if self.text.is_none() && self.reply.is_none() {
            return Err(CorpusError::invalid(
                "turn",
                "a turn needs text, a card answer, or both",
            ));
        }
        Ok(())
    }
}

/// A change to a record made outside the conversation, as its own system makes it: an
/// airline re-quoting a fare. The command is the domain's own, as the workflow serializes
/// it, and never passes through understanding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalSpec {
    /// Workflow of the record.
    pub workflow: WorkflowKey,
    /// The record.
    pub case_id: CaseId,
    /// The domain command.
    pub command: serde_json::Value,
}

impl ExternalSpec {
    fn validate(&self) -> Result<(), CorpusError> {
        if self.workflow.as_str().trim().is_empty() || self.case_id.as_str().trim().is_empty() {
            return Err(CorpusError::invalid(
                "external",
                "an outside change names its workflow and record",
            ));
        }
        if self.command.is_null() {
            return Err(CorpusError::invalid(
                "external.command",
                "the command is missing",
            ));
        }
        Ok(())
    }
}

/// A click, named by what it answers rather than by an identifier.
///
/// A card's [`InteractionId`](turnframe_core::ids::InteractionId) is minted at
/// run time, so no file can name one. What a file *can* name is the case whose
/// blocking card is being answered, which is how a person describes it anyway.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CardReplySpec {
    /// Workflow of the case whose blocking card is answered.
    pub workflow: WorkflowKey,
    /// Case whose blocking card is answered; absent, the one blocking card open on a
    /// case of the workflow, for a case the conversation created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_id: Option<CaseId>,
    /// The stored option chosen.
    pub option: OptionId,
    /// Free-form value, when the stored option permits one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeform: Option<String>,
}

/// A server-issued origin token (spec §12.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginSpec {
    /// The opaque token.
    pub token: String,
    /// Which surface produced it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
}

/// What must be true after the turn, checked without a model (spec §27.6).
///
/// Every field is optional and every one of them means the same thing when
/// absent: *this item does not assert on it*. A field that is present is an
/// exact claim — `commands = []` asserts that the turn compiled no command at
/// all, which is a different and much stronger statement than leaving the key
/// out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Expectations {
    /// Whether the turn must complete at all. Defaults to
    /// [`OutcomeExpectation::Succeeds`], so a crashed turn fails an item even
    /// when every other expectation is trivially satisfied by the wreckage.
    pub outcome: OutcomeExpectation,
    /// The acts the message was understood to ask for, in order.
    pub acts: Option<Vec<ActExpectation>>,
    /// How each act's target resolved.
    pub target_resolution: Vec<TargetExpectation>,
    /// The command types journaled this turn, in admission order.
    pub commands: Option<Vec<String>>,
    /// The event types committed this turn, in append order.
    pub events: Option<Vec<String>>,
    /// The revision each case ends the turn at.
    pub case_revision: Vec<RevisionExpectation>,
    /// The status of the cards on a case.
    pub interaction_status: Vec<InteractionStatusExpectation>,
    /// What a case's state says after the turn, field by field.
    ///
    /// The two questions it answers are not the same one. `equals` asks what a
    /// value ended up being, which is how a scenario about a user changing their
    /// mind, Porto and then Lisbon, can say which one survived; the events
    /// alone cannot, because both stories commit the same event. `unchanged`
    /// asks that a value did not move at all, which is what a turn whose correct
    /// answer writes nothing needs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub case_state: Vec<StateExpectation>,
    /// What some case of a workflow holds at the end, seeded or created by the
    /// conversation, for a case whose identifier no file can know.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workflow_state: Vec<WorkflowStateExpectation>,
    /// How many cases of a workflow exist at the end, seeded and created.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub case_count: Vec<CaseCountExpectation>,
    /// The kinds of response block, in order.
    pub blocks: Option<Vec<BlockKind>>,
    /// The phase the turn finished in.
    pub turn_phase: Option<TurnPhase>,
    /// Effects that must **not** appear.
    pub forbid: ForbiddenEffects,
    /// What each understanding task must make of the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub understanding: Option<crate::understanding::UnderstandingExpectation>,
}

impl Expectations {
    /// Checks the expectations do not contradict each other.
    ///
    /// # Errors
    ///
    /// [`CorpusError::Invalid`] when an act names an operation its kind cannot
    /// carry, when a resolution names a case it cannot have, or when the same
    /// command or event is both required and forbidden.
    pub fn validate(&self) -> Result<(), CorpusError> {
        for act in self.acts.iter().flatten() {
            act.validate()?;
        }
        for target in &self.target_resolution {
            target.validate()?;
        }
        for state in &self.case_state {
            state.validate()?;
        }
        contradiction("commands", self.commands.as_deref(), &self.forbid.commands)?;
        contradiction("events", self.events.as_deref(), &self.forbid.events)?;
        Ok(())
    }

    /// Returns `true` when nothing at all is asserted deterministically.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.outcome == OutcomeExpectation::Succeeds
            && self.acts.is_none()
            && self.target_resolution.is_empty()
            && self.commands.is_none()
            && self.events.is_none()
            && self.case_revision.is_empty()
            && self.interaction_status.is_empty()
            && self.case_state.is_empty()
            && self.workflow_state.is_empty()
            && self.case_count.is_empty()
            && self.blocks.is_none()
            && self.turn_phase.is_none()
            && self.forbid.is_empty()
            && self.understanding.is_none()
    }
}

fn contradiction(
    field: &'static str,
    required: Option<&[String]>,
    forbidden: &[String],
) -> Result<(), CorpusError> {
    let Some(required) = required else {
        return Ok(());
    };
    if let Some(clash) = forbidden.iter().find(|name| required.contains(name)) {
        return Err(CorpusError::Invalid {
            field: field.to_owned(),
            reason: format!("`{clash}` is both required and forbidden"),
        });
    }
    Ok(())
}

/// Whether the turn is expected to complete.
///
/// A crashed turn satisfies "no command was journaled" for the wrong reason, so
/// the default is that a turn must succeed and an item that means otherwise has
/// to say so.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OutcomeExpectation {
    /// The orchestrator must return an assistant turn.
    #[default]
    Succeeds,
    /// The orchestrator must fail, with any error.
    Fails,
    /// The orchestrator must fail with this stable error code.
    FailsWith(String),
}

/// Effects an item asserts must not happen.
///
/// This is the assertion the specification calls out by name: not "the turn did
/// what I wanted" but "the turn did **not** do the dangerous thing". A scenario
/// that asks a question about a trip must not rebook it, and the only way
/// to test that is to say so.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ForbiddenEffects {
    /// Command types that must not be journaled.
    pub commands: Vec<String>,
    /// Event types that must not be committed.
    pub events: Vec<String>,
}

impl ForbiddenEffects {
    /// Returns `true` when nothing is forbidden.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty() && self.events.is_empty()
    }
}

/// One expected normalized act.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ActExpectation {
    /// The act variant.
    pub kind: ActKind,
    /// The operation, for the kinds that name one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// Other shapes that satisfy this position just as well.
    ///
    /// For the turns where more than one act is a CORRECT reading, which is not
    /// the same as a turn nobody decided about: a request may have two doors,
    /// and an expectation admitting one of them reports the other as a defect.
    ///
    /// Not a way to assert less. Each alternative is a whole shape, written out
    /// and validated like the first, so a reader sees the closed set of
    /// readings somebody decided were right — never «any act will do».
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub or: Vec<ActExpectation>,
}

impl ActExpectation {
    /// Checks the operation is one this kind can carry.
    ///
    /// # Errors
    ///
    /// [`CorpusError::Invalid`] when an operation is named on a kind that has
    /// none — a mistake that would otherwise pass silently, because the
    /// observed act has no operation to disagree with.
    pub fn validate(&self) -> Result<(), CorpusError> {
        if self.operation.is_some() && !self.kind.carries_operation() {
            return Err(CorpusError::Invalid {
                field: "expect.acts.operation".to_owned(),
                reason: format!("`{}` acts do not name an operation", self.kind),
            });
        }
        for alternative in &self.or {
            alternative.validate()?;
        }
        Ok(())
    }

    /// Whether `kind` and `operation` are one of the shapes this position
    /// admits.
    #[must_use]
    pub fn admits(&self, kind: &str, operation: Option<&String>) -> bool {
        let matches = self.kind.as_str() == kind
            && self
                .operation
                .as_ref()
                .is_none_or(|wanted| Some(wanted) == operation);
        matches || self.or.iter().any(|other| other.admits(kind, operation))
    }
}

/// The act variants an item can expect, named exactly as they appear in JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ActKind {
    /// Apply a registered operation to a target.
    ApplyOperation,
    /// Start a new case.
    StartWorkflow,
    /// Cancel an earlier act.
    CancelOperation,
    /// Answer the active card with typed text.
    AnswerActiveInteractionFromText,
    /// Pick a target in reply to a selection card.
    SelectTarget,
}

impl ActKind {
    /// The snake-case name, matching
    /// [`UnderstoodAct::kind_name`](turnframe_core::understanding::UnderstoodAct::kind_name).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApplyOperation => "apply_operation",
            Self::StartWorkflow => "start_workflow",
            Self::CancelOperation => "cancel_operation",
            Self::AnswerActiveInteractionFromText => "answer_active_interaction_from_text",
            Self::SelectTarget => "select_target",
        }
    }

    /// Returns `true` for the kinds that can name an operation.
    #[must_use]
    pub const fn carries_operation(self) -> bool {
        matches!(self, Self::ApplyOperation | Self::CancelOperation)
    }
}

impl fmt::Display for ActKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How one act's target must resolve (spec §12.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetExpectation {
    /// Position of the act in the normalized plan.
    pub act_index: usize,
    /// The resolution.
    pub resolution: ResolutionKind,
    /// The case it resolved to, for the resolutions that name one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case_id: Option<CaseId>,
}

impl TargetExpectation {
    /// Checks the case identifier is one this resolution can carry.
    ///
    /// # Errors
    ///
    /// [`CorpusError::Invalid`] when a case is named on an `ambiguous`,
    /// `missing` or `unauthorized` resolution, none of which has one.
    pub fn validate(&self) -> Result<(), CorpusError> {
        if self.case_id.is_some() && !self.resolution.carries_case() {
            return Err(CorpusError::Invalid {
                field: "expect.target_resolution.case_id".to_owned(),
                reason: format!("a `{}` resolution names no case", self.resolution),
            });
        }
        Ok(())
    }
}

/// The resolutions of [`TargetResolution`](turnframe_core::target::TargetResolution),
/// as an item file names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResolutionKind {
    /// Exactly one authorized case.
    Exact,
    /// Several authorized cases; the runtime must never pick one (I8).
    Ambiguous,
    /// The token was issued this turn but the case is gone.
    Missing,
    /// Unknown token, or another tenant's.
    Unauthorized,
    /// The case moved past the revision the token was issued at.
    Stale,
}

impl ResolutionKind {
    /// The snake-case name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Ambiguous => "ambiguous",
            Self::Missing => "missing",
            Self::Unauthorized => "unauthorized",
            Self::Stale => "stale",
        }
    }

    /// Returns `true` for the resolutions that carry a case reference.
    #[must_use]
    pub const fn carries_case(self) -> bool {
        matches!(self, Self::Exact | Self::Stale)
    }
}

impl fmt::Display for ResolutionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A value some case of a workflow must hold at the end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowStateExpectation {
    /// The workflow.
    pub workflow: WorkflowKey,
    /// JSON Pointer into a case's state.
    pub path: String,
    /// The value that path must hold in at least one case.
    pub equals: serde_json::Value,
    /// Compare strings without regard to case.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ignore_case: bool,
}

/// How many cases of a workflow must exist at the end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseCountExpectation {
    /// The workflow.
    pub workflow: WorkflowKey,
    /// How many.
    pub count: usize,
}

/// The revision a case must end the turn at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionExpectation {
    /// Workflow of the case.
    pub workflow: WorkflowKey,
    /// The case.
    pub case_id: CaseId,
    /// The revision. Equal to the seeded one means "nothing committed here".
    pub revision: u64,
}

/// The statuses the cards of a case must have, in creation order (spec §15.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionStatusExpectation {
    /// Workflow of the case.
    pub workflow: WorkflowKey,
    /// The case.
    pub case_id: CaseId,
    /// The statuses, oldest card first. An empty list asserts the case has no
    /// cards at all.
    pub statuses: Vec<InteractionStatus>,
}

/// The kinds of response block, as an item file names them (spec §18.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BlockKind {
    /// Model-authored answer.
    Answer,
    /// Model-authored transition.
    Transition,
    /// Server-authored receipt.
    Receipt,
    /// Server-authored notice.
    Notice,
    /// A persisted card.
    Interaction,
    /// An artifact.
    Artifact,
    /// A block this crate does not know about yet, so a core addition does not
    /// silently read as one of the above.
    Other,
}

impl BlockKind {
    /// The kind of a rendered block.
    #[must_use]
    pub const fn of(block: &ResponseBlock) -> Self {
        match block {
            ResponseBlock::Answer(_) => Self::Answer,
            ResponseBlock::Transition(_) => Self::Transition,
            ResponseBlock::Receipt(_) => Self::Receipt,
            ResponseBlock::Notice(_) => Self::Notice,
            ResponseBlock::Interaction(_) => Self::Interaction,
            ResponseBlock::Artifact(_) => Self::Artifact,
            _ => Self::Other,
        }
    }

    /// The snake-case name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::Transition => "transition",
            Self::Receipt => "receipt",
            Self::Notice => "notice",
            Self::Interaction => "interaction",
            Self::Artifact => "artifact",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for BlockKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The optional `suite.toml` (or `suite.json`) beside a directory of items.
///
/// It exists so a corpus whose seeded states are all written by the same
/// projector says so **once**, instead of repeating the table in every file —
/// where one forgotten line is a silently unpaired item.
///
/// ```toml
/// # corpus/trip/suite.toml
/// [provenance]
/// setup = "derived"
/// ```
///
/// The blanket applies to each item that actually has the part; an item with no
/// seeded cases is simply unaffected, rather than failing to load. An item's own
/// declaration wins over the directory's, part by part, so one item can say
/// `setup = "recorded"` inside a directory that derives everything else.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SuiteManifest {
    /// Where each part came from, for every item in the directory that has it.
    pub provenance: Provenance,
}

/// The file name [`Suite::load_dir`] reads a [`SuiteManifest`] from, without
/// its extension.
const MANIFEST_STEM: &str = "suite";

/// A set of items with a name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    /// What the suite is called, in reports and baselines.
    pub name: String,
    /// The items, in the order they run.
    pub items: Vec<EvalItem>,
}

impl Suite {
    /// Builds a suite, validating every item and refusing duplicate
    /// identifiers.
    ///
    /// # Errors
    ///
    /// * [`CorpusError::Invalid`] from any item's own validation;
    /// * [`CorpusError::DuplicateId`] when two items share an identifier, which
    ///   would make a report join two different measurements.
    pub fn new(name: impl Into<String>, items: Vec<EvalItem>) -> Result<Self, CorpusError> {
        let suite = Self {
            name: name.into(),
            items,
        };
        for (index, item) in suite.items.iter().enumerate() {
            item.validate()?;
            if suite.items[..index].iter().any(|other| other.id == item.id) {
                return Err(CorpusError::DuplicateId {
                    id: item.id.clone(),
                });
            }
        }
        Ok(suite)
    }

    /// [`Suite::new`], with a blanket [`Provenance`] folded into every item that
    /// has the part.
    ///
    /// Each item is validated with the declaration *it* wrote before the blanket
    /// is folded in, so a typo in a file is still an error naming that file's
    /// field, and the blanket is never one. An item that declared a part itself
    /// keeps its own answer; the blanket only fills in the parts the item left
    /// authored.
    ///
    /// # Errors
    ///
    /// Whatever [`Suite::new`] raises.
    pub fn with_provenance(
        name: impl Into<String>,
        mut items: Vec<EvalItem>,
        provenance: Provenance,
    ) -> Result<Self, CorpusError> {
        for item in &mut items {
            item.validate()?;
            for part in provenance.declared() {
                if item.carries(part) && item.provenance.of(part) == PartProvenance::Authored {
                    item.provenance.set(part, provenance.of(part));
                }
            }
        }
        Self::new(name, items)
    }

    /// Loads one item from a `.toml` or `.json` file.
    ///
    /// # Errors
    ///
    /// * [`CorpusError::Read`] when the file cannot be read;
    /// * [`CorpusError::UnknownFormat`] when the extension is neither;
    /// * [`CorpusError::Parse`] when the document is malformed or carries a key
    ///   this crate does not understand;
    /// * [`CorpusError::Invalid`] when the item parses but cannot mean
    ///   anything.
    pub fn load_item(path: impl AsRef<Path>) -> Result<EvalItem, CorpusError> {
        let item: EvalItem = read_document(path.as_ref())?;
        item.validate()?;
        Ok(item)
    }

    /// Loads the [`SuiteManifest`] of a directory, if it has one.
    ///
    /// The manifest is `suite.toml` or `suite.json` beside the items. A
    /// directory without one has no blanket declaration, which is the ordinary
    /// case.
    ///
    /// # Errors
    ///
    /// * [`CorpusError::Read`] when the file exists and cannot be read;
    /// * [`CorpusError::Parse`] when it is not a manifest, including when it
    ///   names a part this crate does not know.
    pub fn load_manifest(dir: impl AsRef<Path>) -> Result<SuiteManifest, CorpusError> {
        for extension in ["toml", "json"] {
            let path = dir.as_ref().join(MANIFEST_STEM).with_extension(extension);
            if path.is_file() {
                return read_document(&path);
            }
        }
        Ok(SuiteManifest::default())
    }

    /// Loads every `.toml` and `.json` item in a directory, in file-name order,
    /// with the directory's [`SuiteManifest`] applied.
    ///
    /// A `suite.toml` or `suite.json` in the directory is the manifest, not an
    /// item.
    ///
    /// # Errors
    ///
    /// * [`CorpusError::Read`] when the directory or one of its files cannot be
    ///   read;
    /// * whatever [`Suite::load_manifest`], [`Suite::load_item`] and
    ///   [`Suite::with_provenance`] raise.
    pub fn load_dir(name: impl Into<String>, dir: impl AsRef<Path>) -> Result<Self, CorpusError> {
        let dir = dir.as_ref();
        let manifest = Self::load_manifest(dir)?;
        let mut paths = Vec::new();
        let entries = std::fs::read_dir(dir).map_err(|error| CorpusError::Read {
            path: dir.to_path_buf(),
            message: error.to_string(),
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| CorpusError::Read {
                path: dir.to_path_buf(),
                message: error.to_string(),
            })?;
            let path = entry.path();
            if path.file_stem().and_then(|stem| stem.to_str()) == Some(MANIFEST_STEM) {
                continue;
            }
            if matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("toml" | "json")
            ) {
                paths.push(path);
            }
        }
        paths.sort();
        let items = paths
            .iter()
            .map(Self::load_item)
            .collect::<Result<Vec<_>, _>>()?;
        Self::with_provenance(name, items, manifest.provenance)
    }

    /// The items a selection runs, in suite order.
    #[must_use]
    pub fn select(&self, selection: &SelectionConfig) -> Vec<&EvalItem> {
        self.items
            .iter()
            .filter(|item| selection.selects(&item.id, &item.tags))
            .collect()
    }

    /// One item by identifier.
    #[must_use]
    pub fn get(&self, id: &ItemId) -> Option<&EvalItem> {
        self.items.iter().find(|item| &item.id == id)
    }

    /// Every tag used in the suite, sorted and deduplicated.
    #[must_use]
    pub fn tags(&self) -> Vec<Tag> {
        let mut tags: Vec<Tag> = self
            .items
            .iter()
            .flat_map(|item| item.tags.iter().cloned())
            .collect();
        tags.sort();
        tags.dedup();
        tags
    }
}

/// Reads one `.toml` or `.json` document, refusing anything it does not fully
/// understand.
fn read_document<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, CorpusError> {
    let source = std::fs::read_to_string(path).map_err(|error| CorpusError::Read {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("toml") => toml::from_str(&source).map_err(|error| CorpusError::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
        Some("json") => serde_json::from_str(&source).map_err(|error| CorpusError::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
        _ => Err(CorpusError::UnknownFormat {
            path: path.to_path_buf(),
        }),
    }
}

/// Why a corpus could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CorpusError {
    /// The file or directory could not be read.
    #[error("corpus path {path} could not be read: {message}")]
    Read {
        /// The path that was tried.
        path: PathBuf,
        /// What the filesystem said.
        message: String,
    },
    /// The extension is neither `.toml` nor `.json`.
    #[error("corpus file {path} is neither .toml nor .json")]
    UnknownFormat {
        /// The path that was tried.
        path: PathBuf,
    },
    /// The document is malformed, or carries a key this crate does not know.
    #[error("corpus file {path} could not be parsed: {message}")]
    Parse {
        /// The file.
        path: PathBuf,
        /// What the parser said, including the unknown key when there was one.
        message: String,
    },
    /// The item parses but cannot mean anything.
    #[error("corpus item is invalid at `{field}`: {reason}")]
    Invalid {
        /// Which field.
        field: String,
        /// Why it cannot stand.
        reason: String,
    },
    /// Two items share an identifier.
    #[error("corpus contains two items with the identifier `{id}`")]
    DuplicateId {
        /// The identifier used twice.
        id: ItemId,
    },
}

impl CorpusError {
    fn invalid(field: &str, reason: &str) -> Self {
        Self::Invalid {
            field: field.to_owned(),
            reason: reason.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_world_can_hold_a_record_that_is_not_a_case() {
        let item: EvalItem = toml::from_str(
            r#"
            id = "traveler.delete_a_registered_one"
            name = "Deleting a traveler that is already registered"

            [[setup.records]]
            kind = "traveler"
            data = { full_name = "Luca Ferri", loyalty_number = "AZ2345678" }

            [turn]
            text = "elimina il viaggiatore Luca Ferri"
            "#,
        )
        .expect("si carica");
        item.validate().expect("è coerente");
        assert_eq!(item.setup.records.len(), 1);
        assert_eq!(item.setup.records[0].kind, "traveler");
    }

    #[test]
    fn a_record_without_its_kind_is_refused() {
        // The kind is how a harness knows what to do with the payload. Without
        // it the record would be seeded by nobody and the item would run in a
        // world missing exactly the thing it is about.
        let refused = toml::from_str::<EvalItem>(
            r#"
            id = "traveler.kindless"
            name = "A record with no kind"

            [[setup.records]]
            data = { full_name = "Rossi" }

            [turn]
            text = "ciao"
            "#,
        );
        assert!(refused.is_err());
    }

    #[test]
    fn an_item_asserting_only_state_is_not_empty() {
        // `is_empty` is a list, a list falls behind, and an expectation missing
        // from it makes a real item read as one that asserts nothing.
        let item: EvalItem = toml::from_str(
            r#"
            id = "traveler.only_state"
            name = "Only a state expectation"

            [[expect.case_state]]
            case_id = "c-1"
            path = "/fields/email/value"
            unchanged = true

            [turn]
            text = "ciao"
            "#,
        )
        .expect("si carica");
        assert!(!item.expect.is_empty(), "questo item assertisce qualcosa");
    }

    #[test]
    fn a_state_expectation_that_asks_nothing_is_refused() {
        // The dangerous shape: it reads like a strict assertion and checks
        // nothing, so an item carrying it looks measured and is not.
        let refused = toml::from_str::<EvalItem>(
            r#"
            id = "traveler.silent"
            name = "A state expectation with neither side"

            [[expect.case_state]]
            case_id = "c-1"
            path = "/fields/email/value"

            [turn]
            text = "ciao"
            "#,
        )
        .expect("si carica")
        .validate();
        assert!(refused.is_err(), "deve essere rifiutata in validazione");
    }

    #[test]
    fn a_state_expectation_cannot_ask_both_at_once() {
        let refused = toml::from_str::<EvalItem>(
            r#"
            id = "traveler.both"
            name = "Both at once"

            [[expect.case_state]]
            case_id = "c-1"
            path = "/fields/email/value"
            equals = "a@b.it"
            unchanged = true

            [turn]
            text = "ciao"
            "#,
        )
        .expect("si carica")
        .validate();
        assert!(refused.is_err());
    }

    #[test]
    fn a_state_expectation_carries_either_side() {
        let item: EvalItem = toml::from_str(
            r#"
            id = "traveler.mind_changed"
            name = "The second gate won"

            [[expect.case_state]]
            case_id = "c-1"
            path = "/fields/gate/value"
            equals = "Gate B14"

            [[expect.case_state]]
            case_id = "c-1"
            path = "/fields/email/value"
            unchanged = true

            [turn]
            text = "sì quella"
            "#,
        )
        .expect("si carica");
        item.validate().expect("è coerente");
        assert_eq!(item.expect.case_state.len(), 2);
    }

    #[test]
    fn a_setup_can_carry_what_was_already_said() {
        let item: EvalItem = toml::from_str(
            r#"
            id = "traveler.changed_their_mind"
            name = "The user contradicts something they said earlier"

            [[setup.history]]
            user = "il viaggiatore è Luca Ferri"
            assistant = "Va bene. Mi serve anche l'indirizzo."

            [[setup.history]]
            user = "anzi no, è la Bianchi"

            [turn]
            text = "sì quella, vai avanti"
            "#,
        )
        .expect("l'item si carica");
        item.validate().expect("l'item è coerente");
        assert_eq!(item.setup.history.len(), 2);
        assert_eq!(
            item.setup.history[0].assistant.as_deref(),
            Some("Va bene. Mi serve anche l'indirizzo.")
        );
        assert!(
            item.setup.history[1].assistant.is_none(),
            "un turno senza risposta è il caso che rende utile questo campo"
        );
    }

    #[test]
    fn a_setup_without_history_has_none_rather_than_an_empty_turn() {
        let item: EvalItem = toml::from_str(
            r#"
            id = "traveler.plain"
            name = "No history at all"

            [turn]
            text = "ciao"
            "#,
        )
        .expect("l'item si carica");
        assert!(item.setup.history.is_empty());
    }

    #[test]
    fn a_misspelled_side_of_an_exchange_is_refused() {
        // `assistent` would otherwise seed a turn the assistant never answered,
        // which is a different scenario from the one the author wrote — and a
        // plausible one, so nothing downstream would look wrong.
        let refused = toml::from_str::<EvalItem>(
            r#"
            id = "traveler.typo"
            name = "A typo"

            [[setup.history]]
            user = "ciao"
            assistent = "ciao a te"

            [turn]
            text = "ciao"
            "#,
        );
        assert!(refused.is_err(), "una chiave sconosciuta è un errore");
    }

    #[test]
    fn a_case_says_which_conversation_opened_it_by_name() {
        let item: EvalItem = toml::from_str(
            r#"
            id = "trip.written_from_another_chat"
            name = "A draft opened elsewhere is not this turn's subject"

            [[setup.cases]]
            workflow = "trip"
            case_id = "trip-1"
            label = "Trip 1"
            state = { status = "draft" }
            conversation = "the other chat"

            [turn]
            text = "add a line for 100 euro"
            "#,
        )
        .expect("l'item si carica");
        item.validate().expect("l'item è coerente");
        assert_eq!(
            item.setup.cases[0].conversation.as_deref(),
            Some("the other chat")
        );
    }

    #[test]
    fn a_case_without_one_belongs_to_the_turns_own_conversation() {
        let item: EvalItem = toml::from_str(
            r#"
            id = "trip.ordinary"
            name = "The ordinary case"

            [[setup.cases]]
            workflow = "trip"
            case_id = "trip-1"
            label = "Trip 1"
            state = { status = "draft" }

            [turn]
            text = "add a line for 100 euro"
            "#,
        )
        .expect("l'item si carica");
        assert!(
            item.setup.cases[0].conversation.is_none(),
            "l'assenza è il caso normale e non va confusa con un nome vuoto"
        );
    }

    #[test]
    fn a_misspelled_conversation_key_is_still_refused() {
        // The loader's whole posture: a key it does not know is an error. A
        // corpus that shrugged at `converstaion` would quietly run the scenario
        // it was written to avoid — the record in the turn's own chat — and
        // report it green.
        let refused = toml::from_str::<EvalItem>(
            r#"
            id = "trip.typo"
            name = "A typo"

            [[setup.cases]]
            workflow = "trip"
            case_id = "trip-1"
            label = "Trip 1"
            state = { status = "draft" }
            converstaion = "the other chat"

            [turn]
            text = "hello"
            "#,
        );
        assert!(refused.is_err(), "una chiave sconosciuta è un errore");
    }

    use super::*;

    fn item(body: &str) -> Result<EvalItem, String> {
        let parsed: EvalItem = toml::from_str(body).map_err(|error| error.to_string())?;
        parsed.validate().map_err(|error| error.to_string())?;
        Ok(parsed)
    }

    const MINIMAL: &str = r#"
id = "a"
name = "A scenario"
[turn]
text = "hello"
"#;

    #[test]
    fn a_minimal_item_loads() {
        let parsed = item(MINIMAL).unwrap();
        assert_eq!(parsed.id, ItemId::new("a"));
        assert!(parsed.expect.is_empty());
        assert!(parsed.judge.is_empty());
    }

    #[test]
    fn an_unknown_field_is_refused_rather_than_skipped() {
        let error = item(&format!("{MINIMAL}unexpected = 1\n")).expect_err("unknown key");
        assert!(error.contains("unexpected"), "{error}");
    }

    #[test]
    fn a_turn_with_nothing_in_it_is_refused() {
        let error = item("id = \"a\"\nname = \"A\"\n[turn]\n").expect_err("empty turn");
        assert!(error.contains("turn"), "{error}");
    }

    #[test]
    fn an_operation_on_a_kind_that_has_none_is_refused() {
        let error = item(&format!(
            "{MINIMAL}[[expect.acts]]\nkind = \"start_workflow\"\noperation = \"trip.rebook\"\n"
        ))
        .expect_err("operation on start_workflow");
        assert!(error.contains("start_workflow"), "{error}");
    }

    #[test]
    fn a_case_on_an_ambiguous_resolution_is_refused() {
        let error = item(&format!(
            "{MINIMAL}[[expect.target_resolution]]\nact_index = 0\nresolution = \"ambiguous\"\ncase_id = \"trip-1\"\n"
        ))
        .expect_err("case on ambiguous");
        assert!(error.contains("ambiguous"), "{error}");
    }

    #[test]
    fn a_command_both_required_and_forbidden_is_refused() {
        let error = item(&format!(
            "{MINIMAL}[expect]\ncommands = [\"trip.rebook\"]\nforbid = {{ commands = [\"trip.rebook\"] }}\n"
        ))
        .expect_err("contradiction");
        assert!(error.contains("trip.rebook"), "{error}");
    }

    #[test]
    fn a_seeded_state_must_be_an_object() {
        let error = item(&format!(
            "{MINIMAL}[[setup.cases]]\nworkflow = \"trip\"\ncase_id = \"trip-1\"\nlabel = \"Trip 1\"\nstate = 7\n"
        ))
        .expect_err("scalar state");
        assert!(error.contains("state"), "{error}");
    }

    #[test]
    fn a_suite_refuses_two_items_with_the_same_identifier() {
        let one = item(MINIMAL).unwrap();
        let two = item(MINIMAL).unwrap();
        let error = Suite::new("dup", vec![one, two]).expect_err("duplicate");
        assert!(matches!(error, CorpusError::DuplicateId { .. }), "{error}");
    }

    const WITH_A_CASE: &str = r#"
id = "a"
name = "A scenario"
[turn]
text = "hello"
[[setup.cases]]
workflow = "trip"
case_id = "trip-1"
label = "Trip 1"
state = { status = "draft" }
"#;

    #[test]
    fn a_provenance_declaration_loads_and_reaches_the_fingerprint() {
        let parsed = item(&format!(
            "provenance = {{ setup = \"derived\", turn = \"recorded\" }}\n{WITH_A_CASE}"
        ))
        .unwrap();
        assert_eq!(parsed.provenance.setup, PartProvenance::Derived);
        let fingerprint = parsed.fingerprint();
        assert_eq!(
            fingerprint.provenance_of(ItemPart::Setup),
            PartProvenance::Derived
        );
        assert_eq!(
            fingerprint.provenance_of(ItemPart::Turn),
            PartProvenance::Recorded
        );
        // Anything undeclared is authored, which is the conservative reading.
        assert_eq!(
            fingerprint.provenance_of(ItemPart::Expect),
            PartProvenance::Authored
        );
        assert!(!fingerprint.is_unknown());
    }

    #[test]
    fn only_a_derived_part_may_be_regenerated() {
        // The whole point of the third value: tooling may rewrite what the code
        // under test produced, and may never rewrite what was recorded.
        assert!(PartProvenance::Derived.may_be_regenerated());
        assert!(!PartProvenance::Authored.may_be_regenerated());
        assert!(!PartProvenance::Recorded.may_be_regenerated());
    }

    #[test]
    fn an_unknown_part_name_is_refused_rather_than_skipped() {
        let error = item(&format!(
            "provenance = {{ setpu = \"derived\" }}\n{WITH_A_CASE}"
        ))
        .expect_err("typo");
        assert!(error.contains("setpu"), "{error}");
    }

    #[test]
    fn an_unknown_provenance_is_refused_rather_than_skipped() {
        let error = item(&format!(
            "provenance = {{ setup = \"transcribed\" }}\n{WITH_A_CASE}"
        ))
        .expect_err("unknown value");
        assert!(error.contains("transcribed"), "{error}");
    }

    #[test]
    fn a_part_declared_on_an_item_that_has_none_is_refused() {
        // `setup = "recorded"` on an item with no seeded cases claims testimony
        // that is not there, and a corpus that accepted it would excuse a
        // difference nobody ever recorded.
        let error = item(&format!(
            "provenance = {{ setup = \"recorded\" }}\n{MINIMAL}"
        ))
        .expect_err("no setup");
        assert!(
            error.contains("this item has no `setup`"),
            "the reason must be the missing part, not a stray parse error: {error}"
        );
    }

    /// A world made of what was already said is still a world.
    ///
    /// `carries` read only the cases, which was right when a case was the only
    /// thing a setup could hold. It is not any more: the register and the prior
    /// exchanges are seeded through the same writers, and a scene whose whole
    /// setup is twenty-four turns of conversation had its declaration refused
    /// as a typo.
    #[test]
    fn a_setup_made_only_of_history_or_records_is_still_a_setup() {
        let mut item = item(MINIMAL).expect("the minimal item loads");
        assert!(
            !item.carries(ItemPart::Setup),
            "nothing seeded, nothing said"
        );

        item.setup.history.push(PriorExchange {
            user: "e il viaggiatore di Torino?".to_owned(),
            assistant: None,
        });
        assert!(item.carries(ItemPart::Setup));

        item.setup.history.clear();
        item.setup.records.push(SeededRecord {
            kind: "traveler".to_owned(),
            data: serde_json::json!({"name": "Ferri"}),
        });
        assert!(item.carries(ItemPart::Setup));
    }

    #[test]
    fn a_blanket_declaration_reaches_every_item_that_has_the_part() {
        let with_case = item(WITH_A_CASE).unwrap();
        let mut without_case = item(MINIMAL).unwrap();
        without_case.id = ItemId::new("b");

        let blanket = Provenance {
            setup: PartProvenance::Derived,
            turn: PartProvenance::Recorded,
            ..Provenance::authored()
        };
        let suite = Suite::with_provenance("s", vec![with_case, without_case], blanket).unwrap();

        assert_eq!(suite.items[0].provenance.setup, PartProvenance::Derived);
        assert_eq!(suite.items[0].provenance.turn, PartProvenance::Recorded);
        // The blanket is not an error on an item that has no `setup`; it simply
        // does not apply there.
        assert_eq!(suite.items[1].provenance.setup, PartProvenance::Authored);
        assert_eq!(suite.items[1].provenance.turn, PartProvenance::Recorded);
    }

    #[test]
    fn an_items_own_declaration_wins_over_the_directorys() {
        let recorded = item(&format!(
            "provenance = {{ setup = \"recorded\" }}\n{WITH_A_CASE}"
        ))
        .unwrap();
        let suite = Suite::with_provenance(
            "s",
            vec![recorded],
            Provenance {
                setup: PartProvenance::Derived,
                ..Provenance::authored()
            },
        )
        .unwrap();
        assert_eq!(suite.items[0].provenance.setup, PartProvenance::Recorded);
    }

    #[test]
    fn a_fingerprint_ignores_the_order_the_seeded_state_was_written_in() {
        let one = item(
            "id = \"a\"\nname = \"A\"\n[turn]\ntext = \"hi\"\n[[setup.cases]]\nworkflow = \"trip\"\ncase_id = \"trip-1\"\nlabel = \"L\"\nstate = { alpha = 1, beta = 2 }\n",
        )
        .unwrap();
        let two = item(
            "id = \"a\"\nname = \"A\"\n[turn]\ntext = \"hi\"\n[[setup.cases]]\nworkflow = \"trip\"\ncase_id = \"trip-1\"\nlabel = \"L\"\nstate = { beta = 2, alpha = 1 }\n",
        )
        .unwrap();
        assert!(
            one.fingerprint()
                .differing_parts(&two.fingerprint())
                .is_empty()
        );
    }

    #[test]
    fn a_changed_seeded_state_shows_up_as_a_changed_setup_part() {
        let one = item(WITH_A_CASE).unwrap();
        let two = item(&WITH_A_CASE.replace("draft", "sent")).unwrap();
        assert_eq!(
            one.fingerprint().differing_parts(&two.fingerprint()),
            vec![ItemPart::Setup]
        );
    }

    #[test]
    fn an_unknown_fingerprint_claims_nothing() {
        let known = item(WITH_A_CASE).unwrap().fingerprint();
        let unknown = ItemFingerprint::default();
        assert!(unknown.is_unknown());
        assert!(known.differing_parts(&unknown).is_empty());
        assert!(unknown.differing_parts(&known).is_empty());
    }

    #[test]
    fn selection_narrows_a_suite() {
        let mut tagged = item(MINIMAL).unwrap();
        tagged.tags = vec![Tag::new("trip")];
        let mut other = item(MINIMAL).unwrap();
        other.id = ItemId::new("b");
        other.tags = vec![Tag::new("traveler")];
        let suite = Suite::new("s", vec![tagged, other]).unwrap();

        let selection = SelectionConfig {
            include_tags: vec![Tag::new("trip")],
            ..SelectionConfig::default()
        };
        let selected = suite.select(&selection);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, ItemId::new("a"));
        assert_eq!(suite.tags().len(), 2);
    }
}
