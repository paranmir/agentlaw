//! One tool with explicit action namespaces.
use crate::{tool_input_schema, DomainError, Request, Result};
use serde_json::{json, Map, Value};

/// `Result` using the existing contracts error.
pub type ContractResult<T> = Result<T>;

/// A request that passed the public or internal validation path.
/// The field is private: this module's constructors are the only way to obtain one.
#[derive(Debug, Clone)]
pub struct ValidatedRequest(Request);

impl ValidatedRequest {
    pub fn parse_public(raw: &str) -> ContractResult<Self> {
        crate::parse_request(raw).map(Self)
    }

    pub fn request(&self) -> &Request {
        &self.0
    }

    pub fn into_request(self) -> Request {
        self.0
    }
}

fn invalid(message: impl Into<String>) -> DomainError {
    DomainError::new("invalid_input", message)
}

/// Project a typed request into the flat form accepted by the runtime validator.
pub fn validation_value(request: &Request) -> ContractResult<Value> {
    let mut value = serde_json::to_value(request).map_err(|_| {
        DomainError::new(
            "invalid_input",
            "The request could not be represented for validation.",
        )
    })?;
    remove_null_entries(&mut value);
    Ok(value)
}

/// Encode a request as the runtime's compact JSON call body.
pub fn encode_wire(request: &Request) -> ContractResult<Vec<u8>> {
    let value = validation_value(request)?;
    serde_json::to_vec(&value).map_err(|_| {
        DomainError::new(
            "invalid_input",
            "The request could not be encoded for the runtime.",
        )
    })
}

/// Decode and validate the runtime's flat JSON call body.
pub fn decode_wire(body: &[u8]) -> ContractResult<ValidatedRequest> {
    let text = std::str::from_utf8(body).map_err(|_| {
        DomainError::new(
            "invalid_input",
            "Invalid JSON, duplicate object key, or invalid Unicode.",
        )
    })?;
    let value = crate::validation::decode_unique(text)?;
    crate::validation::validate_input(&value)?;
    serde_json::from_value::<Request>(value)
        .map(ValidatedRequest)
        .map_err(|_| {
            DomainError::new(
                "invalid_input",
                "Input exceeds supported numeric bounds or has invalid typed fields.",
            )
        })
}

fn remove_null_entries(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.retain(|_, value| !value.is_null());
            for value in object.values_mut() {
                remove_null_entries(value);
            }
        }
        Value::Array(array) => {
            for value in array {
                remove_null_entries(value);
            }
        }
        _ => {}
    }
}

pub(crate) fn normalize(value: Value) -> Result<Value> {
    let schema = tool_input_schema();
    let root = value
        .as_object()
        .ok_or_else(|| invalid("Expected an object with action and its matching input object."))?;
    let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
    let action = root
        .get("action")
        .and_then(Value::as_str)
        .filter(|a| actions.iter().any(|known| known.as_str() == Some(a)))
        .ok_or_else(|| {
            invalid(
                "action must be recall, remember_this, history, connect_project_memory or sync.",
            )
        })?;
    if root.len() != 2 || !root.contains_key(action) {
        return Err(invalid(format!("For action={action}, supply only action and the {action} object. Do not mix action objects or top-level input fields.")));
    }
    let input = root[action].clone();
    let shape = check_shape(&schema["properties"][action], &input, action, &schema);
    let mut flat: Map<String, Value> = match input.as_object() {
        Some(fields) => fields.clone(),
        None => return Err(shape.expect_err("action input must be an object")),
    };
    flat.insert("action".into(), action.into());
    let flat = Value::Object(flat);
    shape.map_err(|error| crate::validation::public_input_field_error(&flat).unwrap_or(error))?;
    Ok(flat)
}

fn check_shape(schema: &Value, value: &Value, path: &str, public: &Value) -> Result<()> {
    let ty = schema["type"].as_str().unwrap();
    let valid = match ty {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        _ => false,
    };
    if !valid {
        return Err(invalid(format!("{path} must be {ty}.")));
    }
    if let Some(minimum) = schema.get("minimum").and_then(Value::as_u64) {
        if value.as_u64().is_none_or(|provided| provided < minimum) {
            return Err(invalid(format!(
                "{path} must be an integer >= {minimum}; received {value}."
            ))
            .with_details(json!([{
                "path":format!("/{}",path.replace('.',"/")),
                "provided_value":value,
                "constraint":{"type":"integer","minimum":minimum},
                "corrected_example":minimum
            }])));
        }
    }
    if let Some(choices) = schema.get("enum").and_then(Value::as_array) {
        if !choices.contains(value) {
            return Err(invalid(format!(
                "{path} must use one of the declared enum values: {}.",
                Value::Array(choices.clone())
            )));
        }
    }
    if let Some(object) = value.as_object() {
        let fields = schema["properties"].as_object().unwrap();
        for (name, child) in object {
            let Some(field) = fields.get(name) else {
                // Only repeat known schema field names, never untrusted values/keys.
                let owners: Vec<_> = public["properties"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter(|(a, s)| a.as_str() != "action" && s["properties"].get(name).is_some())
                    .map(|(a, _)| a.as_str())
                    .collect();
                if path == "recall" && name == "max_matches" {
                    return Err(invalid("history.max_matches is not a recall field. Use recall.memory_candidate_limit for memory candidates or recall.procedure_candidate_limit for procedure candidates; omit either to use its default."));
                }
                if !owners.is_empty() {
                    return Err(invalid(format!("{name} does not belong in {path}; it belongs to {}. Move it only if you intend that action, otherwise omit it.", owners.join(", "))));
                }
                return Err(invalid(format!(
                    "{path} contains an unknown field. Allowed fields: {}.",
                    fields.keys().cloned().collect::<Vec<_>>().join(", ")
                )));
            };
            check_shape(field, child, &format!("{path}.{name}"), public)?;
        }
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for name in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    return Err(invalid(format!("{path}.{name} is required.")));
                }
            }
        }
    }
    if let Some(items) = value.as_array() {
        for item in items {
            check_shape(&schema["items"], item, &format!("{path}[]"), public)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_request;
    use serde_json::json;

    fn examples() -> Value {
        serde_json::from_str(include_str!(
            "../../../docs/design/contracts/agentlaw-input.examples.json"
        ))
        .unwrap()
    }

    #[test]
    fn decode_wire_round_trips_public_examples() {
        let fixtures = examples();
        for example in fixtures["examples"].as_array().unwrap() {
            if example["valid"] != true {
                continue;
            }
            let id = example["id"].as_str().unwrap();
            let input = &example["input"];
            let mut fields = input.as_object().unwrap().clone();
            let action = fields.remove("action").unwrap();
            let mut public = Map::new();
            public.insert("action".into(), action.clone());
            public.insert(action.as_str().unwrap().into(), Value::Object(fields));
            let request = ValidatedRequest::parse_public(&Value::Object(public).to_string())
                .unwrap_or_else(|error| panic!("{id}: public parsing failed: {error:?}"));
            let bytes = encode_wire(request.request())
                .unwrap_or_else(|error| panic!("{id}: encoding failed: {error:?}"));
            let decoded = decode_wire(&bytes)
                .unwrap_or_else(|error| panic!("{id}: decoding failed: {error:?}"));
            let actual = validation_value(decoded.request())
                .unwrap_or_else(|error| panic!("{id}: projection failed: {error:?}"));
            assert_eq!(&actual, input, "{id}");
        }
    }

    #[test]
    fn decode_wire_preserves_typed_serialization() {
        let fixtures = examples();
        for example in fixtures["examples"].as_array().unwrap() {
            if example["valid"] != true {
                continue;
            }
            let id = example["id"].as_str().unwrap();
            let before: Request = serde_json::from_value(example["input"].clone())
                .unwrap_or_else(|error| panic!("{id}: deserialization failed: {error}"));
            let bytes = encode_wire(&before)
                .unwrap_or_else(|error| panic!("{id}: encoding failed: {error:?}"));
            let after = decode_wire(&bytes)
                .unwrap_or_else(|error| panic!("{id}: decoding failed: {error:?}"))
                .into_request();
            assert_eq!(
                serde_json::to_string(&before).unwrap(),
                serde_json::to_string(&after).unwrap(),
                "{id}"
            );
        }
    }

    #[test]
    fn decode_wire_rejects_invalid_examples() {
        let fixtures = examples();
        for example in fixtures["examples"].as_array().unwrap() {
            if example["valid"] != false {
                continue;
            }
            let id = example["id"].as_str().unwrap();
            let bytes = serde_json::to_vec(&example["input"]).unwrap();
            let error = decode_wire(&bytes).expect_err(id);
            assert_eq!(error.code, "invalid_input", "{id}");
        }
    }

    #[test]
    fn decode_wire_rejects_invalid_boundary_inputs() {
        let cases: &[(&str, &[u8])] = &[
            ("invalid UTF-8", &[0xff, 0xfe]),
            (
                "duplicate key",
                br#"{"action":"recall","recall_for":"a","recall_for":"b"}"#,
            ),
            (
                "public nested form",
                br#"{"action":"recall","recall":{"recall_for":"x"}}"#,
            ),
            (
                "numeric bound",
                br#"{"action":"recall","recall_for":"x","memory_candidate_limit":4294967296}"#,
            ),
        ];
        for (name, body) in cases {
            let error = decode_wire(body).expect_err(name);
            assert_eq!(error.code, "invalid_input", "{name}");
        }
    }

    #[test]
    fn encode_wire_preserves_text_and_compact_projection() {
        let text = "기억 \"a\\b\"\n끝";
        let input = json!({
            "action": "recall",
            "recall": {"recall_for": text, "restore_context": false}
        });
        let request = parse_request(&input.to_string()).unwrap();
        let bytes = encode_wire(&request).unwrap();
        assert!(std::str::from_utf8(&bytes).is_ok());
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value, validation_value(&request).unwrap());
        assert_eq!(value["recall_for"], text);
        assert_eq!(value["restore_context"], false);
        assert!(!value.as_object().unwrap().values().any(Value::is_null));
        assert_eq!(bytes, serde_json::to_vec(&value).unwrap());
    }

    #[test]
    fn validation_projection_preserves_valid_examples() {
        let fixtures: Value = serde_json::from_str(include_str!(
            "../../../docs/design/contracts/agentlaw-input.examples.json"
        ))
        .unwrap();
        for example in fixtures["examples"].as_array().unwrap() {
            if example["valid"] != true {
                continue;
            }
            let id = example["id"].as_str().unwrap();
            let input = &example["input"];
            let request: Request = serde_json::from_value(input.clone())
                .unwrap_or_else(|error| panic!("{id}: deserialization failed: {error}"));
            let projected = validation_value(&request)
                .unwrap_or_else(|error| panic!("{id}: projection failed: {error:?}"));
            assert_eq!(&projected, input, "{id}");
            assert!(
                crate::validation::validate_input(&projected).is_ok(),
                "{id}"
            );
        }
    }

    #[test]
    fn validation_projection_keeps_false_and_empty_array() {
        let request: Request = serde_json::from_value(json!({
            "action": "recall", "restore_context": false, "memory_ids": []
        }))
        .unwrap();
        let projected = validation_value(&request).unwrap();
        assert_eq!(
            projected,
            json!({
                "action": "recall", "restore_context": false, "memory_ids": []
            })
        );
    }

    #[test]
    fn validation_projection_removes_nested_optional_nulls() {
        let request: Request = serde_json::from_value(json!({
            "action": "remember_this",
            "procedure": {
                "operation": "create",
                "evidence_memory_ids": ["11111111-1111-4111-8111-111111111111"]
            }
        }))
        .unwrap();
        let projected = validation_value(&request).unwrap();
        fn contains_null(value: &Value) -> bool {
            match value {
                Value::Null => true,
                Value::Array(items) => items.iter().any(contains_null),
                Value::Object(fields) => fields.values().any(contains_null),
                _ => false,
            }
        }
        assert!(!contains_null(&projected), "{projected}");
        assert_eq!(projected["procedure"]["operation"], "create");
    }

    #[test]
    fn validated_request_preserves_public_parsing() {
        let raw = r#"{"action":"recall","recall":{"recall_for":"x","restore_context":false}}"#;
        let validated = ValidatedRequest::parse_public(raw).unwrap();
        assert!(matches!(
            validated.request(),
            Request::Recall(input) if input.restore_context == Some(false)
        ));
        assert!(matches!(
            validated.into_request(),
            Request::Recall(input) if input.restore_context == Some(false)
        ));

        let invalid = r#"{"action":"recall","recall_for":"x"}"#;
        let expected = parse_request(invalid).unwrap_err();
        let actual = ValidatedRequest::parse_public(invalid).unwrap_err();
        assert_eq!(actual.code, expected.code);
        assert_eq!(actual.message, expected.message);
    }

    #[test]
    fn action_boundaries_types_and_no_secret_echo() {
        assert!(
            parse_request(r#"{"action":"recall","recall_for":"A former flat request"}"#).is_err()
        );
        for input in [json!({"action":"recall","recall":{"recall_for":"x","max_matches":10}})] {
            let error = parse_request(&input.to_string()).unwrap_err();
            assert!(error.message.contains("recall.memory_candidate_limit"));
        }
        for input in [
            json!({"action":"recall","history":{}}),
            json!({"action":"recall","recall":{},"history":{}}),
            json!({"action":"recall","recall":{"recall_for":"x"},"recall_for":"y"}),
            json!({"action":"recall","recall":null}),
            json!({"action":"recall","recall":{"recall_for":"x","memory_candidate_limit":1.5}}),
            json!({"action":"remember_this","remember_this":{"memories":[{"operation":"create"}]}}),
        ] {
            assert!(parse_request(&input.to_string()).is_err());
        }
        let error = normalize(
            json!({"action":"history","history":{"secret-test-key":"secret-test-value"}}),
        )
        .unwrap_err();
        assert!(!error.message.contains("secret-test"));
        assert!(parse_request(
            r#"{"action":"recall","recall":{"recall_for":"a","recall_for":"b"}}"#
        )
        .is_err());
    }
    #[test]
    fn candidate_limits_report_the_exact_field_and_bound() {
        let error = parse_request(
            &json!({"action":"recall","recall":{"recall_for":"x","procedure_candidate_limit":0}})
                .to_string(),
        )
        .unwrap_err();
        assert_eq!(error.code, "invalid_input");
        assert!(error.message.contains("recall.procedure_candidate_limit"));
        assert!(error.message.contains("integer >= 1; received 0"));
        assert_eq!(
            error.details.as_ref().unwrap()[0]["path"],
            "/recall/procedure_candidate_limit"
        );
        assert_eq!(error.details.as_ref().unwrap()[0]["provided_value"], 0);
        assert_eq!(
            tool_input_schema()["properties"]["recall"]["properties"]["procedure_candidate_limit"]
                ["minimum"],
            1
        );
    }
}
