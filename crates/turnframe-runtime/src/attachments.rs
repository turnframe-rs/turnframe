//! The turn's files, on their way to a model.
//!
//! # The half that was missing
//!
//! Attachments already travel end to end at the *plan* level: a turn carries
//! them, the interpretation schema offers `attachment_extraction` as evidence
//! pinned to the identifiers the turn actually has, and an act can name a file
//! so an application's own extractor fetches the bytes and rewrites the command.
//! That is how a document's numbers reach a record without passing through a
//! model at all, and it needs nothing from here.
//!
//! What was missing is the model **seeing** the file. Nothing built a
//! [`ContentPart`] from an attachment, so a turn carrying a photograph reached
//! the model as a sentence mentioning one. An adopter whose extraction is a
//! vision call behind a domain prompt has no fallback when it returns nothing —
//! a receipt photographed at an angle, a scan of a scan, a document that is not
//! what the prompt asks for — except "I cannot read this", on the turn where the
//! user has just done the work of taking the picture.
//!
//! # What is not decided here
//!
//! Which providers can accept what. A part carrying an image makes the request
//! require vision and one carrying a document makes it require documents, both
//! derived from the part itself, so the router picks a candidate that has them
//! and falls back through the chain like any other requirement. A file one
//! vendor refuses and another accepts is therefore already handled, and handled
//! in the one place that knows the vendors.

use std::sync::Arc;

use turnframe_core::ids::{AttachmentId, TurnId};
use turnframe_core::locale::{Locale, LocalizedText};
use turnframe_core::turn::{AttachmentRef, AttachmentSource};
use turnframe_provider::request::ContentPart;

use crate::config::AttachmentConfig;

/// Why a file the turn carried was not put in front of the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NotShown {
    /// The application could not hand over the bytes.
    Unavailable,
    /// The request's declared budget would not take it.
    OverBudget,
    /// No model this deployment can route to accepts its kind.
    ///
    /// Asked before the request is sent rather than discovered by sending it: a
    /// part carrying an image makes a request require vision, and a router with
    /// nothing to serve it fails the call. Attaching a photograph to a
    /// deployment whose models have no vision would then turn "I could not look
    /// at your file" into "the turn failed", on the turn where the user has just
    /// taken the picture.
    Unsupported,
}

/// One file the turn carried and the model was not shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmittedAttachment {
    /// The file.
    pub attachment_id: AttachmentId,
    /// What the user called it, when the upload carried a name.
    pub filename: Option<String>,
    /// Why.
    pub reason: NotShown,
}

/// The turn's files as a model can be given them, and the ones it cannot.
#[derive(Debug, Clone, Default)]
pub struct TurnAttachments {
    /// The parts to append to the request, in the order the turn carried them.
    pub parts: Vec<ContentPart>,
    /// The files that did not make it, in the same order.
    ///
    /// Never silently empty on a turn that dropped something: the whole point of
    /// carrying this beside the parts is that an answer about the wrong document
    /// is worse than an answer about none.
    pub omitted: Vec<OmittedAttachment>,
}

impl TurnAttachments {
    /// Whether the turn carried nothing, or nothing survived.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty() && self.omitted.is_empty()
    }

    /// Fetches every attachment of the turn, in order, within the budget.
    ///
    /// # Order, and why it is the turn's
    ///
    /// The files are taken in the order the user attached them and the budget is
    /// spent in that order, so a turn that overruns loses its last file rather
    /// than an arbitrary one. A file too large for the whole budget is skipped
    /// and the smaller ones behind it are still taken, because dropping the rest
    /// of somebody's upload over one oversized photograph helps nobody.
    ///
    /// # With no source configured
    ///
    /// Nothing is fetched and nothing is reported. A deployment that never
    /// registered an [`AttachmentSource`] has not lost a file it was showing —
    /// it is the behaviour this runtime had before the port existed, and saying
    /// "I could not look at your file" to every such turn would be noise about a
    /// feature nobody asked for.
    pub async fn gather(
        source: Option<&Arc<dyn AttachmentSource>>,
        config: AttachmentConfig,
        turn_id: &TurnId,
        attachments: &[AttachmentRef],
        carries: impl Fn(&ContentPart) -> bool,
    ) -> Self {
        let Some(source) = source else {
            return Self::default();
        };
        let mut gathered = Self::default();
        let mut spent = 0usize;
        for attachment in attachments {
            if config
                .max_files
                .is_some_and(|max| gathered.parts.len() >= max)
            {
                gathered.omit(attachment, NotShown::OverBudget);
                continue;
            }
            let content = match source.fetch(turn_id, attachment).await {
                Ok(content) => content,
                Err(error) => {
                    // A file the application cannot hand over is ordinary: it
                    // may keep the bytes for one turn and never write them down.
                    tracing::info!(
                        target: "turnframe.attachments",
                        attachment = %attachment.attachment_id,
                        error = %error,
                        "an attachment could not be fetched; the turn stands without it"
                    );
                    gathered.omit(attachment, NotShown::Unavailable);
                    continue;
                }
            };
            if config
                .max_total_bytes
                .is_some_and(|max| spent.saturating_add(content.bytes.len()) > max)
            {
                gathered.omit(attachment, NotShown::OverBudget);
                continue;
            }
            let part = ContentPart::inline_bytes(content.media_type, &content.bytes);
            if !carries(&part) {
                gathered.omit(attachment, NotShown::Unsupported);
                continue;
            }
            spent = spent.saturating_add(content.bytes.len());
            gathered.parts.push(part);
        }
        gathered
    }

    fn omit(&mut self, attachment: &AttachmentRef, reason: NotShown) {
        self.omitted.push(OmittedAttachment {
            attachment_id: attachment.attachment_id.clone(),
            filename: attachment.filename.clone(),
            reason,
        });
    }
}

/// What a user is told about a file the model was not shown.
///
/// Deterministic, and reaches them whether or not a model runs — which is the
/// property that matters, because the alternative is prose about a document
/// nobody looked at.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AttachmentCopy {
    /// The application could not hand the file over.
    pub unavailable: LocalizedText,
    /// The file did not fit the budget for one request.
    pub over_budget: LocalizedText,
    /// No model this deployment runs accepts its kind.
    pub unsupported: LocalizedText,
}

impl AttachmentCopy {
    /// The built-in copy: English, with Italian.
    #[must_use]
    pub fn standard() -> Self {
        crate::copy::ServerCopy::translated(Self::english(), "it", ITALIAN)
    }

    /// English alone.
    #[must_use]
    pub fn english() -> Self {
        Self {
            unavailable: LocalizedText::new(
                "I could not open one of the files you sent, so I have not looked at it.",
            ),
            over_budget: LocalizedText::new(
                "One of the files you sent was too large for me to look at.",
            ),
            unsupported: LocalizedText::new("I cannot open files of that kind."),
        }
    }

    /// The sentence for one reason.
    #[must_use]
    pub fn for_reason(&self, reason: NotShown) -> &LocalizedText {
        match reason {
            NotShown::Unavailable => &self.unavailable,
            NotShown::OverBudget => &self.over_budget,
            NotShown::Unsupported => &self.unsupported,
        }
    }

    /// The sentence for one reason, resolved.
    #[must_use]
    pub fn resolved(&self, reason: NotShown, locale: &Locale) -> String {
        self.for_reason(reason).resolve(locale).to_owned()
    }
}

impl Default for AttachmentCopy {
    fn default() -> Self {
        Self::standard()
    }
}

crate::copy::server_copy!(AttachmentCopy, [unavailable, over_budget, unsupported]);

/// The built-in Italian of [`AttachmentCopy`], by field.
const ITALIAN: &[(&str, &str)] = &[
    (
        "unavailable",
        "Non ho potuto aprire uno dei file che hai inviato, quindi non l'ho guardato.",
    ),
    (
        "over_budget",
        "Uno dei file che hai inviato era troppo grande per poterlo guardare.",
    ),
    ("unsupported", "Non posso aprire file di quel tipo."),
];
