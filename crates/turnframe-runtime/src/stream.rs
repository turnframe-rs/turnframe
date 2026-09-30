//! Safe streaming (spec §18.5).
//!
//! Streaming is where a careful runtime is most likely to lie, because the
//! natural thing to send first is the sentence and the sentence is the part
//! that is not true yet. So publication is gated by the turn's own phase:
//!
//! * **Nothing that states an outcome leaves before commit.** A
//!   [`ResponseBlock::Receipt`], and every model-authored block, is refused by
//!   [`PublicationGate`] until the turn reaches
//!   [`TurnPhase::Committed`]. There is no configuration that relaxes it.
//! * **Model-authored text goes out only once the plan is safe to publish.**
//!   Which, for this runtime, is the same moment: composition runs after the
//!   commit bundle has landed.
//! * **Cards and receipts are atomic typed events.** They travel as whole
//!   [`TurnEvent::Block`] values, never as a stream of characters a client
//!   would have to reassemble into a button.
//! * **A click-only turn returns immediately.** [`TurnStream::immediate`]
//!   yields a finished turn with no model call behind it (§9).
//!
//! # Prose arrives whole
//!
//! A reply is reviewed before it is shown, so model-authored text is published as
//! one finished [`TurnEvent::Block`], under the same gate as every other block.
//!
//! # Progress is not an outcome
//!
//! While the turn is interpreting and committing, the only thing on the wire is
//! [`TurnEvent::Phase`]. It is what a client hangs an activity indicator on, and
//! it says nothing about what will happen — which is exactly the point: a phase
//! marker cannot be misread as a receipt.

use std::collections::VecDeque;
use std::fmt;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::Stream;
use tokio::sync::mpsc;
use turnframe_core::replay::TurnPhase;
use turnframe_core::response::{AssistantTurn, ResponseBlock};

/// One thing a client learns while a turn is being handled.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TurnEvent {
    /// The turn moved to a new phase. Progress, never an outcome.
    Phase(TurnPhase),
    /// One decision of the turn's understanding, as it was made. What was understood,
    /// never what happened: it goes out before the commit, like a phase marker.
    Step(Box<turnframe_understand::Step>),
    /// A step said in the turn's language, when `NarrationConfig::steps` asks for it.
    StepSaid {
        /// The step.
        step: Box<turnframe_understand::Step>,
        /// The progress line a model wrote for it.
        text: String,
    },
    /// One finished block, published atomically.
    Block(Box<ResponseBlock>),
    /// The turn is done; this is exactly what was persisted.
    Completed(Box<AssistantTurn>),
    /// The turn failed, with a stable code and no free text.
    Failed {
        /// Stable code from the error family.
        code: String,
    },
}

impl TurnEvent {
    /// The block this event carries, if any.
    #[must_use]
    pub fn block(&self) -> Option<&ResponseBlock> {
        match self {
            Self::Block(block) => Some(block),
            _ => None,
        }
    }

    /// Returns `true` for the two events that end a stream.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed(_) | Self::Failed { .. })
    }
}

/// Returns `true` once a turn's effects are settled, so a block that states an
/// outcome may be published (spec §18.5).
///
/// [`TurnPhase::Executing`] is deliberately not one of them: effects *may*
/// exist there, which is precisely the state in which nothing may be claimed.
#[must_use]
pub const fn publishes_outcomes(phase: TurnPhase) -> bool {
    matches!(
        phase,
        TurnPhase::Committed | TurnPhase::Composed | TurnPhase::Delivered
    )
}

/// Decides what may go on the wire at the phase the turn has reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicationGate {
    phase: TurnPhase,
}

impl PublicationGate {
    /// A gate at the start of a turn.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            phase: TurnPhase::Received,
        }
    }

    /// A gate at an arbitrary phase.
    #[must_use]
    pub const fn at(phase: TurnPhase) -> Self {
        Self { phase }
    }

    /// The phase the gate is at.
    #[must_use]
    pub const fn phase(&self) -> TurnPhase {
        self.phase
    }

    /// Moves the gate to `phase`.
    pub const fn advance(&mut self, phase: TurnPhase) {
        self.phase = phase;
    }

    /// Whether `block` may be published now.
    ///
    /// Receipts and model-authored blocks wait for commit. Notices and cards do
    /// not: a notice is a server statement about what is *not* happening, and a
    /// card is a question, so neither can be mistaken for a success.
    #[must_use]
    pub fn admits(&self, block: &ResponseBlock) -> bool {
        match block {
            ResponseBlock::Notice(_) | ResponseBlock::Interaction(_) => true,
            ResponseBlock::Receipt(_)
            | ResponseBlock::Answer(_)
            | ResponseBlock::Transition(_)
            | ResponseBlock::Artifact(_) => publishes_outcomes(self.phase),
            // A block kind added after this version is treated as if it could
            // state an outcome, which is the only safe reading.
            _ => publishes_outcomes(self.phase),
        }
    }
}

impl Default for PublicationGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Receives the events of a turn as it runs.
pub trait TurnSink: Send + Sync {
    /// Delivers one event. Implementations must not block.
    fn emit(&self, event: TurnEvent);
}

/// A sink that drops everything, for a caller that does not stream.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullSink;

impl TurnSink for NullSink {
    fn emit(&self, _event: TurnEvent) {}
}

/// A sink that records every event, for tests and for a caller that wants the
/// whole trace at the end.
#[derive(Debug, Default)]
pub struct RecordingSink {
    events: Mutex<Vec<TurnEvent>>,
}

impl RecordingSink {
    /// An empty recorder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything emitted so far, in order.
    #[must_use]
    pub fn events(&self) -> Vec<TurnEvent> {
        self.events
            .lock()
            .map(|events| events.clone())
            .unwrap_or_default()
    }

    /// The blocks emitted so far, in order.
    #[must_use]
    pub fn blocks(&self) -> Vec<ResponseBlock> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                TurnEvent::Block(block) => Some(*block),
                _ => None,
            })
            .collect()
    }
}

impl TurnSink for RecordingSink {
    fn emit(&self, event: TurnEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }
}

/// A sink that forwards to a [`TurnStream`].
#[derive(Debug, Clone)]
pub struct ChannelSink {
    sender: mpsc::UnboundedSender<TurnEvent>,
}

impl TurnSink for ChannelSink {
    fn emit(&self, event: TurnEvent) {
        // A closed receiver means the client went away; the turn carries on and
        // still persists its answer.
        let _ = self.sender.send(event);
    }
}

/// Publishes a turn's events through a sink, under the [`PublicationGate`].
///
/// The gate is the reason this type exists rather than a bare sink: the
/// orchestrator advances the phase as it goes, and a block offered too early is
/// dropped instead of published, whatever the caller intended.
pub struct TurnPublisher {
    sink: Arc<dyn TurnSink>,
    gate: Mutex<PublicationGate>,
    live: bool,
}

impl turnframe_understand::StepSink for TurnPublisher {
    fn step(&self, step: turnframe_understand::Step) {
        self.publish_step(step);
    }
}

impl fmt::Debug for TurnPublisher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnPublisher")
            .field("phase", &self.phase())
            .finish_non_exhaustive()
    }
}

impl TurnPublisher {
    /// A publisher writing to `sink`.
    #[must_use]
    pub fn new(sink: Arc<dyn TurnSink>) -> Self {
        Self {
            sink,
            gate: Mutex::new(PublicationGate::new()),
            live: true,
        }
    }

    /// A publisher that drops everything.
    #[must_use]
    pub fn null() -> Self {
        Self {
            sink: Arc::new(NullSink),
            gate: Mutex::new(PublicationGate::new()),
            live: false,
        }
    }

    /// Whether anything is listening: `false` for [`Self::null`].
    ///
    /// [`compose`](crate::compose) asks before it goes looking for a
    /// streaming-capable provider, because a turn nobody is watching has no
    /// reason to prefer one.
    #[must_use]
    pub const fn is_live(&self) -> bool {
        self.live
    }

    /// The phase the gate is at.
    #[must_use]
    pub fn phase(&self) -> TurnPhase {
        self.gate
            .lock()
            .map_or(TurnPhase::Received, |gate| gate.phase())
    }

    /// Announces a phase and moves the gate to it.
    pub fn phase_reached(&self, phase: TurnPhase) {
        if let Ok(mut gate) = self.gate.lock() {
            gate.advance(phase);
        }
        self.sink.emit(TurnEvent::Phase(phase));
    }

    /// Publishes one step of the understanding. Steps are not outcomes, so the gate
    /// never holds them back.
    pub fn publish_step(&self, step: turnframe_understand::Step) {
        self.sink.emit(TurnEvent::Step(Box::new(step)));
    }

    /// Publishes a step as a model said it.
    pub fn step_said(&self, step: turnframe_understand::Step, text: String) {
        self.sink.emit(TurnEvent::StepSaid {
            step: Box::new(step),
            text,
        });
    }

    /// Publishes one block if the gate admits it, and returns whether it went
    /// out.
    pub fn block(&self, block: &ResponseBlock) -> bool {
        let admitted = self.gate.lock().is_ok_and(|gate| gate.admits(block));
        if admitted {
            self.sink.emit(TurnEvent::Block(Box::new(block.clone())));
        }
        admitted
    }

    /// Publishes every block of a composed turn, in order.
    pub fn blocks(&self, turn: &AssistantTurn) {
        for block in &turn.blocks {
            self.block(block);
        }
    }

    /// Ends the stream with the turn that was persisted.
    pub fn completed(&self, turn: &AssistantTurn) {
        self.phase_reached(TurnPhase::Delivered);
        self.sink.emit(TurnEvent::Completed(Box::new(turn.clone())));
    }

    /// Ends the stream with a stable failure code.
    pub fn failed(&self, code: impl Into<String>) {
        if let Ok(mut gate) = self.gate.lock() {
            gate.advance(TurnPhase::Failed);
        }
        self.sink.emit(TurnEvent::Failed { code: code.into() });
    }
}

/// The events of one turn, as a [`Stream`].
///
/// Two shapes come out of the same type: a live stream fed by a
/// [`ChannelSink`], and a finished one built by [`TurnStream::immediate`] for a
/// turn that needed no model call at all.
pub struct TurnStream {
    buffered: VecDeque<TurnEvent>,
    receiver: Option<mpsc::UnboundedReceiver<TurnEvent>>,
}

impl fmt::Debug for TurnStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnStream")
            .field("buffered", &self.buffered.len())
            .field("live", &self.receiver.is_some())
            .finish()
    }
}

impl TurnStream {
    /// A live stream and the sink that feeds it.
    #[must_use]
    pub fn channel() -> (Self, ChannelSink) {
        let (sender, receiver) = mpsc::unbounded_channel();
        (
            Self {
                buffered: VecDeque::new(),
                receiver: Some(receiver),
            },
            ChannelSink { sender },
        )
    }

    /// A finished stream over a turn that is already composed.
    ///
    /// This is the click-only path of spec §18.5: the answer exists before the
    /// client asked for a stream, so every block is published at once and the
    /// stream ends.
    #[must_use]
    pub fn immediate(turn: AssistantTurn) -> Self {
        let mut buffered = VecDeque::with_capacity(turn.blocks.len() + 2);
        buffered.push_back(TurnEvent::Phase(TurnPhase::Delivered));
        for block in &turn.blocks {
            buffered.push_back(TurnEvent::Block(Box::new(block.clone())));
        }
        buffered.push_back(TurnEvent::Completed(Box::new(turn)));
        Self {
            buffered,
            receiver: None,
        }
    }

    /// A finished stream carrying only a failure.
    #[must_use]
    pub fn failed(code: impl Into<String>) -> Self {
        let mut buffered = VecDeque::with_capacity(1);
        buffered.push_back(TurnEvent::Failed { code: code.into() });
        Self {
            buffered,
            receiver: None,
        }
    }

    /// Drains the stream into a vector, for a caller that wants the trace
    /// rather than the incremental delivery.
    pub async fn collect_events(mut self) -> Vec<TurnEvent> {
        let mut events: Vec<TurnEvent> = self.buffered.drain(..).collect();
        if let Some(receiver) = self.receiver.as_mut() {
            while let Some(event) = receiver.recv().await {
                events.push(event);
            }
        }
        events
    }
}

impl Stream for TurnStream {
    type Item = TurnEvent;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(event) = this.buffered.pop_front() {
            return Poll::Ready(Some(event));
        }
        match this.receiver.as_mut() {
            Some(receiver) => receiver.poll_recv(context),
            None => Poll::Ready(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use turnframe_core::event::{OperationalReceipt, ReceiptSeverity};
    use turnframe_core::ids::{BlockId, ConversationId, EventId, ReceiptId, TurnId};
    use turnframe_core::locale::LocalizedText;
    use turnframe_core::response::{GeneratedTransition, ReceiptBlock, ServerNotice};

    use super::*;

    fn receipt_block() -> ResponseBlock {
        ResponseBlock::Receipt(ReceiptBlock {
            block_id: BlockId::from("receipt:1"),
            receipt: OperationalReceipt {
                receipt_id: ReceiptId::nil(),
                event_ids: vec![EventId::nil()],
                severity: ReceiptSeverity::Success,
                title: LocalizedText::new("Sent"),
                body: LocalizedText::new("It went out."),
                status_code: "trip.rebooking_sent".to_owned(),
                artifact_refs: Vec::new(),
            },
        })
    }

    fn transition_block() -> ResponseBlock {
        ResponseBlock::Transition(GeneratedTransition {
            block_id: BlockId::from("transition:0"),
            text: "Right away.".to_owned(),
            facts_used: Vec::new(),
        })
    }

    fn notice_block() -> ResponseBlock {
        ResponseBlock::Notice(ServerNotice {
            block_id: BlockId::from("notice:x"),
            code: "x".to_owned(),
            severity: turnframe_core::response::NoticeSeverity::Info,
            text: LocalizedText::new("nothing was submitted"),
        })
    }

    #[test]
    fn no_outcome_leaves_before_commit() {
        for phase in [
            TurnPhase::Received,
            TurnPhase::Interpreted,
            TurnPhase::Reduced,
            TurnPhase::Executing,
        ] {
            let gate = PublicationGate::at(phase);
            assert!(!gate.admits(&receipt_block()), "{phase:?}");
            assert!(!gate.admits(&transition_block()), "{phase:?}");
            assert!(gate.admits(&notice_block()), "{phase:?}");
        }
    }

    #[test]
    fn everything_is_publishable_once_the_turn_committed() {
        let gate = PublicationGate::at(TurnPhase::Committed);
        assert!(gate.admits(&receipt_block()));
        assert!(gate.admits(&transition_block()));
        assert!(gate.admits(&notice_block()));
    }

    #[test]
    fn the_publisher_drops_a_block_offered_too_early() {
        let sink = Arc::new(RecordingSink::new());
        let publisher = TurnPublisher::new(sink.clone());
        publisher.phase_reached(TurnPhase::Executing);
        assert!(!publisher.block(&receipt_block()));
        assert!(sink.blocks().is_empty());
        publisher.phase_reached(TurnPhase::Committed);
        assert!(publisher.block(&receipt_block()));
        assert_eq!(sink.blocks().len(), 1);
    }

    #[tokio::test]
    async fn a_click_only_turn_streams_without_waiting() {
        let turn = AssistantTurn {
            turn_id: TurnId::nil(),
            conversation_id: ConversationId::nil(),
            blocks: vec![notice_block()],
            subjects: Vec::new(),
            expectations: Vec::new(),
            replay_token: turnframe_core::response::ReplayToken::from("t"),
            done: Vec::new(),
            offers: Vec::new(),
        };
        let events = TurnStream::immediate(turn).collect_events().await;
        assert_eq!(events.len(), 3);
        assert!(events.last().is_some_and(TurnEvent::is_terminal));
    }
}
