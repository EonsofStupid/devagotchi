//! Test-only: a tool result's `structuredContent` checked against the output schema the tool
//! declares, the way an MCP client checks it. Claude's client refuses a whole result over
//! one property its schema requires and the result leaves out (increment 78: `offers`
//! wrote no `description` or `url` for a blank offering, and Claude refused every call).
//!
//! It knows the part of JSON Schema 2020-12 that schemars writes. A keyword it does not
//! know is a failure, never a pass: a schema it cannot read is not one it has checked.

use serde_json::{Map, Value};

/// Every way `value` breaks `schema`, as `<path> <what>` (`/offerings/1 must have required
/// property 'url'`); empty when it satisfies it.
pub(crate) fn errors(schema: &Value, value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    check(schema, schema, value, "", &mut out);
    out
}

/// Panics, naming every mismatch, when a successful call to `tool` answered with
/// `structured` that its declared output schema refuses.
pub(crate) fn assert_matches(tool: &str, schema: &Value, structured: &Value) {
    let found = errors(schema, structured);
    assert!(
        found.is_empty(),
        "{tool}: structuredContent does not match the tool's output schema: {}\nresult: {structured}\nschema: {schema}",
        found.join("; ")
    );
}

/// The output schema `tool` declares, as `tools/list` gives it; `None` when it declares none.
pub(crate) fn declared(tool: &str) -> Option<Value> {
    crate::hub::tools()
        .into_iter()
        .find(|t| t.name == tool)
        .and_then(|t| t.output_schema)
        .map(|schema| Value::Object(schema.as_ref().clone()))
}

/// What a client does with a `tools/call` result of `tool`: a successful one of a tool with
/// an output schema carries `structuredContent`, and it matches. Refusals (`isError`) and
/// JSON-RPC errors are not results to check.
pub(crate) fn assert_result(tool: &str, answer: &Value) {
    if answer["isError"] == true || answer.get("content").is_none() {
        return;
    }
    let Some(schema) = declared(tool) else {
        return;
    };
    let Some(structured) = answer.get("structuredContent") else {
        panic!("{tool}: has an output schema but answered without structuredContent: {answer}");
    };
    assert_matches(tool, &schema, structured);
}

fn at(path: &str) -> &str {
    if path.is_empty() { "(root)" } else { path }
}

fn check(root: &Value, schema: &Value, value: &Value, path: &str, out: &mut Vec<String>) {
    let rules = match schema {
        Value::Bool(true) => return,
        Value::Bool(false) => {
            out.push(format!("{} is not allowed", at(path)));
            return;
        }
        Value::Object(rules) => rules,
        other => {
            out.push(format!("{}: the schema is not a schema: {other}", at(path)));
            return;
        }
    };
    for (keyword, rule) in rules {
        match keyword.as_str() {
            // Annotations: they say nothing a value must be.
            "$schema" | "$defs" | "definitions" | "$comment" | "title" | "description"
            | "default" | "examples" | "deprecated" | "readOnly" | "writeOnly" | "format" => {}
            "$ref" => match resolve(root, rule) {
                Some(target) => check(root, target, value, path, out),
                None => out.push(format!("{}: unresolved $ref {rule}", at(path))),
            },
            "type" => {
                let types: Vec<&str> = match rule {
                    Value::String(one) => vec![one.as_str()],
                    Value::Array(many) => many.iter().filter_map(Value::as_str).collect(),
                    _ => vec![],
                };
                if !types.iter().any(|t| is_type(value, t)) {
                    out.push(format!("{} must be {}", at(path), types.join(" or ")));
                }
            }
            "properties" => {
                if let (Some(properties), Some(object)) = (rule.as_object(), value.as_object()) {
                    for (name, sub) in properties {
                        if let Some(field) = object.get(name) {
                            check(root, sub, field, &format!("{path}/{name}"), out);
                        }
                    }
                }
            }
            "required" => {
                if let Some(object) = value.as_object() {
                    for name in rule.as_array().into_iter().flatten().filter_map(Value::as_str) {
                        if !object.contains_key(name) {
                            out.push(format!("{} must have required property '{name}'", at(path)));
                        }
                    }
                }
            }
            "additionalProperties" => {
                if let Some(object) = value.as_object() {
                    let known = rules.get("properties").and_then(Value::as_object);
                    for (name, field) in object {
                        if !known.is_some_and(|k| k.contains_key(name)) {
                            check(root, rule, field, &format!("{path}/{name}"), out);
                        }
                    }
                }
            }
            "items" => {
                if let Some(items) = value.as_array() {
                    let skip = rules.get("prefixItems").and_then(Value::as_array).map_or(0, Vec::len);
                    for (i, item) in items.iter().enumerate().skip(skip) {
                        check(root, rule, item, &format!("{path}/{i}"), out);
                    }
                }
            }
            "prefixItems" => {
                if let (Some(prefix), Some(items)) = (rule.as_array(), value.as_array()) {
                    for (i, (sub, item)) in prefix.iter().zip(items).enumerate() {
                        check(root, sub, item, &format!("{path}/{i}"), out);
                    }
                }
            }
            "minItems" | "maxItems" => bound(keyword, rule, value.as_array().map(Vec::len), path, out),
            "minLength" | "maxLength" => {
                bound(keyword, rule, value.as_str().map(|s| s.chars().count()), path, out)
            }
            "minProperties" | "maxProperties" => {
                bound(keyword, rule, value.as_object().map(Map::len), path, out)
            }
            "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" => {
                if let (Some(limit), Some(number)) = (rule.as_f64(), value.as_f64()) {
                    let ok = match keyword.as_str() {
                        "minimum" => number >= limit,
                        "maximum" => number <= limit,
                        "exclusiveMinimum" => number > limit,
                        _ => number < limit,
                    };
                    if !ok {
                        out.push(format!("{} must be {keyword} {limit}", at(path)));
                    }
                }
            }
            "enum" => {
                if !rule.as_array().is_some_and(|allowed| allowed.contains(value)) {
                    out.push(format!("{} must be one of {rule}", at(path)));
                }
            }
            "const" => {
                if rule != value {
                    out.push(format!("{} must be {rule}", at(path)));
                }
            }
            "anyOf" | "oneOf" | "allOf" => {
                let branches = rule.as_array().map(Vec::as_slice).unwrap_or_default();
                let failing: Vec<Vec<String>> = branches
                    .iter()
                    .map(|sub| {
                        let mut found = Vec::new();
                        check(root, sub, value, path, &mut found);
                        found
                    })
                    .collect();
                let passing = failing.iter().filter(|f| f.is_empty()).count();
                match keyword.as_str() {
                    "allOf" => out.extend(failing.into_iter().flatten()),
                    "anyOf" if passing == 0 => out.push(format!(
                        "{} must match a schema in anyOf: {}",
                        at(path),
                        failing.into_iter().flatten().collect::<Vec<_>>().join("; ")
                    )),
                    "oneOf" if passing != 1 => out.push(format!(
                        "{} must match exactly one schema in oneOf, matches {passing}",
                        at(path)
                    )),
                    _ => {}
                }
            }
            "not" => {
                let mut found = Vec::new();
                check(root, rule, value, path, &mut found);
                if found.is_empty() {
                    out.push(format!("{} must not match {rule}", at(path)));
                }
            }
            unknown => out.push(format!(
                "{}: the schema says `{unknown}`, which this check does not know; teach it before trusting it",
                at(path)
            )),
        }
    }
}

fn bound(keyword: &str, rule: &Value, len: Option<usize>, path: &str, out: &mut Vec<String>) {
    let (Some(limit), Some(len)) = (rule.as_u64(), len) else {
        return;
    };
    let ok = if keyword.starts_with("min") { len as u64 >= limit } else { len as u64 <= limit };
    if !ok {
        out.push(format!("{} must have {keyword} {limit}, has {len}", at(path)));
    }
}

fn is_type(value: &Value, name: &str) -> bool {
    match name {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "number" => value.is_number(),
        "integer" => {
            value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|n| n.fract() == 0.0)
        }
        _ => false,
    }
}

/// A `$ref` within the same document: `#`, or `#/<pointer>`.
fn resolve<'a>(root: &'a Value, reference: &Value) -> Option<&'a Value> {
    let pointer = reference.as_str()?.strip_prefix('#')?;
    if pointer.is_empty() {
        return Some(root);
    }
    root.pointer(pointer)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::errors;

    #[test]
    fn it_finds_what_a_client_refuses_and_passes_what_it_accepts() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "offerings": { "type": "array", "items": { "$ref": "#/$defs/Offering" } },
                "count": { "type": "integer", "format": "uint64", "minimum": 0 },
                "note": { "type": ["string", "null"] },
                "state": { "type": "string", "enum": ["a", "b"] }
            },
            "required": ["offerings", "count", "state"],
            "$defs": { "Offering": {
                "type": "object",
                "properties": { "name": { "type": "string" }, "url": { "type": "string" } },
                "required": ["name", "url"]
            } }
        });
        let good = json!({ "offerings": [{ "name": "a", "url": "" }], "count": 3, "state": "a" });
        assert_eq!(errors(&schema, &good), Vec::<String>::new());
        let bad = json!({ "offerings": [{ "name": "a", "url": "u" }, { "name": "b" }], "count": -1, "note": 1, "state": "c" });
        let found = errors(&schema, &bad);
        for expected in [
            "/offerings/1 must have required property 'url'",
            "/count must be minimum 0",
            "/note must be string or null",
            "/state must be one of",
        ] {
            assert!(found.iter().any(|f| f.starts_with(expected)), "{expected} in {found:?}");
        }
        assert!(errors(&schema, &json!({ "count": 1, "state": "a" }))[0].contains("'offerings'"));
        // A keyword it does not know fails; it is never skipped.
        assert!(!errors(&json!({ "type": "string", "pattern": "^a" }), &json!("a")).is_empty());
        // anyOf / oneOf as schemars writes enums and options.
        let either = json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] });
        assert!(errors(&either, &json!(null)).is_empty());
        assert!(!errors(&either, &json!(1)).is_empty());
        let one = json!({ "oneOf": [{ "const": "x" }, { "type": "string" }] });
        assert!(!errors(&one, &json!("x")).is_empty());
        assert!(errors(&one, &json!("y")).is_empty());
    }
}
