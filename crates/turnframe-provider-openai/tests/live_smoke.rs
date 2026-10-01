//! Optional live smoke tests (spec §20.8: "wiremock fixtures in CI and
//! optional live smoke tests outside CI").
//!
//! Mocks cannot say whether a real endpoint accepts the request this adapter builds: a
//! field renamed by a vendor, an `api-version` retired, a `strict` schema rejected for a
//! rule the mock never enforced all pass every mocked row. These tests make the smallest
//! calls that answer it: one structured call, each understanding task's request with
//! the schema it sends, and one streamed narration.
//!
//! They run only when [`KEY_VAR`] holds a credential and never when `CI` is set. A skip
//! prints a note and passes, so the suite stays green on a machine with no key. Pointing
//! the base URL at a gateway runs the same calls against it.
//!
//! ```bash
//! TURNFRAME_OPENAI_LIVE_KEY=sk-… cargo test -p turnframe-provider-openai \
//!     --test live_smoke -- --nocapture
//! ```
//!
//! | Variable | Meaning | Default |
//! |---|---|---|
//! | `TURNFRAME_OPENAI_LIVE_KEY` | the credential; absent means skip | — |
//! | `TURNFRAME_OPENAI_LIVE_MODEL` | the model to call | `gpt-6-luna` |
//! | `TURNFRAME_OPENAI_LIVE_BASE_URL` | an OpenAI-compatible endpoint | OpenAI's own |

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout
)]

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use turnframe_provider::conformance::payloads;
use turnframe_provider::prelude::*;
use turnframe_provider::request::ReasoningEffort;
use turnframe_provider_openai::OpenAiProvider;

/// The credential that switches these tests on.
const KEY_VAR: &str = "TURNFRAME_OPENAI_LIVE_KEY";

/// The model to call, when the default is not the one you pay for.
const MODEL_VAR: &str = "TURNFRAME_OPENAI_LIVE_MODEL";

/// An OpenAI-compatible endpoint to call instead of OpenAI itself.
const BASE_URL_VAR: &str = "TURNFRAME_OPENAI_LIVE_BASE_URL";

/// The model called when [`MODEL_VAR`] is unset: the cheapest one that
/// enforces a JSON schema.
const DEFAULT_MODEL: &str = "gpt-6-luna";

/// A live call gets more room than a mock and still cannot hang a suite.
const LIVE_TIMEOUT: Duration = Duration::from_secs(30);

/// What the structured call asks the model for.
///
/// Deliberately trivial, strict-mode clean (`additionalProperties: false`,
/// every property required) and answerable in a handful of tokens: this is a
/// wire check, not an evaluation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Smoke {
    /// The word the model was asked to echo.
    word: String,
    /// How many letters it has, which forces the model to fill two fields.
    letters: u32,
}

/// Loads the repository's `.env` once per test binary, before any test reads a
/// variable or opens a connection. A variable already set in the shell wins.
fn load_dotenv() {
    static LOADED: std::sync::Once = std::sync::Once::new();
    LOADED.call_once(|| {
        let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    });
}

/// Builds the provider, or explains why the test is skipping.
///
/// Returns `None` after printing a note, so a machine with no credential runs
/// a green suite rather than a red one.
fn live_provider() -> Option<OpenAiProvider> {
    load_dotenv();
    if std::env::var_os("CI").is_some() {
        println!("live smoke skipped: CI is set, and these tests never run there");
        return None;
    }
    let Some(key) = std::env::var_os(KEY_VAR).and_then(|value| value.into_string().ok()) else {
        println!("live smoke skipped: set {KEY_VAR} to run it against a real endpoint");
        return None;
    };
    if key.trim().is_empty() {
        println!("live smoke skipped: {KEY_VAR} is empty");
        return None;
    }
    let model = std::env::var(MODEL_VAR).unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    let mut builder = OpenAiProvider::openai()
        .api_key(ApiKey::new(key))
        .model(model)
        .timeout(LIVE_TIMEOUT);
    if let Ok(base_url) = std::env::var(BASE_URL_VAR) {
        builder = builder.base_url(base_url);
    }
    Some(builder.build().expect("the live configuration is valid"))
}

#[tokio::test]
async fn a_real_endpoint_enforces_a_real_schema() {
    let Some(provider) = live_provider() else {
        return;
    };
    let schema = json!({
        "type": "object",
        "properties": {
            "word": {"type": "string"},
            "letters": {"type": "integer"}
        },
        "required": ["word", "letters"],
        "additionalProperties": false
    });
    let request = ModelRequest::new(ModelPurpose::Extract)
        .with_system("Answer with the requested document and nothing else.")
        .with_message(Message::user("The word is 'turnframe'. Count its letters."))
        .with_output(OutputSpec::json("turnframe_live_smoke", schema.clone()))
        .with_max_output_tokens(64)
        .with_reasoning_effort(ReasoningEffort::Minimal)
        .with_timeout(LIVE_TIMEOUT);

    let response = provider
        .generate(request)
        .await
        .expect("a live structured call");
    let compiled = CompiledSchema::compile(&schema).expect("the smoke schema compiles");
    let smoke: Smoke = parse_structured(&response, &compiled).expect("the answer is schema-valid");

    // The point is that the *shape* survived the round trip, not that the
    // model can count: a live assertion about content would be flaky by
    // construction.
    assert!(!smoke.word.is_empty(), "{smoke:?}");
    assert!(
        smoke.letters <= 64,
        "the second field must be a small integer, not prose: {smoke:?}"
    );
    assert!(response.finish.is_complete(), "{:?}", response.finish);
    println!(
        "live smoke: {} answered {} in {:?}",
        provider.model_key(),
        response.finish,
        response.latency
    );
    println!("live smoke: parsed {smoke:?}");
}

#[tokio::test]
async fn a_real_stream_reassembles_into_a_real_answer() {
    let Some(provider) = live_provider() else {
        return;
    };
    let request = ModelRequest::new(ModelPurpose::Acknowledge)
        .with_system("Reply in one short sentence.")
        .with_message(Message::user("Say hello to Turnframe."))
        .with_max_output_tokens(64)
        .with_reasoning_effort(ReasoningEffort::Minimal)
        .with_timeout(LIVE_TIMEOUT);

    let stream = provider
        .stream(request.clone())
        .await
        .expect("a live streamed call");
    let rebuilt = reconstruct(
        stream,
        StreamAccumulator::new(
            request.request_id,
            provider.provider_key(),
            provider.model_key(),
        ),
    )
    .await
    .expect("the live stream reassembles");

    assert!(!rebuilt.text().trim().is_empty(), "{rebuilt:?}");
    assert!(rebuilt.finish.is_complete(), "{:?}", rebuilt.finish);
    assert!(rebuilt.warnings.contains(&ResponseWarning::Reconstructed));
    println!("live smoke: streamed {:?}", rebuilt.text());
}

#[test]
fn the_corpus_the_mocked_suite_uses_is_reachable_from_here_too() {
    // A cheap guard against the live file drifting away from the suite it
    // complements: both are built on the same crate surface, and a rename that
    // broke one would otherwise only be noticed with a key in hand.
    assert!(!payloads::DUMMY_API_KEY.is_empty());
    assert_ne!(KEY_VAR, MODEL_VAR);
}

/// Every understanding task's answer schema, in the request the task engine sends, put
/// in front of a real endpoint.
///
/// The mocked suite proves this adapter sends a schema; only a real endpoint proves it
/// accepts these ones. Understanding is the stage that needs genuine structured output,
/// so a refusal here means turns cannot be understood against this provider, whatever
/// the offline suite says.
#[tokio::test]
async fn every_understanding_schema_is_accepted_by_a_real_endpoint() {
    let Some(provider) = live_provider() else {
        return;
    };
    let mut refused = Vec::new();
    for request in understanding::requests() {
        let task = request.purpose.as_str();
        match provider.generate(request).await {
            Ok(response) => println!(
                "live smoke: the {task} schema was accepted, finishing {}",
                response.finish.as_str()
            ),
            Err(error) => refused.push(format!("{task}: {error}")),
        }
    }
    assert!(
        refused.is_empty(),
        "the endpoint refused what these understanding tasks send, so they cannot run \
         against it:\n{}",
        refused.join("\n")
    );
}

#[test]
fn every_understanding_request_is_built_without_a_network() {
    // The live test builds its requests only with a key in hand; building them on every
    // run means a smoke domain that stopped validating fails here, for free.
    let requests = understanding::requests();
    let kinds: std::collections::BTreeSet<&str> = requests
        .iter()
        .map(|request| request.purpose.as_str())
        .collect();
    let expected = [
        "segment",
        "coverage",
        "route",
        "locate",
        "extract",
        "verify",
        "question_frame",
    ];
    assert_eq!(kinds, expected.into_iter().collect());
    for request in &requests {
        let schema = request
            .output
            .schema()
            .expect("an understanding task answers in JSON");
        CompiledSchema::compile(schema).expect("the task's schema compiles");
    }
}

/// Every request the understanding tasks send for one trip turn, built as the task
/// engine builds it: the task's instructions, context and answer schema, under the
/// task's shipped profile.
mod understanding {
    use std::collections::BTreeMap;

    use chrono::NaiveDate;
    use schemars::JsonSchema;
    use serde::Deserialize;
    use serde_json::{Value, json};
    use turnframe_core::flow::StateField;
    use turnframe_core::operation::{DateDirection, Money, OperationSpec};
    use turnframe_core::plan::TargetPolicy;
    use turnframe_core::understanding::{
        ArgumentValue, Excerpt, MessageRef, UnderstoodArgument, UnitKind,
    };
    use turnframe_provider::prelude::*;
    use turnframe_tasks::{ModelTask, TaskProfile};
    use turnframe_understand::tasks::coverage::{Coverage, Found};
    use turnframe_understand::tasks::extract::{Extract, ExtractInput, RecordContext};
    use turnframe_understand::tasks::locate::{Candidate, Locate, LocateInput};
    use turnframe_understand::tasks::question_frame::{QuestionFrame, QuestionInput};
    use turnframe_understand::tasks::route::{Route, RouteInput};
    use turnframe_understand::tasks::segment::Segment;
    use turnframe_understand::tasks::verify::{Verify, VerifyInput};
    use turnframe_understand::{
        OpenCard, RecordBrief, Span, Speaker, UnderstandingInput, WorkflowBrief,
    };

    const SET_NAME: &str = "trip.set_name";
    const SET_DATE: &str = "trip.set_travel_date";
    const ADD_LINE: &str = "trip.add_extra";
    const CREATE: &str = "trip.open";
    const SEND: &str = "trip.request_rebooking";

    const SUBJECT: &str = "set the name to Lisbon";
    const LINE: &str = "add an extra of 120 euros for the workshop";
    const DUE: &str = "make it due next friday";
    const QUESTION: &str = "tell me the total";

    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code, reason = "only its schema is sent")]
    struct SetSubject {
        /// What the trip is for.
        value: String,
    }

    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code, reason = "only its schema is sent")]
    struct SetDue {
        /// When payment is due.
        due: NaiveDate,
    }

    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code, reason = "only its schema is sent")]
    struct AddLine {
        /// What the extra bills for.
        description: String,
        /// Its amount.
        amount: Money,
    }

    fn operations() -> Vec<OperationSpec> {
        let existing = |key: &str, summary: &str| {
            OperationSpec::new(key)
                .summary(summary)
                .target(TargetPolicy::RequiresExistingCase)
                .mutating()
        };
        vec![
            existing(SET_NAME, "Set what the trip is for.")
                .arguments::<SetSubject>()
                .argument("value", |a| a.label("name").required()),
            existing(SET_DATE, "Set when the trip is due.")
                .arguments::<SetDue>()
                .argument("due", |a| {
                    a.label("travel date").date_direction(DateDirection::Future)
                }),
            existing(ADD_LINE, "Add an extra to the trip.")
                .arguments::<AddLine>()
                .argument("amount", |a| a.money()),
            OperationSpec::new(CREATE)
                .summary("Start a new trip draft.")
                .target(TargetPolicy::NewCaseOnly)
                .mutating(),
            existing(SEND, "Send the trip."),
        ]
    }

    fn turn() -> UnderstandingInput {
        let mut trips = WorkflowBrief::new("trip")
            .summary("Create, edit and send trips.")
            .on_new_case(CREATE)
            .subject("total");
        for spec in operations() {
            spec.validate().expect("the smoke operations are valid");
            trips = trips.operation(spec);
        }
        for (number, traveler) in [(1, "Aurora"), (2, "Haddad")] {
            let record = RecordBrief::new(
                format!("tok-trip-{number}"),
                format!("Trip {number}"),
                "collecting",
            )
            .field(StateField::new("traveler", json!(traveler)).identifying())
            .field(StateField::new("name", Value::Null))
            .offering([SET_NAME, SET_DATE, ADD_LINE, SEND]);
            trips = trips.record(record);
        }
        let message = format!("{SUBJECT}, {LINE}, {DUE} and {QUESTION}");
        let today = NaiveDate::from_ymd_opt(2026, 9, 26).expect("a valid date");
        UnderstandingInput::new(&message, "en-GB", today)
            .with_workflow(trips)
            .with_earlier(Speaker::User, "open the Aurora trip")
            .with_earlier(
                Speaker::Assistant,
                "Trip 1 for Aurora is open. What should it say?",
            )
            .with_card(
                OpenCard::new("trip", "Send Trip 1 to Aurora?")
                    .about("tok-trip-1")
                    .option("send", "Send")
                    .option("keep", "Not yet"),
            )
    }

    /// The words of `quote` in the turn's message.
    fn span(turn: &UnderstandingInput, quote: &str) -> Span {
        let start = turn
            .message
            .text()
            .find(quote)
            .expect("the quote is in the message");
        let end = start + quote.len();
        let words = turn.message.words();
        let first = words.iter().position(|word| word.end > start);
        let last = words.iter().rposition(|word| word.start < end);
        Span::new(first.expect("a first word"), last.expect("a last word"))
    }

    fn request<T: ModelTask>(task: &T, input: &T::Input) -> ModelRequest {
        let kind = task.kind();
        let profile = TaskProfile::default_for(kind);
        let mut request = ModelRequest::new(kind)
            .with_system(task.instructions())
            .with_output(OutputSpec::json(kind.as_str(), task.schema(input)))
            .with_messages(task.render(input))
            .with_timeout(super::LIVE_TIMEOUT)
            .with_cache_hint(CacheHint::System);
        if let Some(temperature) = profile.temperature {
            request = request.with_temperature(temperature);
        }
        if let Some(tokens) = profile.max_output_tokens {
            request = request.with_max_output_tokens(tokens);
        }
        if let Some(effort) = profile.reasoning_effort {
            request = request.with_reasoning_effort(effort);
        }
        request
    }

    /// One request per task kind, and one extraction per operation with arguments.
    pub(super) fn requests() -> Vec<ModelRequest> {
        let turn = turn();
        let workflow = &turn.workflows[0];
        let spec = |key: &str| workflow.spec(&key.into()).expect("an offered operation");
        let subject = span(&turn, SUBJECT);
        let mut requests = vec![
            request(&Segment::new(&turn), &()),
            request(
                &Coverage::new(&turn),
                &Found {
                    units: vec![
                        (UnitKind::Request, subject),
                        (UnitKind::Question, span(&turn, QUESTION)),
                    ],
                },
            ),
            request(
                &Route::new(&turn),
                &RouteInput {
                    label: "Request",
                    words: subject,
                    workflows: vec![workflow],
                    note: None,
                    others: Vec::new(),
                },
            ),
            request(
                &Locate::new(&turn),
                &LocateInput {
                    label: "Request",
                    words: subject,
                    spec: spec(SET_NAME),
                    workflow: &workflow.key,
                    candidates: workflow.records.iter().map(Candidate::Record).collect(),
                    allow_new: false,
                    allow_not_listed: true,
                    note: None,
                },
            ),
        ];
        for (quote, key) in [(SUBJECT, SET_NAME), (LINE, ADD_LINE), (DUE, SET_DATE)] {
            let spec = spec(key);
            requests.push(request(
                &Extract::new(&turn),
                &ExtractInput {
                    label: "Request",
                    words: span(&turn, quote),
                    spec,
                    workflow,
                    record: RecordContext::Existing(&workflow.records[0]),
                    arguments: spec.arguments.iter().collect(),
                    record_choices: BTreeMap::new(),
                    continues: None,
                    others: Vec::new(),
                    kin: Vec::new(),
                    also: Vec::new(),
                    transcript: 4,
                    note: None,
                    occurrence: None,
                    corrected: std::collections::BTreeMap::new(),
                },
            ));
        }
        let lisbon = turn
            .message
            .range(span(&turn, "Lisbon"))
            .expect("the value is in the message");
        let arguments = BTreeMap::from([(
            "value".to_owned(),
            UnderstoodArgument {
                value: ArgumentValue::Json(Value::from("Lisbon")),
                excerpt: Some(Excerpt {
                    message: MessageRef::Current,
                    words: lisbon,
                }),
            },
        )]);
        requests.push(request(
            &Verify::new(&turn),
            &VerifyInput {
                label: "Request",
                words: subject,
                meaning: format!("{SET_NAME}: Set what the trip is for."),
                record: "Trip 1".to_owned(),
                arguments: &arguments,
                labels: BTreeMap::from([("value".to_owned(), "name".to_owned())]),
                record_labels: BTreeMap::new(),
                meanings: BTreeMap::new(),
                occurrence: None,
                note: None,
                continues: None,
            },
        ));
        requests.push(request(
            &QuestionFrame::new(&turn),
            &QuestionInput {
                words: span(&turn, QUESTION),
                records: workflow.records.iter().collect(),
                subjects: vec!["total"],
            },
        ));
        requests
    }
}
