//! Merged changes reach running chats: an agent whose environment changed is restarted between
//! turns, resumes the same session, and is told what changed.

mod common;

use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::agents::{self, RESTART_DEBOUNCE_SECS, refresh_environments};
use chrono::{Duration, Utc};
use common::{Env, env};
use serde_json::{Value, json};

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
}

/// The template with Botler awake.
async fn botler_awake() -> Env {
    let e = env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk").await.unwrap();
    agents::keep_always_on(&e.hub).await.unwrap();
    settle().await;
    e
}

async fn wake_id(e: &Env) -> Option<String> {
    e.hub.agents.get("botler").await.map(|a| a.wake.clone())
}

/// The template's tool manifest plus a `web.weather` tool (granted to Botler by `web.*`).
fn manifest_with_weather() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../template/tools.manifest.json");
    let mut tools: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    tools.push(json!({"name": "web.weather", "description": "Weather.", "parameters": {"type": "object"},
        "effects": ["network"], "approval": false, "module": "web.weather", "function": "forecast"}));
    serde_json::to_string_pretty(&tools).unwrap()
}

async fn add_weather(e: &Env) {
    e.commit(&[("tools.manifest.json", &manifest_with_weather())]).await;
}

async fn message(e: &Env, text: &str) {
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to("botler")
        .payload(json!({"text": text}));
    e.hub.emit(ev).await.unwrap();
    e.route().await;
}

/// Refresh at `t` and again once the debounce has passed.
async fn refresh_after_debounce(e: &Env) {
    let t = Utc::now();
    refresh_environments(&e.hub, t).await.unwrap();
    refresh_environments(&e.hub, t + Duration::seconds(RESTART_DEBOUNCE_SECS + 1)).await.unwrap();
    settle().await;
}

fn container(e: &Env) -> String {
    agents::container_name(&e.hub, "botler")
}

#[tokio::test]
async fn an_unrelated_merge_restarts_nothing() {
    let e = botler_awake().await;
    let before = wake_id(&e).await.unwrap();
    let files = common::job("nightly", "command = [\"run\"]\n");
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    e.commit(&refs).await;
    refresh_after_debounce(&e).await;
    assert_eq!(wake_id(&e).await, Some(before));
}

#[tokio::test]
async fn a_new_tool_restarts_botler_in_the_same_session_and_it_is_told() {
    let e = botler_awake().await;
    let before = wake_id(&e).await.unwrap();
    let old_spec = e.fake.containers()[&container(&e)].spec.clone();
    add_weather(&e).await;
    refresh_after_debounce(&e).await;
    assert!(e.exited("botler").await, "stopped between turns");
    agents::keep_always_on(&e.hub).await.unwrap(); // always-on: back at once
    settle().await;
    let after = wake_id(&e).await.unwrap();
    assert_ne!(after, before);
    let spec = &e.fake.containers()[&container(&e)].spec;
    assert_eq!(spec.command, old_spec.command, "same session file, model and resources");
    assert_eq!(spec.mounts[0], old_spec.mounts[0], "same home");
    let tools = std::fs::read_to_string(e.hub.paths.agent_dir("botler").join("tools.json")).unwrap();
    assert!(tools.contains("web.weather"), "{tools}");

    // The next delivery carries the note first, without a turn of its own.
    message(&e, "what's the weather?").await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    let notes = e.fake.notes(&container(&e));
    assert!(
        notes.iter().any(|n| n.contains("environment.updated") && n.contains("new tools: web.weather")),
        "{notes:?}"
    );
    assert!(e.fake.prompts(&container(&e)).last().unwrap().contains("weather?"));
    // Told once.
    message(&e, "thanks").await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    assert_eq!(e.fake.notes(&container(&e)).iter().filter(|n| n.contains("environment.updated")).count(), 1);
}

#[tokio::test]
async fn a_delivery_right_after_a_merge_reaches_the_new_container() {
    // What a merged proposal's `proposal.updated` does for the agent that proposed it.
    let e = botler_awake().await;
    add_weather(&e).await;
    message(&e, "your change is merged").await;
    for _ in 0..3 {
        agents::dispatch_pending(&e.hub).await.unwrap();
        settle().await;
        agents::keep_always_on(&e.hub).await.unwrap();
    }
    let prompts = e.fake.prompts(&container(&e));
    assert!(prompts.last().unwrap().contains("your change is merged"));
    let tools = std::fs::read_to_string(e.hub.paths.agent_dir("botler").join("tools.json")).unwrap();
    assert!(tools.contains("web.weather"), "delivered to the restarted container");
    assert!(e.fake.notes(&container(&e)).iter().any(|n| n.contains("new tools: web.weather")));
}

#[tokio::test]
async fn a_busy_agent_finishes_its_turn_first() {
    let e = botler_awake().await;
    e.fake.set_hold_turns(true);
    message(&e, "long task").await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
    let before = wake_id(&e).await.unwrap();
    assert!(e.hub.agents.get("botler").await.unwrap().busy());
    add_weather(&e).await;
    refresh_after_debounce(&e).await;
    assert_eq!(wake_id(&e).await, Some(before), "not interrupted");
    assert!(e.fake.pi_event(&container(&e), json!({"type": "agent_settled"})));
    settle().await;
    refresh_environments(&e.hub, Utc::now() + Duration::minutes(1)).await.unwrap();
    settle().await;
    assert!(e.exited("botler").await, "restarted after the turn");
}

#[tokio::test]
async fn a_burst_of_merges_restarts_once() {
    let e = botler_awake().await;
    let t = Utc::now();
    add_weather(&e).await;
    refresh_environments(&e.hub, t).await.unwrap();
    e.commit(&[("agents/botler/AGENTS.md", "You are Botler. Be brief.\n")]).await;
    refresh_environments(&e.hub, t + Duration::seconds(RESTART_DEBOUNCE_SECS - 2)).await.unwrap();
    refresh_environments(&e.hub, t + Duration::seconds(RESTART_DEBOUNCE_SECS + 1)).await.unwrap();
    settle().await;
    assert!(e.hub.agents.get("botler").await.is_some(), "the second merge restarted the wait");
    refresh_environments(&e.hub, t + Duration::seconds(2 * RESTART_DEBOUNCE_SECS)).await.unwrap();
    settle().await;
    assert!(e.exited("botler").await);
    agents::keep_always_on(&e.hub).await.unwrap();
    settle().await;
    let after = wake_id(&e).await.unwrap();
    refresh_environments(&e.hub, t + Duration::seconds(5 * RESTART_DEBOUNCE_SECS)).await.unwrap();
    settle().await;
    assert_eq!(wake_id(&e).await, Some(after), "one restart for both merges");
    message(&e, "hi").await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    let note = e.fake.notes(&container(&e)).into_iter().find(|n| n.contains("environment.updated")).unwrap();
    assert!(note.contains("new tools: web.weather") && note.contains("AGENTS.md"), "{note}");
}

#[tokio::test]
async fn a_chats_context_size_restarts_it_between_turns() {
    let e = botler_awake().await;
    let before = wake_id(&e).await.unwrap();
    let user =
        botler_hub::ops::Caller { principal: botler_hub::auth::Principal::User, capabilities: vec![], runnable: None };
    let too_small =
        botler_hub::ops::call(&e.hub, &user, "set_chat_context", json!({"chat": "botler", "compact_at": 5000}));
    assert!(too_small.await.is_err());
    botler_hub::ops::call(&e.hub, &user, "set_chat_context", json!({"chat": "botler", "compact_at": 120_000}))
        .await
        .unwrap();
    assert_eq!(e.hub.store.instance("botler").await.unwrap().compact_at, Some(120_000));
    refresh_after_debounce(&e).await;
    agents::keep_always_on(&e.hub).await.unwrap();
    settle().await;
    assert_ne!(wake_id(&e).await, Some(before));
}

/// A chat whose last week would clearly have cost less at another context size is suggested it
/// once; answering `set <N>k` applies it.
#[tokio::test]
async fn a_chat_that_would_cost_less_at_another_context_size_is_suggested_it() {
    let e = botler_awake().await;
    let user =
        botler_hub::ops::Caller { principal: botler_hub::auth::Principal::User, capabilities: vec![], runnable: None };
    botler_hub::ops::call(&e.hub, &user, "set_chat_context", json!({"chat": "botler", "compact_at": 500_000}))
        .await
        .unwrap();
    // 400 calls, each adding 1.5k tokens to a context that never compacts at 500k.
    let start = chrono::Utc::now() - chrono::Duration::hours(10);
    let lines: Vec<String> = (0..400u64)
        .map(|i| {
            let context = 20_000 + i * 1_500;
            json!({"type": "message", "timestamp": (start + chrono::Duration::minutes(i as i64)).to_rfc3339(), "message": {
                "role": "assistant", "content": [{"type": "text", "text": "ok"}],
                "usage": {"input": 500, "cacheRead": context - 500, "output": 300, "reasoning": 100,
                          "cost": {"input": 2.5e-5, "cacheRead": (context - 500) as f64 * 1.5e-8, "output": 1.5e-4}}}})
            .to_string()
        })
        .collect();
    let inst = e.hub.store.instance("botler").await.unwrap();
    std::fs::write(e.hub.paths.sessions("botler").join(&inst.session_file), lines.join("\n")).unwrap();

    let view = botler_hub::ops::call(&e.hub, &user, "context_advice", json!({"chat": "botler"})).await.unwrap();
    assert_eq!(view["suggested"], true, "{view}");
    let best = view["advice"]["best"].as_u64().unwrap();
    assert!(best < 500_000, "{view}");

    let asked = || async {
        let open = e.hub.store.open_dialogs().await.unwrap();
        open.into_iter().filter(|d| botler_hub::context_advice::chat_of(d) == Some("botler")).collect::<Vec<_>>()
    };
    let sweep =
        |days: i64| botler_hub::context_advice::sweep(&e.hub, chrono::Utc::now() + chrono::Duration::days(days));
    // The size was just set: it gets two weeks of its own calls first.
    sweep(0).await.unwrap();
    assert!(asked().await.is_empty());
    sweep(15).await.unwrap();
    sweep(17).await.unwrap();
    let open = asked().await;
    assert_eq!(open.len(), 1, "asked once: {open:?}");
    assert_eq!(open[0].instance, "botler", "shown on the chat");
    // `keep`: not asked again for a month.
    botler_hub::ops::answer_question(&e.hub, &open[0].id, "keep".into(), None).await.unwrap();
    sweep(25).await.unwrap();
    assert!(asked().await.is_empty());
    assert_eq!(e.hub.store.instance("botler").await.unwrap().compact_at, Some(500_000));
    sweep(50).await.unwrap();
    let open = asked().await;
    let set = format!("set {}k", best / 1000);
    assert!(open[0].ctas.contains(&set), "{:?}", open[0].ctas);
    botler_hub::ops::answer_question(&e.hub, &open[0].id, set, None).await.unwrap();
    assert_eq!(e.hub.store.instance("botler").await.unwrap().compact_at, Some(best));
}

/// A chat's view says how full its context is: the latest call's size against its compact_at,
/// unknown right after a compaction.
#[tokio::test]
async fn a_chats_view_says_how_full_its_context_is() {
    let e = botler_awake().await;
    let inst = e.hub.store.instance("botler").await.unwrap();
    let path = e.hub.paths.sessions("botler").join(&inst.session_file);
    let call = |id: &str, parent: &str, input: u64| {
        json!({"type": "message", "id": id, "parentId": parent, "timestamp": "2026-11-07T10:00:00Z", "message": {
            "role": "assistant", "content": [{"type": "text", "text": "ok"}],
            "usage": {"input": input, "cacheRead": 40_000, "cacheWrite": 0, "output": 500}}})
        .to_string()
    };
    let lines = [call("a", "", 1_000), call("b", "a", 2_000)];
    std::fs::write(&path, lines.join("\n")).unwrap();
    let view = |inst| botler_hub::api::session_view(&e.hub, inst, None, botler_hub::sessions::Window::default());
    let v = view(inst.clone()).await.unwrap();
    assert_eq!(v.context.tokens, Some(42_500));
    assert_eq!(v.context.compact_at, e.hub.applied().await.defs.instance.defaults.compact_at);

    let compaction =
        json!({"type": "compaction", "id": "c", "parentId": "b", "timestamp": "2026-11-07T10:01:00Z", "summary": "s"});
    std::fs::write(&path, [lines[0].clone(), lines[1].clone(), compaction.to_string()].join("\n")).unwrap();
    assert_eq!(view(inst).await.unwrap().context.tokens, None);
}
