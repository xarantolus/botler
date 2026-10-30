//! The Python SDK's op wrappers are generated from the hub's `OPS` specs, and the binary embeds
//! every SDK file. Run with `BOTLER_BLESS=1` to rewrite `botler/_ops.py` after changing an op.
//! Granted Python tools reach the agent container.

mod common;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use botler_hub::ops::OPS;
use serde_json::Value;

fn sdk_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../python/botler-sdk")
}

const PY_KEYWORDS: &[&str] = &["in", "from", "for", "is", "as", "class", "def", "if", "else", "global", "lambda"];

/// Python annotation for a JSON-schema property, without `| None`.
fn py_type(prop: &Value) -> &'static str {
    let ty = match prop.get("type") {
        Some(Value::String(t)) => t.as_str(),
        // `["string", "null"]` from `Option<String>`.
        Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).find(|t| *t != "null").unwrap_or(""),
        _ => "",
    };
    match ty {
        "string" => "str",
        "boolean" => "bool",
        "integer" => "int",
        "number" => "float",
        "array" if prop.pointer("/items/type").and_then(Value::as_str) == Some("string") => "list[str]",
        "array" => "list[Any]",
        "object" => "dict[str, Any]",
        _ => "Any",
    }
}

fn doc(text: &str) -> String {
    text.replace('\\', "\\\\").replace("\"\"\"", "\\\"\\\"\\\"")
}

fn generate() -> String {
    let mut out = String::from(
        "\"\"\"Every hub operation as a function. GENERATED from the hub's `OPS` specs by\n\
         `BOTLER_BLESS=1 cargo test -p botler --test sdk`; do not edit. The hub decides who may call what.\"\"\"\n\n\
         from __future__ import annotations\n\n\
         from typing import Any\n\n\
         from ._client import call\n",
    );
    for op in OPS {
        let schema = (op.schema)();
        let empty = serde_json::Map::new();
        // A schema without properties is either an empty struct or a union (`oneOf`).
        let union = schema.get("oneOf").is_some() || schema.get("anyOf").is_some();
        let props = schema.get("properties").and_then(Value::as_object).or((!union).then_some(&empty));
        let required: Vec<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|r| r.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let mut params = Vec::new();
        let mut body = Vec::new();
        let mut arg_docs = String::new();
        match props {
            Some(props) => {
                // Required first; all keyword-only.
                let mut fields: Vec<(&String, &Value)> = props.iter().collect();
                fields.sort_by_key(|(k, _)| !required.contains(&k.as_str()));
                for (key, prop) in fields {
                    let py = if PY_KEYWORDS.contains(&key.as_str()) { format!("{key}_") } else { key.clone() };
                    let ty = py_type(prop);
                    if required.contains(&key.as_str()) {
                        params.push(format!("{py}: {ty}"));
                    } else if ty == "Any" {
                        params.push(format!("{py}: Any = None"));
                    } else {
                        params.push(format!("{py}: {ty} | None = None"));
                    }
                    body.push(format!("\"{key}\": {py}"));
                    if let Some(d) = prop.get("description").and_then(Value::as_str) {
                        let _ = write!(arg_docs, "\n        {py}: {}", doc(&d.replace('\n', " ")));
                    }
                }
            }
            // Not a plain object (e.g. a tagged union): pass the arguments through.
            None => params.push("**args: Any".into()),
        }
        let sig = if params.is_empty() {
            String::new()
        } else if props.is_some() {
            format!("*, {}", params.join(", "))
        } else {
            params.join(", ")
        };
        let call_args = if props.is_some() { format!("{{{}}}", body.join(", ")) } else { "args".into() };
        let args_section = if arg_docs.is_empty() { String::new() } else { format!("\n\n    Args:{arg_docs}\n    ") };
        // Every op returns a JSON object or a list of them.
        let (_, output) = (op.ts_names)(&ts_rs::Config::new());
        let ret = if output.starts_with("Array<") { "list[dict[str, Any]]" } else { "dict[str, Any]" };
        let _ = write!(
            out,
            "\n\ndef {name}({sig}) -> {ret}:\n    \"\"\"{desc}{args_section}\"\"\"\n    return call(\"{name}\", {call_args})\n",
            name = op.name,
            desc = doc(op.description),
        );
    }
    out
}

#[test]
fn sdk_ops_are_current() {
    let path = sdk_dir().join("botler/_ops.py");
    let want = generate();
    if std::env::var_os("BOTLER_BLESS").is_some() {
        std::fs::write(&path, &want).unwrap();
        return;
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(have == want, "{} is stale; run `BOTLER_BLESS=1 cargo test -p botler --test sdk`", path.display());
}

#[test]
fn every_sdk_file_is_embedded() {
    let mut on_disk: Vec<String> = std::fs::read_dir(sdk_dir().join("botler"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".py") || n == "py.typed")
        .map(|n| format!("botler/{n}"))
        .collect();
    on_disk.push("pyproject.toml".into());
    on_disk.sort();
    let mut embedded: Vec<String> = botler_hub::embedded::SDK.iter().map(|(p, _)| (*p).to_string()).collect();
    embedded.sort();
    assert_eq!(embedded, on_disk, "update embedded::SDK");
}

#[tokio::test]
async fn granted_python_tools_reach_the_agent() {
    use botler_core::event::kinds;
    use botler_core::{Event, Provenance, Source};
    use botler_hub::agents;
    use botler_runtime::Mount;

    let e = common::env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-test").await.unwrap();
    e.commit(&[
        ("agents/counter/agent.toml", "kind = \"persistent\"\ntools = [\"example.*\"]\n"),
        ("agents/counter/AGENTS.md", "counts words"),
        // The template's agents are granted web.*; this one has no tools.
        ("agents/plain/agent.toml", "kind = \"persistent\"\n"),
        ("agents/plain/AGENTS.md", "no tools"),
    ])
    .await;
    for to in ["counter", "plain"] {
        let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, chrono::Utc::now())
            .to(to)
            .payload(serde_json::json!({"text": "hi"}));
        e.hub.emit(ev).await.unwrap();
    }
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();

    // The template's example tool, as generated into tools.manifest.json.
    let tools: Value =
        serde_json::from_str(&std::fs::read_to_string(e.hub.paths.agent_dir("counter").join("tools.json")).unwrap())
            .unwrap();
    assert_eq!(tools[0]["name"], "example.word_count");
    assert_eq!(tools[0]["module"], "example");
    let containers = e.fake.containers();
    let monorepo = |name: &str| {
        containers[&agents::container_name(&e.hub, name)].spec.mounts.iter().find_map(|m| match m {
            Mount::Bind { src, dst, read_only } if dst == "/botler/applied" => Some((src.clone(), *read_only)),
            _ => None,
        })
    };
    let (src, ro) = monorepo("counter").expect("applied commit mounted for tool code");
    assert!(ro && src.join("tools/example/src/example/__init__.py").exists() && src.join("uv.lock").exists());
    assert!(monorepo("plain").is_none(), "no tools granted, nothing mounted");
    let none = std::fs::read_to_string(e.hub.paths.agent_dir("plain").join("tools.json")).unwrap();
    assert_eq!(none.trim(), "[]");
}
