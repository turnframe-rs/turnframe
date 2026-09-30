# What the assistant is allowed to say

Response composition runs when everything that could happen has happened. Its job is to say so
accurately, and to ask for what the work needs next. The division of labour is the rule that makes
that possible (§18.2).

The **server** decides whether an action occurred, which fields changed, what the command status
is, what the external submission status is, whether a card exists, what its call to action means,
and which revision the case is at. Those become receipts, notices and interaction blocks, rendered
deterministically from committed events and persisted records. The server also chooses what the
reply asks next.

The **model** writes the words around those blocks: a short acknowledgement and one answer per
question, each from a bounded set of facts code gathered. It may never state an operational outcome
that no receipt backs.

## The acknowledgement

Code gathers the turn's outcome into one small document:

- **done**: each receipt, in its own title and body;
- **not done**: each refusal in the domain's own sentence, each act that changed nothing, each card
  the user declined, each part of the message that was not understood;
- **disputes**: what the user said the assistant got wrong;
- **starting**: a workflow the turn began without writing anything yet;
- **ask**: the one thing to ask next, chosen by code;
- **card**: the card on screen, by its title and buttons: one the turn raised, else one an earlier
  turn left open on a record the turn reached, which the reply points to;
- **next**: what the user may do next, when the record owes nothing and no card is on screen;
- **standing**: where the record of a question no fact answers stands.

The ask is a receipt the user contested, else the first value an act is waiting for, else the first
open obligation of the record the turn is on. A turn that reached no record is on the record its
question is about, else the one the last reply was about, so "where are we?", a "thanks" or a "yes"
keeps the conversation moving. When those records need nothing and the turn acted, or left part of
the message unread, the ask falls to the next record in view that needs something, so a message
the runtime could not follow still ends on the work. A question about what can be done, or about
the values a field takes, proposes in its own answer and sets no ask. When the ask is all the reply
has to say, it follows the answers it builds on. An obligation is asked in the workflow's own
sentence (`WorkflowDefinition::obligation_sentence`), which is also what the server says when no
model writes the reply. A workflow that names the act answering an obligation
(`WorkflowDefinition::obligation_act`: the operation, the values the obligation fixes, the values
the answer gives) has it awaited as that act, so «the company» answers «an extra has no payer
yet: who pays for it?» without the user naming the extra. An ask the last reply asked too, of the
same record, with no refusal to explain it, says it is still needed to go on and offers the
record's next steps beside it (`AskCopy::again`); a value asked again after a refusal gives the
refusal as its reason.

A record that owes nothing more, with no card on screen, ends the reply on what its workflow offers
next (`WorkflowDefinition::next_steps`: each an operation, its words and the arguments already
known). A step is offered only when the domain would take it now: its operation is on offer for
the record and, when its arguments are complete, the act compiles and every command validates
against the state (I22). The offers are recorded on the turn (`AssistantTurn::offers`) and the next
message is read against them first, so «yes, do that» runs the offer on its record. A reply with
nothing else to end on ends on the question to go on (`AskCopy::go_on`): every reply ends on a way
forward (I21).

A request only a record of a workflow can do, when none of its records exists, is not left unread:
understanding is shown what such a record could do (`WorkflowDefinition::record_operations`), and
a request for one that names no record is told there is none yet and offered to open one
(`AskCopy::open_new`), an offer «yes» takes up like any other.

A turn has one reply, the transition block, and it is the message the user reads: what was done,
the answers to the questions, what the notices say, and the ask, in that order. The receipts,
answers and notices stay in the turn as blocks of their own, the structured record an application
can show or audit, and the reply says the same things once, so the next turn reads it back as the
transition alone. A turn that only answers questions replies with the answers as written, and no
acknowledgement is asked for, unless a question about the values one record's field may take
leaves that record owing something: the reply then asks for it too.

The acknowledgement task is shown that document, the answers and notices it gives, the card on
screen (so it does not repeat the card's wording), the turn's locale and tone, and the workflow's
guidance for the phase. It is shown the user's message only when nothing was left undone, and never
the words of a question it was given the answer to: beside a refusal, the words that asked for it
read as the thing done, and a question's words get answered twice.

## The review

A written acknowledgement is reviewed before it is shown. The review answers a yes-or-no checklist,
and only the checks that apply are asked: whether the reply asks the ask and nothing else (when there
is an ask), whether it states an action, a value or a promise the material does not hold, whether it
contradicts the card on screen (when there is one), whether it offers every item next lists (when it
lists anything and there is no ask), whether it ends on a question to go on (when nothing else is
its way forward), and whether it leaves out anything an answer or a notice says (when it gives
any). A reply that fails is written once more with the review's
findings; one that fails again is dropped, and code's own words stand in for it: what was done, the
answers, the notices, then the question. The review is part of
the acknowledgement's profile and can be switched off where a stronger model makes it unnecessary.

## Every question is answered, or explicitly not

§19.4 forbids a question disappearing because an action was also requested. Every question gets its
own answer task, run before the reply that gives the answers, and exactly one answer block, carrying its basis and a
status: answered, unsupported, source unavailable, withheld, or not written. An answer rests on the
facts of the records its question is about: which record and where it stands, what it holds, what
it still needs, what can be done, and what a knowledge source returned. A question about records
that names none is about the records in view. What the turn did is the acknowledgement's to say, so a refusal
is said once. A question about what can be done is answered from the operations on offer, and when
no model writes that answer, the server lists them itself. A question no fact answers is told
where its record stands, what it holds and what it still needs, and never left at «I cannot tell».

## Languages

What the server writes itself (notices, the questions code asks, card titles and buttons, what a
file the model was not shown means) lives in five copy structs: `NoticeCopy`, `CompositionCopy`,
`AskCopy`, `ConfirmationCopy` and `AttachmentCopy`. Each ships English and Italian, and resolves by
the turn's language. A deployment declares the languages it serves with
`OrchestratorBuilder::locales`, and the orchestrator refuses to start, naming each sentence, while
any of them has no text in one of those languages. `ServerCopy::translated` adds a language, or
replaces a built-in one, field by field; `english()` gives English alone to start from.

A sentence about a record calls it by its workflow's own word for one, in the user's language
(`WorkflowDefinition::noun`, the workflow's key when it declares none), so the Italian reads «come
viaggiatore» where the English reads «as traveler».

The instructions each task gives a model are English, and the model writes in the turn's language.
A deployment replaces them task by task through a prompt source, by the task's name and, first, by
that name for the turn's locale (`understand.verify.it-IT` before `understand.verify`); every call
records which prompt it ran under.

## Three independent guards, not one

1. **Receipts come from events.** They are rendered through the workflow's own `receipts` from the
   events that actually committed. A command that failed contributes none, so there is nothing to
   render a success from (I16).
2. **The writer is handed only what it may state, and reviewed against it.** The outcome document and
   an answer's facts are the whole of what the model may rest on; the review checks the written reply
   against the same material.
3. **The assembled turn is checked structurally.** The claim guard runs before the turn is returned
   and reads the record, not the words: a receipt with no events behind it, or two receipts sharing an
   id, is a refusal.

There is deliberately no guard that matches phrases against the model's prose. A substring match
cannot see a negation ("the configuration is not completed yet" reads as the claim that it was), and
the turns where such a guard is strictest are exactly the ones whose only true sentence is a denial.
What a reply may say is decided by what it is handed and checked by a model that judges meaning;
what happened is decided by the record.

## Nothing streams that could be taken back

The reply is reviewed before it is shown, so it arrives whole, as one block, published once the turn
has committed (§18.5). What streams while the turn runs is progress: the phase, and each
understanding step as it is decided, which says what was read and never what happened.

## A provider failure here cannot repeat an effect

Every call this layer makes runs after the commit. The events are already fixed; a different model
can only phrase them differently, and a model that answers nothing at all leaves the receipts,
notices and cards exactly as they were (I17).
