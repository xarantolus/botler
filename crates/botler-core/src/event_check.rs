//! Checking emitted events against their declarations, with feedback written for whoever has
//! to fix the payload (often a model).

use serde::Serialize;
use serde_json::Value;

use crate::defs::{Definitions, Undeclared};

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FieldError {
    /// JSON pointer into the payload, e.g. `/sender`. Empty for the payload itself.
    pub path: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Check {
    Ok,
    /// Declared but without a schema: accepted unchecked.
    Unchecked,
    /// Not declared and the policy allows it.
    UndeclaredAllowed,
    /// Not declared and the policy rejects it. `similar` are declared types with a close name.
    Undeclared {
        similar: Vec<String>,
    },
    Invalid {
        errors: Vec<FieldError>,
        schema: Value,
    },
}

impl Check {
    pub fn accepted(&self) -> bool {
        matches!(self, Self::Ok | Self::Unchecked | Self::UndeclaredAllowed)
    }

    /// A message for the emitter explaining what to change.
    pub fn feedback(&self, kind: &str) -> Option<String> {
        match self {
            Self::Ok | Self::Unchecked => None,
            Self::UndeclaredAllowed => Some(format!(
                "Accepted, but event type `{kind}` is not declared anywhere. Declare it (with a schema) in the monorepo so consumers can rely on its shape."
            )),
            Self::Undeclared { similar } => {
                let mut s = format!("Event type `{kind}` is not declared, so it was rejected.");
                if !similar.is_empty() {
                    s.push_str(&format!(
                        " Did you mean: {}?",
                        similar.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")
                    ));
                }
                s.push_str(" Use `list_event_types` to see declared types, or declare a new one in the monorepo (events/ or a pydantic model) and propose the change.");
                Some(s)
            }
            Self::Invalid { errors, schema } => {
                let mut s = format!("The payload does not match the schema of `{kind}`, so the event was rejected:\n");
                for e in errors {
                    let at = if e.path.is_empty() { "payload".to_string() } else { format!("`{}`", e.path) };
                    s.push_str(&format!("- {at}: {}\n", e.message));
                }
                let pretty =
                    serde_json::to_string_pretty(schema).unwrap_or_else(|e| format!("<schema unprintable: {e}>"));
                s.push_str(&format!("\nExpected schema:\n```json\n{pretty}\n```\nFix the payload and emit again."));
                Some(s)
            }
        }
    }
}

/// Compile a schema; used at load time to reject invalid declarations.
pub fn compile(schema: &Value) -> Result<jsonschema::Validator, String> {
    jsonschema::validator_for(schema).map_err(|e| e.to_string())
}

/// Check one event against the definitions. Built-in hub types are not checked here; the hub
/// produces those itself and emitters may not use them.
pub fn check(defs: &Definitions, kind: &str, payload: &Value) -> Check {
    let Some(decl) = defs.events.get(kind) else {
        return match defs.instance.events.undeclared {
            Undeclared::Warn => Check::UndeclaredAllowed,
            Undeclared::Reject => Check::Undeclared { similar: similar(defs, kind) },
        };
    };
    let Some(schema) = &decl.schema else { return Check::Unchecked };
    let validator = match compile(schema) {
        Ok(v) => v,
        // Load-time validation rejects invalid schemas; if one slips through, fail closed.
        Err(e) => {
            return Check::Invalid {
                errors: vec![FieldError {
                    path: String::new(),
                    message: format!("the declared schema is invalid: {e}"),
                }],
                schema: schema.clone(),
            };
        }
    };
    let errors: Vec<FieldError> = validator
        .iter_errors(payload)
        .take(20)
        .map(|e| FieldError { path: e.instance_path().to_string(), message: e.to_string() })
        .collect();
    if errors.is_empty() { Check::Ok } else { Check::Invalid { errors, schema: schema.clone() } }
}

/// Declared types sharing a dotted prefix or close in spelling.
fn similar(defs: &Definitions, kind: &str) -> Vec<String> {
    let prefix = kind.split('.').next().unwrap_or(kind);
    let mut out: Vec<(usize, String)> = defs
        .events
        .keys()
        .filter_map(|k| {
            let d = distance(k, kind);
            (k.starts_with(&format!("{prefix}.")) || d <= 3).then(|| (d, k.clone()))
        })
        .collect();
    out.sort();
    out.into_iter().take(5).map(|(_, k)| k).collect()
}

fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + usize::from(ca != *cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defs::{MemTree, load};
    use serde_json::json;

    fn defs(extra_botler: &str) -> Definitions {
        let tree = MemTree::new()
            .with(
                "botler.toml",
                format!("{extra_botler}\n[models]\ndefault = \"d\"\n[models.routes.d]\nprovider = \"openrouter\"\nmodel = \"x\"\n"),
            )
            .with(
                "events/mail.toml",
                r#"
[[event]]
type = "mail.received"
schema = { type = "object", required = ["sender", "subject"], properties = { sender = { type = "string" }, subject = { type = "string" }, size = { type = "integer", minimum = 0 } }, additionalProperties = false }

[[event]]
type = "mail.sent"
"#,
            )
            .with(
                "events.manifest.json",
                r#"[{"type": "home.arrival", "description": "user is about to arrive", "schema": {"type": "object", "required": ["eta_minutes"], "properties": {"eta_minutes": {"type": "integer"}}}}]"#,
            );
        load(&tree).into_result().unwrap_or_else(|d| panic!("{d:#?}"))
    }

    #[test]
    fn valid_invalid_unchecked() {
        let d = defs("");
        assert_eq!(check(&d, "mail.received", &json!({"sender": "a@b", "subject": "hi"})), Check::Ok);
        assert_eq!(check(&d, "mail.sent", &json!({"anything": 1})), Check::Unchecked);
        assert_eq!(check(&d, "home.arrival", &json!({"eta_minutes": 12})), Check::Ok, "manifest declarations count");

        let bad = check(&d, "mail.received", &json!({"sender": 3, "size": -1, "extra": true}));
        let Check::Invalid { errors, .. } = &bad else { panic!("{bad:?}") };
        let paths: Vec<&str> = errors.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"/sender") && paths.contains(&"/size") && paths.contains(&""), "{errors:#?}");
        let fb = bad.feedback("mail.received").unwrap();
        assert!(fb.contains("`/sender`") && fb.contains("Expected schema") && fb.contains("emit again"), "{fb}");
    }

    #[test]
    fn undeclared_policy_and_suggestions() {
        let strict = defs("");
        let c = check(&strict, "mail.recieved", &json!({}));
        assert!(!c.accepted());
        assert!(c.feedback("mail.recieved").unwrap().contains("`mail.received`"));
        let lenient = defs("[events]\nundeclared = \"warn\"\n");
        let c = check(&lenient, "brand.new", &json!({}));
        assert!(c.accepted());
        assert!(c.feedback("brand.new").unwrap().contains("not declared"));
    }

    #[test]
    fn invalid_schema_and_builtin_redeclaration_fail_load() {
        let tree = MemTree::new()
            .with("botler.toml", "[models]\ndefault = \"d\"\n[models.routes.d]\nprovider = \"p\"\nmodel = \"x\"\n")
            .with("events/x.toml", "[[event]]\ntype = \"x.y\"\nschema = { type = \"nonsense\" }\n\n[[event]]\ntype = \"runnable.failed\"\n");
        let report = load(&tree);
        let msgs: Vec<String> = report.errors().map(|d| d.message.clone()).collect();
        assert!(msgs.iter().any(|m| m.contains("schema of x.y is invalid")), "{msgs:#?}");
        assert!(msgs.iter().any(|m| m.contains("built-in")), "{msgs:#?}");
    }
}
