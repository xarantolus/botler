//! Loop defences between agents, end to end with the fake runtime: pair breaker, acknowledgement
//! notes, and end-of-turn notices that do not double-wake the parent.

mod common;

use botler_core::event::kinds;
use botler_core::payloads::{BreakerKind, BreakerTrippedPayload};
use botler_core::{Event, FRONT_DOOR, Provenance, Source};
use botler_hub::auth::Principal;
use botler_hub::ops::{self, Caller, OpError};
use botler_hub::{agents, loops, router};
use botler_store::{EventQuery, InstanceState};
use chrono::Utc;
use serde_json::json;

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

async fn env() -> common::Env {
    let e = common::env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-test").await.unwrap();
    e
}

async fn pump(e: &common::Env) {
    for _ in 0..3 {
        e.route().await;
        agents::dispatch_pending(&e.hub).await.unwrap();
        settle().await;
    }
}

async fn user_says(e: &common::Env, to: &str, text: &str) {
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to(to.to_string())
        .payload(json!({"text": text}));
    e.hub.emit(ev).await.unwrap();
    pump(e).await;
}

/// A chat that is awake, and its caller identity for hub operations.
async fn awake_chat(e: &common::Env) -> (String, Caller) {
    let name = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    user_says(e, &name, "hello").await;
    (name.clone(), caller(e, &name).await)
}

async fn caller(e: &common::Env, name: &str) -> Caller {
    let wake = e.hub.agents.get(name).await.expect("awake").wake.clone();
    Caller::resolve(&e.hub, Principal::Agent { instance: name.into(), wake }).await.unwrap()
}

async fn send(e: &common::Env, from: &Caller, to: &str, text: &str) {
    ops::call(&e.hub, from, "send_message", json!({"to": to, "text": text})).await.unwrap();
    e.route().await;
}

fn prompts(e: &common::Env, name: &str) -> Vec<String> {
    e.fake.prompts(&agents::container_name(&e.hub, name))
}

fn notes(e: &common::Env, name: &str) -> Vec<String> {
    e.fake.notes(&agents::container_name(&e.hub, name))
}

#[tokio::test]
async fn ping_pong_between_two_agents_trips_the_pair_breaker_until_resumed() {
    let e = env().await;
    let (a, ca) = awake_chat(&e).await;
    let (b, cb) = awake_chat(&e).await;
    // Eight exchanges are fine; the ninth without the user in between is held.
    for i in 0..9 {
        let (from, to) = if i % 2 == 0 { (&ca, &b) } else { (&cb, &a) };
        send(&e, from, to, &format!("round {i}: here is my next idea for the plan")).await;
    }
    let subject = botler_core::loops::pair_subject(&a, &b);
    let breaker = e.hub.store.breaker(&subject).await.unwrap().expect("pair breaker tripped");
    assert_eq!(breaker.kind, "pair");
    let pending_a = e.hub.store.pending_deliveries(&a).await.unwrap();
    assert!(!pending_a.iter().any(|d| d.event_id.is_empty()), "nothing odd queued");

    // The user is told, with the pair and what they said.
    let tripped = e
        .hub
        .store
        .events(&EventQuery { kind: Some(kinds::BREAKER_TRIPPED.into()), limit: 5, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(tripped.len(), 1);
    assert_eq!(tripped[0].to.as_deref(), Some(FRONT_DOOR));
    let p: BreakerTrippedPayload = serde_json::from_value(tripped[0].payload.clone()).unwrap();
    assert_eq!(p.breaker, BreakerKind::Pair);
    assert!(p.pair.as_ref().unwrap().contains(&a) && p.pair.as_ref().unwrap().contains(&b));
    assert!(p.recent.last().unwrap().text.starts_with("round 8"), "{:?}", p.recent);

    // More messages on the paused pair are held, not delivered.
    send(&e, &ca, &b, "round 9: are you still there").await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
    assert!(!prompts(&e, &b).iter().any(|t| t.contains("round 9")));

    // Resuming releases what was held and starts the count again.
    let (cleared, released) = loops::clear_breaker(&e.hub, &subject).await.unwrap();
    assert!(cleared);
    assert_eq!(released, 2, "round 8 and round 9");
    pump(&e).await;
    assert!(prompts(&e, &b).iter().any(|t| t.contains("round 9")));
    send(&e, &cb, &a, "round 10: fresh start").await;
    assert!(e.hub.store.breaker(&subject).await.unwrap().is_none());
}

#[tokio::test]
async fn a_user_message_resets_the_pair_count() {
    let e = env().await;
    let (a, ca) = awake_chat(&e).await;
    let (b, cb) = awake_chat(&e).await;
    for i in 0..8 {
        let (from, to) = if i % 2 == 0 { (&ca, &b) } else { (&cb, &a) };
        send(&e, from, to, &format!("round {i}: more details on the design")).await;
    }
    user_says(&e, &b, "carry on, you two").await;
    for i in 8..16 {
        let (from, to) = if i % 2 == 0 { (&ca, &b) } else { (&cb, &a) };
        send(&e, from, to, &format!("round {i}: more details on the design")).await;
    }
    assert!(e.hub.store.breakers().await.unwrap().is_empty());
}

#[tokio::test]
async fn acknowledgements_do_not_wake_and_ride_along_with_the_next_message() {
    let e = env().await;
    let (a, ca) = awake_chat(&e).await;
    let (b, _) = awake_chat(&e).await;
    let woken = prompts(&e, &b).len();

    // Awake recipient: the note is added to its session without a turn.
    send(&e, &ca, &b, "Thanks, no reply needed!").await;
    pump(&e).await;
    assert_eq!(prompts(&e, &b).len(), woken, "no new turn");
    assert!(notes(&e, &b).last().unwrap().contains("no reply needed"));

    // Sleeping recipient: the note waits; nothing starts the container.
    agents::stop_all(&e.hub).await;
    settle().await;
    assert_eq!(e.hub.store.instance(&b).await.unwrap().state, InstanceState::Dormant);
    let ca = {
        user_says(&e, &a, "wake up").await;
        caller(&e, &a).await
    };
    send(&e, &ca, &b, "👍 got it").await;
    pump(&e).await;
    assert_eq!(e.hub.store.instance(&b).await.unwrap().state, InstanceState::Dormant, "an ack never wakes");
    assert_eq!(e.hub.store.pending_deliveries(&b).await.unwrap().len(), 1, "kept for later");

    // The next real message wakes it, and the note arrives first.
    send(&e, &ca, &b, "ok, but what about the failing test?").await;
    pump(&e).await;
    assert_eq!(e.hub.store.instance(&b).await.unwrap().state, InstanceState::Running);
    assert!(notes(&e, &b).last().unwrap().contains("got it"));
    assert!(prompts(&e, &b).last().unwrap().contains("failing test"));
}

#[tokio::test]
async fn a_short_answer_to_a_question_wakes() {
    let e = env().await;
    let (a, ca) = awake_chat(&e).await;
    let (b, cb) = awake_chat(&e).await;
    send(&e, &cb, &a, "Shall I merge the branch now?").await;
    pump(&e).await;
    let before = prompts(&e, &b).len();
    send(&e, &ca, &b, "ok").await;
    pump(&e).await;
    assert_eq!(prompts(&e, &b).len(), before + 1, "\"ok\" answers the question: it wakes");
}

#[tokio::test]
async fn messaging_yourself_is_refused() {
    let e = env().await;
    let (a, ca) = awake_chat(&e).await;
    match ops::call(&e.hub, &ca, "send_message", json!({"to": a, "text": "note to self"})).await {
        Err(OpError::BadRequest(m)) => assert!(m.contains("set_timer"), "{m}"),
        other => panic!("{other:?}"),
    }
}

async fn idle_notices(e: &common::Env) -> usize {
    e.hub
        .store
        .events(&EventQuery { kind: Some(kinds::AGENT_IDLE.into()), limit: 50, ..Default::default() })
        .await
        .unwrap()
        .len()
}

#[tokio::test]
async fn a_turn_ending_with_a_message_to_the_parent_sends_no_notice() {
    let e = env().await;
    let (parent, _) = awake_chat(&e).await;
    e.fake.set_hold_turns(true);
    let child = router::spawn(&e.hub, "dev", Some(&parent), Some("build it"), None, None).await.unwrap();
    pump(&e).await;
    let container = agents::container_name(&e.hub, &child);
    let me = caller(&e, &child).await;

    // Work, then report to the parent as the last thing: the report is the only message.
    assert!(e.fake.pi_event(&container, json!({"type": "tool_execution_start", "toolName": "bash"})));
    settle().await;
    send(&e, &me, &parent, "Done: the parser is on agents/x/work, tests pass.").await;
    assert!(
        e.fake.pi_event(&container, json!({"type": "tool_execution_start", "toolName": "mcp__botler__send_message"}))
    );
    assert!(e.fake.pi_event(&container, json!({"type": "agent_settled"})));
    settle().await;
    e.route().await;
    assert_eq!(idle_notices(&e).await, 0, "the parent already has the news");

    // Next turn: report first, then more work. The end of the turn is news again.
    user_says(&e, &child, "also add docs").await;
    let me = caller(&e, &child).await;
    send(&e, &me, &parent, "Starting on the docs.").await;
    assert!(e.fake.pi_event(&container, json!({"type": "tool_execution_start", "toolName": "write"})));
    settle().await;
    e.fake.set_last_reply(&container, "Docs written in docs/parser.md.");
    assert!(e.fake.pi_event(&container, json!({"type": "agent_settled"})));
    settle().await;
    e.route().await;
    let idle = e
        .hub
        .store
        .events(&EventQuery { kind: Some(kinds::AGENT_IDLE.into()), limit: 5, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(idle.len(), 1);
    assert_eq!(idle[0].to.as_deref(), Some(parent.as_str()));
    assert_eq!(idle[0].payload["last_reply"], "Docs written in docs/parser.md.");
}

#[tokio::test]
async fn parent_and_child_acknowledgements_end_the_conversation() {
    let e = env().await;
    let (parent, cp) = awake_chat(&e).await;
    let child = router::spawn(&e.hub, "dev", Some(&parent), Some("build it"), None, None).await.unwrap();
    let child_c = agents::container_name(&e.hub, &child);
    e.fake.set_last_reply(&child_c, "Done: built the parser, tests pass.");
    pump(&e).await;
    // The child's end-of-turn report woke the parent.
    assert!(prompts(&e, &parent).last().unwrap().contains("built the parser"));
    let parent_turns = prompts(&e, &parent).len();
    let child_turns = prompts(&e, &child).len();

    // The parent says thanks: a note for the child, no new turn.
    send(&e, &cp, &child, "Thanks, no reply needed.").await;
    pump(&e).await;
    assert_eq!(prompts(&e, &child).len(), child_turns);
    assert!(notes(&e, &child).last().unwrap().contains("no reply needed"));

    // Even if the child is woken and only acknowledges, its notice is a note for the parent.
    e.fake.set_last_reply(&child_c, "Acknowledged, thanks!");
    user_says(&e, &child, "the user says hi").await;
    pump(&e).await;
    assert_eq!(prompts(&e, &parent).len(), parent_turns, "the parent is not woken by an ack");
    assert!(notes(&e, &parent).last().unwrap().contains("Acknowledged"));
    // Nothing else is moving: the conversation is over.
    let (p, c) = (prompts(&e, &parent).len(), prompts(&e, &child).len());
    pump(&e).await;
    assert_eq!((prompts(&e, &parent).len(), prompts(&e, &child).len()), (p, c));
    assert!(e.hub.store.breakers().await.unwrap().is_empty());
}
