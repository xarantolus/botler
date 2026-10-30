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
