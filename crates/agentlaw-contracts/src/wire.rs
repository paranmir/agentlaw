//! One tool with explicit action namespaces.
use crate::{tool_input_schema, DomainError, Result};
use serde_json::{json, Map, Value};

fn invalid(message: impl Into<String>) -> DomainError {
    DomainError::new("invalid_input", message)
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
            invalid("action must be recall, remember_this, history or connect_project_memory.")
        })?;
    if root.len() != 2 || !root.contains_key(action) {
        return Err(invalid(format!("For action={action}, supply only action and the {action} object. Do not mix action objects or top-level input fields.")));
    }
    let input = root[action].clone();
    check_shape(&schema["properties"][action], &input, action, &schema)?;
    let mut flat: Map<String, Value> = input.as_object().unwrap().clone();
    flat.insert("action".into(), action.into());
    Ok(Value::Object(flat))
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
