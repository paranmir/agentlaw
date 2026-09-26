//! Validator for the exact JSON Schema keywords used by the bundled input contract.
use crate::{DomainError, Result, INPUT_SCHEMA};
use serde::{
    de::{MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::{Map, Value};
use std::fmt;

struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON without duplicate keys")
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Unique(Value::Number(n)))
                    .ok_or_else(|| E::custom("invalid number"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_string<E: serde::de::Error>(
                self,
                v: String,
            ) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_none<E: serde::de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut v = Vec::new();
                while let Some(x) = a.next_element::<Unique>()? {
                    v.push(x.0)
                }
                Ok(Unique(Value::Array(v)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut m = Map::new();
                while let Some((k, v)) = a.next_entry::<String, Unique>()? {
                    if m.insert(k, v.0).is_some() {
                        return Err(serde::de::Error::custom("duplicate object key"));
                    }
                }
                Ok(Unique(Value::Object(m)))
            }
        }
        d.deserialize_any(V)
    }
}
pub fn decode_unique(json: &str) -> Result<Value> {
    serde_json::from_str::<Unique>(json)
        .map(|x| x.0)
        .map_err(|_| {
            DomainError::new(
                "invalid_input",
                "Invalid JSON, duplicate object key, or invalid Unicode.",
            )
        })
}
pub fn validate_input(value: &Value) -> Result<()> {
    let schema: Value = serde_json::from_str(INPUT_SCHEMA).expect("bundled schema");
    ensure_supported(&schema)?;
    if accepts(&schema, value, &schema) {
        Ok(())
    } else {
        Err(DomainError::new("invalid_input","Input does not match an Agentlaw action schema. Check required, forbidden, and dependent fields."))
    }
}
fn ensure_supported(s: &Value) -> Result<()> {
    let supported = [
        "$schema",
        "title",
        "description",
        "$comment",
        "type",
        "oneOf",
        "$defs",
        "$ref",
        "properties",
        "required",
        "additionalProperties",
        "allOf",
        "if",
        "then",
        "else",
        "not",
        "anyOf",
        "const",
        "enum",
        "minLength",
        "items",
        "minItems",
        "uniqueItems",
        "dependentRequired",
        "minimum",
        "minProperties",
    ];
    let obj = s
        .as_object()
        .ok_or_else(|| DomainError::new("schema_unsupported", "Expected object schema."))?;
    for (k, v) in obj {
        if !supported.contains(&k.as_str()) {
            return Err(DomainError::new(
                "schema_unsupported",
                format!("Unsupported schema keyword: {k}"),
            ));
        }
        match k.as_str() {
            "$defs" | "properties" => {
                for child in v
                    .as_object()
                    .ok_or_else(|| DomainError::new("schema_unsupported", "Invalid schema map."))?
                    .values()
                {
                    ensure_supported(child)?
                }
            }
            "oneOf" | "allOf" | "anyOf" => {
                for child in v.as_array().ok_or_else(|| {
                    DomainError::new("schema_unsupported", "Invalid schema alternatives.")
                })? {
                    ensure_supported(child)?
                }
            }
            "if" | "then" | "else" | "not" | "items" => ensure_supported(v)?,
            _ => {}
        }
    }
    Ok(())
}
fn accepts(s: &Value, v: &Value, root: &Value) -> bool {
    if let Some(r) = s.get("$ref").and_then(Value::as_str) {
        return root
            .pointer(r.trim_start_matches('#'))
            .is_some_and(|s| accepts(s, v, root));
    }
    if let Some(t) = s.get("type").and_then(Value::as_str) {
        if !match t {
            "object" => v.is_object(),
            "array" => v.is_array(),
            "string" => v.is_string(),
            "boolean" => v.is_boolean(),
            "integer" => v.as_i64().is_some() || v.as_u64().is_some(),
            _ => false,
        } {
            return false;
        }
    }
    if s.get("const").is_some_and(|x| x != v) {
        return false;
    }
    if s.get("enum")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.contains(v))
    {
        return false;
    }
    if let Some(a) = s.get("allOf").and_then(Value::as_array) {
        if !a.iter().all(|x| accepts(x, v, root)) {
            return false;
        }
    }
    if let Some(a) = s.get("anyOf").and_then(Value::as_array) {
        if !a.iter().any(|x| accepts(x, v, root)) {
            return false;
        }
    }
    if let Some(a) = s.get("oneOf").and_then(Value::as_array) {
        if a.iter().filter(|x| accepts(x, v, root)).count() != 1 {
            return false;
        }
    }
    if s.get("not").is_some_and(|x| accepts(x, v, root)) {
        return false;
    }
    if let Some(i) = s.get("if") {
        let branch = if accepts(i, v, root) {
            s.get("then")
        } else {
            s.get("else")
        };
        if branch.is_some_and(|x| !accepts(x, v, root)) {
            return false;
        }
    }
    if let Some(o) = v.as_object() {
        if s.get("minProperties")
            .and_then(Value::as_u64)
            .is_some_and(|n| (o.len() as u64) < n)
        {
            return false;
        }
        if let Some(a) = s.get("required").and_then(Value::as_array) {
            if a.iter().any(|k| !o.contains_key(k.as_str().unwrap())) {
                return false;
            }
        }
        let p = s.get("properties").and_then(Value::as_object);
        for (k, x) in o {
            if let Some(schema) = p.and_then(|p| p.get(k)) {
                if !accepts(schema, x, root) {
                    return false;
                }
            } else if s.get("additionalProperties") == Some(&Value::Bool(false)) {
                return false;
            }
        }
        if let Some(deps) = s.get("dependentRequired").and_then(Value::as_object) {
            for (k, required) in deps {
                if o.contains_key(k)
                    && required
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|r| !o.contains_key(r.as_str().unwrap()))
                {
                    return false;
                }
            }
        }
    }
    if let Some(a) = v.as_array() {
        if s.get("minItems")
            .and_then(Value::as_u64)
            .is_some_and(|n| (a.len() as u64) < n)
        {
            return false;
        }
        if s.get("uniqueItems") == Some(&Value::Bool(true))
            && (0..a.len()).any(|i| a[i + 1..].contains(&a[i]))
        {
            return false;
        }
        if let Some(item) = s.get("items") {
            if !a.iter().all(|x| accepts(item, x, root)) {
                return false;
            }
        }
    }
    if let Some(t) = v.as_str() {
        if s.get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|n| (t.chars().count() as u64) < n)
        {
            return false;
        }
    }
    if let Some(n) = s.get("minimum").and_then(Value::as_f64) {
        if v.as_f64().is_some_and(|x| x < n) {
            return false;
        }
    }
    true
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_design_examples() {
        let fixtures: Value = serde_json::from_str(include_str!(
            "../../../docs/design/contracts/agentlaw-input.examples.json"
        ))
        .unwrap();
        for e in fixtures["examples"].as_array().unwrap() {
            assert_eq!(
                crate::parse_request(&e["input"].to_string()).is_ok(),
                e["valid"].as_bool().unwrap(),
                "{}",
                e["id"]
            );
        }
    }
    #[test]
    fn published_schema_is_self_contained_and_preserves_validation() {
        let published = crate::tool_input_schema();
        ensure_supported(&published).unwrap();
        fn assert_inline(value: &Value) {
            match value {
                Value::Object(object) => {
                    assert!(!object.contains_key("$ref"));
                    assert!(!object.contains_key("$defs"));
                    for child in object.values() {
                        assert_inline(child);
                    }
                }
                Value::Array(values) => {
                    for child in values {
                        assert_inline(child);
                    }
                }
                _ => {}
            }
        }
        assert_inline(&published);
        assert_eq!(published["title"], "Agentlaw action input");
        let fixtures: Value = serde_json::from_str(include_str!(
            "../../../docs/design/contracts/agentlaw-input.examples.json"
        ))
        .unwrap();
        for example in fixtures["examples"].as_array().unwrap() {
            assert_eq!(
                accepts(&published, &example["input"], &published),
                validate_input(&example["input"]).is_ok(),
                "{}",
                example["id"]
            );
        }
        let recall = &published["oneOf"][0];
        assert_eq!(recall["properties"]["action"]["const"], "recall");
        assert_eq!(recall["properties"]["recall_for"]["type"], "string");
        assert!(recall["properties"]["recall_for"]["description"].is_string());
    }
    #[test]
    fn duplicate_keys_rejected() {
        assert!(
            crate::parse_request(r#"{"action":"recall","recall_for":"a","recall_for":"b"}"#)
                .is_err()
        );
    }
    #[test]
    fn fractional_integer_rejected() {
        assert!(crate::parse_request(
            r#"{"action":"history","memory_id":"x","context_layers":1.0}"#
        )
        .is_err());
    }
}
