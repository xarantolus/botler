//! Forms in questions: `ask_user(form = <JSON Schema>)`. The schema is checked when the question is
//! asked, the user's values against it when they answer; file fields hold uploads, which reach the
//! agent's inbox like message attachments.

use serde_json::{Map, Value};

/// The `format` of a string field the user fills with a file (an upload, `<id>/<name>`).
pub const FILE: &str = "file";
const MAX_FIELDS: usize = 30;

/// Why a form schema is refused, written for the agent that sent it.
pub fn check_schema(schema: &Value) -> Result<(), String> {
    let props = schema.get("properties").and_then(Value::as_object).filter(|p| !p.is_empty()).ok_or(
        "a form is a JSON Schema object with `properties`, e.g. {\"properties\": {\"folder\": {\"type\": \"string\"}}}",
    )?;
    if schema.get("type").is_some_and(|t| t != "object") {
        return Err("a form's top-level `type` must be \"object\"".into());
    }
    if props.len() > MAX_FIELDS {
        return Err(format!("{} fields; a form has at most {MAX_FIELDS}", props.len()));
    }
    for (key, field) in props {
        check_field(field).map_err(|e| format!("field `{key}`: {e}"))?;
    }
    botler_core::event_check::compile(schema).map(|_| ()).map_err(|e| format!("not a valid JSON Schema: {e}"))
}

const SUPPORTED: &str = "fields are strings (optionally `enum`, `format`: date, date-time, time, file, \
                         or `\"x-multiline\": true`), numbers, integers, booleans, or arrays of an `enum` \
                         string (multi-select) or of files";

fn check_field(f: &Value) -> Result<(), String> {
    let ty = f.get("type").and_then(Value::as_str).ok_or_else(|| format!("give it a `type`; {SUPPORTED}"))?;
    match ty {
        "string" | "number" | "integer" | "boolean" => Ok(()),
        "array" => {
            let items = f.get("items").ok_or_else(|| format!("an array needs `items`; {SUPPORTED}"))?;
            let strings = items.get("type").and_then(Value::as_str) == Some("string");
            if strings && (items.get("enum").is_some() || items.get("format").and_then(Value::as_str) == Some(FILE)) {
                Ok(())
            } else {
                Err(format!("unsupported array; {SUPPORTED}"))
            }
        }
        other => Err(format!("type {other:?} is not supported; {SUPPORTED}")),
    }
}

/// Why the values do not match the schema, one line per problem.
pub fn check_values(schema: &Value, values: &Value) -> Result<(), String> {
    let v = botler_core::event_check::compile(schema).map_err(|e| format!("the form's schema is invalid: {e}"))?;
    let errors: Vec<String> = v
        .iter_errors(values)
        .take(20)
        .map(|e| {
            let at = e.instance_path().to_string();
            if at.is_empty() { e.to_string() } else { format!("{}: {e}", at.trim_start_matches('/')) }
        })
        .collect();
    if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
}

/// Whether a field (or an array field's items) holds files.
fn is_file(field: &Value) -> bool {
    let f = field.get("items").unwrap_or(field);
    f.get("format").and_then(Value::as_str) == Some(FILE)
}

/// The uploads the values name, by field.
pub fn files(schema: &Value, values: &Value) -> Vec<String> {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else { return vec![] };
    props
        .iter()
        .filter(|(_, f)| is_file(f))
        .filter_map(|(k, _)| values.get(k))
        .flat_map(|v| match v {
            Value::Array(a) => a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
            Value::String(s) => vec![s.clone()],
            _ => vec![],
        })
        .collect()
}

/// The values with each upload replaced by where the agent finds it (`map`: upload → path).
pub fn with_paths(values: &Value, map: &[(String, String)]) -> Value {
    match values {
        Value::String(s) => {
            map.iter().find(|(u, _)| u == s).map_or_else(|| values.clone(), |(_, p)| Value::String(p.clone()))
        }
        Value::Array(a) => Value::Array(a.iter().map(|v| with_paths(v, map)).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), with_paths(v, map))).collect::<Map<_, _>>())
        }
        _ => values.clone(),
    }
}

/// The answer's one line for the transcript: "Folder: INBOX · Every: 15".
pub fn summary(schema: &Value, values: &Value) -> String {
    let props = schema.get("properties").and_then(Value::as_object);
    let empty = Map::new();
    let given = values.as_object().unwrap_or(&empty);
    // In the form's order, then anything the schema does not name.
    let keys = props
        .into_iter()
        .flat_map(|p| p.keys())
        .chain(given.keys().filter(|k| !props.is_some_and(|p| p.contains_key(*k))));
    let line = keys
        .filter_map(|k| given.get(k).filter(|v| !v.is_null()).map(|v| (k, v)))
        .map(|(k, v)| {
            let title = props
                .and_then(|p| p.get(k))
                .and_then(|f| f.get("title"))
                .and_then(Value::as_str)
                .map_or_else(|| k.replace('_', " "), str::to_owned);
            let shown = match v {
                Value::String(s) => s.rsplit('/').next().unwrap_or(s).to_owned(),
                Value::Array(a) => a
                    .iter()
                    .map(|x| x.as_str().map_or_else(|| x.to_string(), |s| s.rsplit('/').next().unwrap_or(s).to_owned()))
                    .collect::<Vec<_>>()
                    .join(", "),
                other => other.to_string(),
            };
            format!("{title}: {shown}")
        })
        .collect::<Vec<_>>()
        .join(" · ");
    if line.is_empty() { "(submitted empty)".into() } else { line }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn form() -> Value {
        json!({
            "type": "object",
            "required": ["folder"],
            "properties": {
                "folder": {"type": "string", "title": "Folder", "default": "INBOX"},
                "every": {"type": "integer", "minimum": 1},
                "days": {"type": "array", "items": {"type": "string", "enum": ["mon", "tue"]}},
                "scan": {"type": "string", "format": "file"}
            }
        })
    }

    #[test]
    fn schemas() {
        assert_eq!(check_schema(&form()), Ok(()));
        assert!(check_schema(&json!({"type": "object"})).unwrap_err().contains("properties"));
        assert!(check_schema(&json!({"properties": {"x": {"type": "object"}}})).unwrap_err().contains("field `x`"));
        assert!(check_schema(&json!({"properties": {"x": {"type": "array", "items": {"type": "number"}}}})).is_err());
    }

    #[test]
    fn values_files_and_summary() {
        let f = form();
        assert!(check_values(&f, &json!({"every": 0})).unwrap_err().contains("folder"));
        let v = json!({"folder": "INBOX", "every": 15, "days": ["mon"], "scan": "u1/scan.pdf"});
        assert_eq!(check_values(&f, &v), Ok(()));
        assert_eq!(files(&f, &v), ["u1/scan.pdf"]);
        let p = with_paths(&v, &[("u1/scan.pdf".into(), "/inbox/files/u1/scan.pdf".into())]);
        assert_eq!(p["scan"], "/inbox/files/u1/scan.pdf");
        assert_eq!(summary(&f, &v), "Folder: INBOX · every: 15 · days: mon · scan: scan.pdf");
    }
}
