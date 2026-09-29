//! The fixed corpus every adapter is measured against.
//!
//! The suite owns the schema, the requests and the payloads so that two
//! adapters are compared on the same thing. A [`WireFixtures`](super::WireFixtures)
//! implementation supplies only the *vendor framing*: which endpoint, which
//! body shape, which status code. What goes inside is decided here.
//!
//! Two constants carry weight beyond being test data:
//!
//! * [`DUMMY_API_KEY`] is planted so the secret-redaction check has something
//!   specific to hunt for. It must never appear in any rendering of the
//!   adapter, its errors or its responses.
//! * [`SCHEMA_MARKER`] is a property name that exists nowhere except inside
//!   [`plan_schema`]. Finding it in a captured request body is proof the schema
//!   actually travelled, which is how the no-silent-downgrade check tells a
//!   real `NativeJsonSchema` transport from a declaration.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ids::RequestId;
use crate::purpose::ModelPurpose;
use crate::request::{Message, ModelRequest, OutputSpec, ToolSpec};

/// The credential the suite configures every adapter with.
///
/// It is not a valid key anywhere. Its only job is to be findable: if this
/// string appears in a log line, a `Debug` rendering or an error message, the
/// adapter leaks credentials.
pub const DUMMY_API_KEY: &str = "sk-turnframe-conformance-DUMMY-0000000000000000";

/// Name the suite gives the structured-output schema.
pub const SCHEMA_NAME: &str = "turnframe_conformance_plan";

/// A property name that appears only inside [`plan_schema`].
///
/// Its presence in a captured request body proves the schema was sent.
pub const SCHEMA_MARKER: &str = "turnframe_conformance_marker";

/// Name of the read-only tool the suite declares.
pub const TOOL_NAME: &str = "turnframe_conformance_read";

/// The call id the suite expects an adapter to preserve.
pub const EXPECTED_CALL_ID: &str = "turnframe-call-0001";

/// A body that starts like JSON and stops mid-object.
pub const MALFORMED_JSON: &str = "{\"acts\": [{\"operation\": \"set_travel_date\", ";

/// The text a refusing model produces.
pub const REFUSAL_TEXT: &str = "I cannot help with that request.";

/// How long the slow fixture delays its answer.
pub const SLOW_RESPONSE_DELAY: Duration = Duration::from_secs(5);

/// The deadline the timeout and cancellation checks give the adapter.
pub const SHORT_TIMEOUT: Duration = Duration::from_millis(150);

/// Seconds the rate-limit fixture asks the caller to wait.
pub const RETRY_AFTER_SECONDS: u64 = 3;

/// Prompt tokens the [`CachedUsage`](super::Scenario::CachedUsage) fixture
/// reports — the **whole** prompt, cached part included.
pub const USAGE_INPUT_TOKENS: u64 = 42;

/// Prompt tokens that fixture says were served from the provider's cache.
///
/// Deliberately more than half of [`USAGE_INPUT_TOKENS`]: an adapter reporting
/// the net figure would report an input of twelve tokens next to a cache of
/// thirty, and a cache larger than the prompt it is supposed to be part of is
/// exactly what the usage row catches.
pub const USAGE_CACHED_TOKENS: u64 = 30;

/// Generated tokens that fixture reports.
pub const USAGE_OUTPUT_TOKENS: u64 = 7;

/// One act of the conformance plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConformanceAct {
    /// Semantic operation the model proposes.
    pub operation: String,
    /// Opaque target token.
    pub target: String,
}

/// The plan a valid structured response decodes into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConformancePlan {
    /// The proposed acts, in order.
    pub acts: Vec<ConformanceAct>,
}

/// The JSON Schema every structured request in the suite carries.
///
/// It is strict on purpose: `additionalProperties: false` at both levels, so an
/// unknown field is a violation the schema itself catches, and
/// [`SCHEMA_MARKER`] is declared as an optional property so the schema text is
/// recognizable on the wire.
#[must_use]
pub fn plan_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "acts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "operation": {"type": "string"},
                        "target": {"type": "string"},
                        SCHEMA_MARKER: {"type": "boolean"}
                    },
                    "required": ["operation", "target"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["acts"],
        "additionalProperties": false
    })
}

/// A one-act plan that satisfies [`plan_schema`].
#[must_use]
pub fn valid_plan() -> Value {
    json!({"acts": [{"operation": "set_travel_date", "target": "tok_1"}]})
}

/// A two-act plan, for the multiple-acts check.
#[must_use]
pub fn two_act_plan() -> Value {
    json!({"acts": [
        {"operation": "set_travel_date", "target": "tok_1"},
        {"operation": "set_amount", "target": "tok_2"}
    ]})
}

/// A two-act plan whose **second** act carries a field the schema forbids.
///
/// The first act is perfect, which is the point: an adapter or a parser that
/// executes the parseable subset breaks invariant I18 here and nowhere else.
#[must_use]
pub fn plan_with_unknown_field() -> Value {
    json!({"acts": [
        {"operation": "set_travel_date", "target": "tok_1"},
        {"operation": "set_amount", "target": "tok_2", "force": true}
    ]})
}

/// A two-act plan whose second act is missing a required field.
#[must_use]
pub fn plan_with_missing_field() -> Value {
    json!({"acts": [
        {"operation": "set_travel_date", "target": "tok_1"},
        {"operation": "set_amount"}
    ]})
}

/// The read-only tool the suite declares when it needs tool calling.
#[must_use]
pub fn read_tool() -> ToolSpec {
    ToolSpec::new(
        TOOL_NAME,
        "Loads the current state of a case. Read-only.",
        json!({
            "type": "object",
            "properties": {"target": {"type": "string"}},
            "required": ["target"],
            "additionalProperties": false
        }),
    )
}

/// A structured-output request for an understanding task.
#[must_use]
pub fn structured_request() -> ModelRequest {
    ModelRequest::new(ModelPurpose::Extract)
        .with_request_id(RequestId::new())
        .with_system("Propose acts. Never invent identifiers.")
        .with_message(Message::user("sposta il volo al 30"))
        .with_output(OutputSpec::json(SCHEMA_NAME, plan_schema()))
}

/// A request that declares the read tool, for the id-preservation check.
#[must_use]
pub fn tool_request() -> ModelRequest {
    ModelRequest::new(ModelPurpose::Investigate)
        .with_request_id(RequestId::new())
        .with_message(Message::user("che stato ha la pratica?"))
        .with_output(OutputSpec::ToolCalls)
        .with_tools(vec![read_tool()])
}

/// A free-text request, for the streaming and narration checks.
#[must_use]
pub fn narration_request() -> ModelRequest {
    ModelRequest::new(ModelPurpose::Acknowledge)
        .with_request_id(RequestId::new())
        .with_message(Message::user("riassumi in una frase"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structured::{CompiledSchema, StructuredOutputError, parse_structured_value};

    fn schema() -> CompiledSchema {
        match CompiledSchema::compile(&plan_schema()) {
            Ok(schema) => schema,
            Err(error) => panic!("the suite's own schema must compile: {error}"),
        }
    }

    #[test]
    fn the_corpus_matches_its_own_schema() {
        let schema = schema();
        let plan: ConformancePlan = parse_structured_value(&valid_plan(), &schema).unwrap();
        assert_eq!(plan.acts.len(), 1);
        let two: ConformancePlan = parse_structured_value(&two_act_plan(), &schema).unwrap();
        assert_eq!(two.acts.len(), 2);
        assert_eq!(two.acts[1].operation, "set_amount");
    }

    #[test]
    fn the_broken_payloads_break_in_the_intended_way() {
        let schema = schema();
        assert!(matches!(
            parse_structured_value::<ConformancePlan>(&plan_with_unknown_field(), &schema),
            Err(StructuredOutputError::UnknownField { .. })
        ));
        assert!(matches!(
            parse_structured_value::<ConformancePlan>(&plan_with_missing_field(), &schema),
            Err(StructuredOutputError::MissingField { .. })
        ));
        assert!(serde_json::from_str::<Value>(MALFORMED_JSON).is_err());
    }

    #[test]
    fn the_marker_lives_only_in_the_schema() {
        assert!(plan_schema().to_string().contains(SCHEMA_MARKER));
        for payload in [valid_plan(), two_act_plan(), plan_with_missing_field()] {
            assert!(!payload.to_string().contains(SCHEMA_MARKER));
        }
    }

    #[test]
    fn requests_carry_what_each_check_needs() {
        assert!(structured_request().output.is_structured());
        assert_eq!(tool_request().tools.len(), 1);
        assert!(narration_request().tools.is_empty());
        assert!(
            SHORT_TIMEOUT < SLOW_RESPONSE_DELAY,
            "the slow fixture must outlast the deadline"
        );
    }

    #[test]
    fn the_cached_usage_corpus_can_catch_a_net_figure() {
        // The row subtracts nothing itself: it relies on these numbers being
        // chosen so that reporting `input - cached` produces an input smaller
        // than the cache inside it.
        let (input, cached, output) =
            (USAGE_INPUT_TOKENS, USAGE_CACHED_TOKENS, USAGE_OUTPUT_TOKENS);
        assert!(cached < input, "the cache is a subset of the prompt");
        assert!(
            cached > input - cached,
            "a net figure must break the contract, or the row proves nothing"
        );
        assert!(output > 0, "an unreported usage is not a usage");
    }
}
