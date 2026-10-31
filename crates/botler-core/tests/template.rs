//! The shipped monorepo template must always load cleanly.

use botler_core::defs::{FsTree, Severity, load};

#[test]
fn template_loads_without_errors_or_warnings() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../template");
    let report = load(&FsTree::new(root));
    let problems: Vec<String> = report.diagnostics.iter().map(ToString::to_string).collect();
    assert!(report.diagnostics.iter().all(|d| d.severity != Severity::Error), "{problems:#?}");
    assert!(problems.is_empty(), "template should not produce warnings either: {problems:#?}");
    let defs = report.definitions.expect("definitions");
    assert!(defs.agents.contains_key(botler_core::FRONT_DOOR));
    assert!(defs.agents.contains_key("triage"), "failures need a triage agent");
    assert_eq!(defs.persistent_agents().count(), 1);
}

#[test]
fn tool_render_hints_are_typed() {
    use botler_core::defs::{MemTree, ToolRender};
    let tree = |render: &str| {
        let tool =
            format!(r#"[{{"name": "t.f", "parameters": {{}}, "module": "t", "function": "f", "render": {render}}}]"#);
        MemTree::new().with("botler.toml", "[models]\ndefault = \"p/m\"\n").with("tools.manifest.json", tool)
    };
    let errors = |render: &str| load(&tree(render)).errors().map(|d| d.message.clone()).collect::<Vec<_>>();
    let ok = load(&tree(r#"{"kind": "links", "items": "results"}"#));
    let want = ToolRender::Links { items: Some("results".into()), title: None, url: None, snippet: None };
    let defs = ok.into_result().unwrap_or_else(|d| panic!("{d:#?}"));
    assert_eq!(defs.tools["t.f"].render, Some(want));
    for (bad, why) in [
        (r#"{"kind": "carousel"}"#, "unknown variant `carousel`"),
        (r#"{"kind": "table", "rows": "x"}"#, "unknown field `rows`"),
        (r#"{"kind": "markdown", "field": 3}"#, "invalid type"),
    ] {
        let errs = errors(bad);
        assert!(errs.iter().any(|e| e.contains(why)), "{bad}: {errs:?}");
    }
}
