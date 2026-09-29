//! Read-only tool contract for the bounded context loop (spec §11.2).
//!
//! Read tools query, search, retrieve, calculate and inspect. They never
//! mutate. Every result carries a source label and a [`TrustLevel`] so retrieved
//! content is treated as data, never as instructions (spec §25.3).

use std::time::Duration;

use schemars::{JsonSchema, Schema};
use serde::{Deserialize, Serialize};

use crate::error::SchemaCheckError;
use crate::ids::{ReadRequestId, ReadToolKey};
use crate::schema::validate_against;

/// How sensitive the data a tool returns is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DataSensitivity {
    /// Public.
    Public,
    /// Internal to the account.
    Internal,
    /// Confidential (personal or financial data).
    Confidential,
    /// Restricted (regulated, must not leave the region/provider allowlist).
    Restricted,
}

/// How much a result can be trusted.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// Content from a third party or user upload; may contain injection.
    Untrusted,
    /// Retrieved from an approved but non-authoritative source.
    Retrieved,
    /// Authoritative application state.
    Authoritative,
}

/// Declaration of a read-only tool (spec §11.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadToolDefinition {
    /// Tool key.
    pub key: ReadToolKey,
    /// Description shown to the model.
    pub description: String,
    /// JSON Schema of the arguments.
    pub input_schema: Schema,
    /// JSON Schema of the output.
    pub output_schema: Schema,
    /// Sensitivity of returned data.
    pub sensitivity: DataSensitivity,
    /// Per-call timeout.
    pub timeout: Duration,
    /// Maximum result size; larger outputs are truncated and flagged.
    pub max_result_bytes: usize,
}

impl ReadToolDefinition {
    /// Validates arguments against `input_schema`.
    pub fn validate_input(&self, arguments: &serde_json::Value) -> Result<(), SchemaCheckError> {
        validate_against(&self.input_schema, arguments)
    }

    /// Validates an output against `output_schema`.
    pub fn validate_output(&self, output: &serde_json::Value) -> Result<(), SchemaCheckError> {
        validate_against(&self.output_schema, output)
    }
}

/// A read request the model needs answered before it can interpret the turn.
/// All requests of one response are executed together or not at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadRequest {
    /// Model-assigned id used to match the result.
    pub request_id: ReadRequestId,
    /// The tool.
    pub tool: ReadToolKey,
    /// Arguments matching the tool's input schema, which the registry checks.
    pub arguments: serde_json::Value,
}

/// The normalized result of a read request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadResult {
    /// The request answered.
    pub request_id: ReadRequestId,
    /// The tool.
    pub tool: ReadToolKey,
    /// Output (possibly truncated).
    pub output: serde_json::Value,
    /// Whether the output was truncated to `max_result_bytes`.
    pub truncated: bool,
    /// Source label shown to the model (e.g. `"case_state"`, `"knowledge_base"`).
    pub source_label: String,
    /// Trust level of the content.
    pub trust: TrustLevel,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_request_denies_unknown_fields() {
        let ok = serde_json::json!({
            "request_id": "r1",
            "tool": "case.get",
            "arguments": {"case_id": "inv-1"}
        });
        let parsed = serde_json::from_value::<ReadRequest>(ok).expect("a well-formed request");
        assert_eq!(parsed.arguments["case_id"], "inv-1");
        let bad = serde_json::json!({
            "request_id": "r1",
            "tool": "case.get",
            "arguments": {},
            "write": true
        });
        assert!(serde_json::from_value::<ReadRequest>(bad).is_err());
    }

    #[test]
    fn trust_ordering() {
        assert!(TrustLevel::Untrusted < TrustLevel::Authoritative);
        assert!(DataSensitivity::Public < DataSensitivity::Restricted);
    }
}
