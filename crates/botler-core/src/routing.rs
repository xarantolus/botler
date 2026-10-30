//! Routing: which targets an event reaches. Pure: definitions + event in, dispatches out.

use serde::Serialize;

use crate::defs::{Definitions, Target, TriggerKind};
use crate::event::{Event, Source};
use crate::limits::MAX_CHAIN_HOPS;

/// One thing the hub must do because of an event.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Dispatch {
    /// The trigger that matched; `None` for direct addressing.
    pub trigger: Option<String>,
    pub action: Action,
    /// Message text for agent targets (the trigger's `prompt`), prepended to the event.
    pub prompt: Option<String>,
    /// Shadow-mode triggers are recorded but not executed.
    pub shadow: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum Action {
    /// Deliver into an existing instance (persistent or a live ephemeral one).
    Deliver(String),
    Spawn(String),
    Run(String),
}

impl From<&Target> for Action {
    fn from(t: &Target) -> Self {
        match t {
            Target::Resume(n) => Self::Deliver(n.clone()),
            Target::Spawn(n) => Self::Spawn(n.clone()),
            Target::Run(n) => Self::Run(n.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Routed {
    Dispatches(Vec<Dispatch>),
    /// The correlation chain is too long: agents are likely ping-ponging.
    CycleBreaker {
        hops: u32,
    },
}

pub fn route(defs: &Definitions, event: &Event) -> Routed {
    if event.hops > MAX_CHAIN_HOPS {
        return Routed::CycleBreaker { hops: event.hops };
    }
    let mut out = Vec::new();
    if let Some(to) = &event.to {
        out.push(Dispatch { trigger: None, action: Action::Deliver(to.clone()), prompt: None, shadow: false });
    }
    for t in defs.triggers.values() {
        let TriggerKind::On { pattern } = &t.kind else { continue };
        if !matches_type(pattern, &event.kind) {
            continue;
        }
        if !t.spec.filter.iter().all(|(path, want)| event.field(path) == Some(want)) {
            continue;
        }
        // An agent never wakes itself through its own subscription; that is a loop by construction.
        if let (Source::Agent(src), Target::Resume(dst)) = (&event.source, &t.target)
            && src == dst
        {
            continue;
        }
        let action = Action::from(&t.target);
        if out.iter().any(|d| d.action == action && d.trigger.is_none()) {
            continue; // already directly addressed
        }
        out.push(Dispatch {
            trigger: Some(t.id.clone()),
            action,
            prompt: t.spec.prompt.clone(),
            shadow: t.spec.mode == crate::defs::Mode::Shadow,
        });
    }
    Routed::Dispatches(out)
}

/// Event type glob: `*` matches within a segment, `**` across segments; plain strings match exactly.
pub fn matches_type(pattern: &str, kind: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == kind;
    }
    let p = pattern.replace('.', "/");
    let k = kind.replace('.', "/");
    globset::GlobBuilder::new(&p)
        .literal_separator(true)
        .build()
        .map(|g| g.compile_matcher().is_match(&k))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defs::{MemTree, load};
    use crate::event::Provenance;
    use chrono::Utc;
    use serde_json::json;

    fn defs() -> Definitions {
        let tree = MemTree::new()
            .with("botler.toml", "[models]\ndefault = \"d\"\n[models.routes.d]\nprovider = \"openrouter\"\nmodel = \"x\"\n")
            .with("images/agent/Dockerfile", "FROM x")
            .with("agents/main/agent.toml", "kind = \"persistent\"\n[[trigger]]\nname = \"mail\"\non = \"mail.*\"\nfilter = { folder = \"inbox\" }\nprompt = \"New mail\"\n")
            .with("agents/main/AGENTS.md", "hi")
            .with("agents/triage/agent.toml", "[[trigger]]\nname = \"fail\"\non = \"runnable.failed\"\nmode = \"shadow\"\n")
            .with("agents/triage/AGENTS.md", "hi");
        let r = load(&tree);
        r.into_result().unwrap_or_else(|d| panic!("{d:#?}"))
    }

    fn ev(kind: &str, payload: serde_json::Value) -> Event {
        Event::new(kind, Source::Runnable("poller".into()), Provenance::External, Utc::now()).payload(payload)
    }

    #[test]
    fn glob_semantics() {
        assert!(matches_type("mail.*", "mail.received"));
        assert!(!matches_type("mail.*", "mail.received.spam"));
        assert!(matches_type("mail.**", "mail.received.spam"));
        assert!(matches_type("x", "x"));
        assert!(!matches_type("x", "xy"));
    }

    #[test]
    fn subscription_with_filter() {
        let d = defs();
        let Routed::Dispatches(hit) = route(&d, &ev("mail.received", json!({"folder": "inbox"}))) else { panic!() };
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].action, Action::Deliver("main".into()));
        assert_eq!(hit[0].prompt.as_deref(), Some("New mail"));
        let Routed::Dispatches(miss) = route(&d, &ev("mail.received", json!({"folder": "spam"}))) else { panic!() };
        assert!(miss.is_empty());
    }

    #[test]
    fn shadow_and_spawn() {
        let d = defs();
        let Routed::Dispatches(hit) = route(&d, &ev("runnable.failed", json!({}))) else { panic!() };
        assert_eq!(hit[0].action, Action::Spawn("triage".into()));
        assert!(hit[0].shadow);
    }

    #[test]
    fn no_self_wake_and_dedup_direct() {
        let d = defs();
        let mut e = ev("mail.received", json!({"folder": "inbox"}));
        e.source = Source::Agent("main".into());
        let Routed::Dispatches(hit) = route(&d, &e) else { panic!() };
        assert!(hit.is_empty());
        let e = ev("mail.received", json!({"folder": "inbox"})).to("main");
        let Routed::Dispatches(hit) = route(&d, &e) else { panic!() };
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].trigger, None);
    }

    #[test]
    fn cycle_breaker() {
        let d = defs();
        let mut e = ev("message", json!({}));
        e.hops = MAX_CHAIN_HOPS + 1;
        assert!(matches!(route(&d, &e), Routed::CycleBreaker { .. }));
    }
}
