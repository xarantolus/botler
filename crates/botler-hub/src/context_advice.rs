//! What context size suits a chat, from its own calls. Every model call resends the context, so a
//! large one costs on every call; a small one compacts often, and each compaction costs a summary
//! call, a cold cache for the next call and the work the chat repeats because it forgot (the
//! apartment curator read the same listing photos again). Which wins depends on the chat: how fast
//! its context grows, how long its turns are, what it repeats. The hub replays the chat's calls of
//! the last week under each candidate size, with their real growth and prices, and suggests the
//! cheapest when it beats the current size clearly (`[defaults] suggest_context_savings`).
//!
//! Sizes do not flip back and forth: the suggestion is the size nearest the current one among
//! those within 5% of the cheapest (neighbouring sizes differ by about that much, which is noise),
//! it must save the configured share (20%), a chat whose size changed is left alone for two weeks
//! so its new size gets calls of its own, and one the user answered `keep` for a month.

use std::sync::Arc;

use botler_store::{Dialog, DialogKind, NewDialog, Priority};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::hub::{Hub, Result};

/// Candidate sizes, in tokens.
const SIZES: [u64; 9] = [40_000, 60_000, 80_000, 100_000, 150_000, 200_000, 300_000, 500_000, 800_000];
/// Too few calls say nothing about a chat.
const MIN_CALLS: usize = 100;
/// Sizes within this share of the cheapest count as the cheapest: the nearest to the current wins.
const NOISE: f64 = 1.05;
/// kv: chat → when it is looked at next (RFC 3339).
const CHECKED: &str = "hub:context-advice";
/// kv: chat → when its context size last changed.
const CHANGED: &str = "hub:context-changed";
const ABOUT: &str = "context:";
/// Output tokens of a summary call (the summary, ~1,500 words, and its thinking).
const SUMMARY_OUTPUT: f64 = 4_000.0;

/// One candidate size and what the chat's last week would have cost with it.
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
pub struct SizeCost {
    pub compact_at: u64,
    pub cost_usd: f64,
    pub compactions: u32,
}

/// A chat's context sizes, priced on its own recent calls.
#[derive(Clone, Debug, Serialize, ts_rs::TS)]
pub struct ContextAdvice {
    pub current: u64,
    /// The size to suggest: the cheapest, or one within 5% of it nearer the current size.
    pub best: u64,
    /// The calls replayed, and over how many days.
    pub calls: u32,
    pub days: f64,
    /// Tool calls the chat repeated right after a compaction, on average.
    pub repeated_after_compaction: f64,
    pub sizes: Vec<SizeCost>,
}

impl ContextAdvice {
    fn cost(&self, size: u64) -> Option<f64> {
        self.sizes.iter().find(|s| s.compact_at == size).map(|s| s.cost_usd)
    }
    /// The current size's cost, simulated like the others (it may not be a candidate).
    fn current_cost(&self) -> Option<f64> {
        self.cost(self.current)
    }
}

struct Call {
    at: DateTime<Utc>,
    segment: usize,
    turn: usize,
    prompt: f64,
    reasoning: f64,
    cost_out: f64,
    tools: Vec<String>,
    /// How much the context grew after this call (the next call's prompt minus this one's).
    growth: f64,
}

struct History {
    calls: Vec<Call>,
    /// Index of the first call after each compaction, and the summary's size in tokens.
    compactions: Vec<(usize, f64)>,
    /// $ per token: input, cache read.
    price_in: f64,
    price_cached: f64,
    price_out: f64,
}

fn num(v: &Value, path: &str) -> f64 {
    v.pointer(path).and_then(Value::as_f64).unwrap_or(0.0)
}

fn read(session: &str) -> History {
    let (mut calls, mut compactions) = (Vec::<Call>::new(), Vec::new());
    let (mut segment, mut turn) = (0, 0);
    let (mut tin, mut cin, mut tcr, mut ccr, mut tout, mut cout) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for line in session.lines() {
        let Ok(e) = serde_json::from_str::<Value>(line) else { continue };
        if e["type"] == "compaction" {
            let summary = e["summary"].as_str().map_or(0, str::len) as f64 / 4.0;
            compactions.push((calls.len(), summary));
            segment += 1;
            continue;
        }
        let m = &e["message"];
        if m["role"] == "user" {
            turn += 1;
        }
        let Some(at) = e["timestamp"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()) else { continue };
        if m["role"] != "assistant" || m["usage"].is_null() {
            continue;
        }
        let u = &m["usage"];
        let prompt = num(u, "/input") + num(u, "/cacheRead") + num(u, "/cacheWrite");
        tin += num(u, "/input");
        cin += num(u, "/cost/input");
        tcr += num(u, "/cacheRead");
        ccr += num(u, "/cost/cacheRead");
        tout += num(u, "/output");
        cout += num(u, "/cost/output");
        let tools = m["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["type"] == "toolCall")
            .map(|p| format!("{}{}", p["name"], p["arguments"]))
            .collect();
        calls.push(Call {
            at: at.with_timezone(&Utc),
            segment,
            turn,
            prompt,
            reasoning: num(u, "/reasoning"),
            cost_out: num(u, "/cost/output"),
            tools,
            growth: f64::NAN,
        });
    }
    // Growth is measured where two calls follow each other in one segment; elsewhere (a
    // compaction, a failed call) it is the median.
    let measured: Vec<f64> = calls
        .windows(2)
        .filter(|w| w[0].segment == w[1].segment && w[0].prompt > 0.0 && w[1].prompt >= w[0].prompt)
        .map(|w| w[1].prompt - w[0].prompt)
        .collect();
    let median = median(&measured);
    for i in 0..calls.len() {
        let next = calls.get(i + 1).filter(|n| n.segment == calls[i].segment && calls[i].prompt > 0.0);
        calls[i].growth = next.map(|n| n.prompt - calls[i].prompt).filter(|g| *g >= 0.0).unwrap_or(median);
    }
    let per = |c: f64, t: f64| if t > 0.0 { c / t } else { 0.0 };
    History { calls, compactions, price_in: per(cin, tin), price_cached: per(ccr, tcr), price_out: per(cout, tout) }
}

fn median(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

/// After each compaction, the calls among the next 40 that repeat a tool call from before it, and
/// how much they added to the context. Averaged over the latest 20 compactions.
fn repeated(h: &History) -> (f64, f64) {
    let recent: Vec<(f64, f64)> = h.compactions[h.compactions.len().saturating_sub(20)..]
        .iter()
        .map(|&(i, _)| {
            let before: std::collections::HashSet<&String> =
                h.calls[i.saturating_sub(200)..i].iter().flat_map(|c| &c.tools).collect();
            let again: Vec<&Call> = h.calls[i..(i + 40).min(h.calls.len())]
                .iter()
                .filter(|c| c.tools.iter().any(|t| before.contains(t)))
                .collect();
            (again.len() as f64, again.iter().map(|c| c.growth).sum())
        })
        .collect();
    if recent.is_empty() {
        return (0.0, 0.0);
    }
    let n = recent.len() as f64;
    (recent.iter().map(|r| r.0).sum::<f64>() / n, recent.iter().map(|r| r.1).sum::<f64>() / n)
}

/// What `calls` would have cost compacting at `size`: each call resends the context (cached,
/// except right after a compaction); earlier turns' thinking leaves it (the extension drops it);
/// a compaction costs a summary call, keeps the summary and the latest sixth, and is followed by
/// the chat's usual repeated calls.
fn simulate(h: &History, calls: &[Call], size: u64, base: f64, summary: f64, again: (f64, f64)) -> SizeCost {
    let size = size as f64;
    let out_per_call = calls.iter().map(|c| c.cost_out).sum::<f64>() / calls.len().max(1) as f64;
    let (mut context, mut cost, mut uncached, mut compactions) = (base, 0.0, 0.0, 0);
    let (mut turn, mut turn_thinking) = (None, 0.0);
    for c in calls {
        if turn != Some(c.turn) {
            context -= turn_thinking;
            turn_thinking = 0.0;
            turn = Some(c.turn);
        }
        cost += uncached * h.price_in + (context - uncached) * h.price_cached + c.cost_out;
        uncached = 0.0;
        context += c.growth;
        turn_thinking += c.reasoning;
        if context > size {
            compactions += 1;
            cost += context * h.price_cached + SUMMARY_OUTPUT * h.price_out;
            context = base + summary + size / 6.0;
            turn_thinking = 0.0;
            uncached = context;
            let (n, tokens) = again;
            for _ in 0..n.round() as u32 {
                cost += context * h.price_cached + out_per_call;
                context += tokens / n;
            }
        }
    }
    SizeCost { compact_at: size as u64, cost_usd: cost, compactions }
}

/// Price the candidate sizes (and `current`) on the session's calls since `since`; `window` is the
/// model's context window, which no size may reach. `None` with too few calls.
pub fn analyse(session: &str, since: DateTime<Utc>, current: u64, window: Option<u64>) -> Option<ContextAdvice> {
    let h = read(session);
    let first = h.calls.iter().position(|c| c.at >= since)?;
    let calls = &h.calls[first..];
    if calls.len() < MIN_CALLS {
        return None;
    }
    // The system prompt and tools: about the smallest prompt the chat sent.
    let mut prompts: Vec<f64> = calls.iter().map(|c| c.prompt).filter(|p| *p > 0.0).collect();
    prompts.sort_by(f64::total_cmp);
    let base = prompts.get(prompts.len() / 50).copied()?;
    let summaries: Vec<f64> = h.compactions.iter().rev().take(10).map(|c| c.1).collect();
    let summary = if summaries.is_empty() { 3_000.0 } else { median(&summaries) };
    let again = repeated(&h);
    let fits = |s: &u64| window.is_none_or(|w| *s + 16_000 <= w) && *s as f64 > base + summary;
    let mut candidates: Vec<u64> = SIZES.iter().copied().filter(fits).collect();
    if !candidates.contains(&current) && current > 0 {
        candidates.push(current);
        candidates.sort_unstable();
    }
    let sizes: Vec<SizeCost> = candidates.into_iter().map(|s| simulate(&h, calls, s, base, summary, again)).collect();
    let cheapest = sizes.iter().map(|s| s.cost_usd).min_by(f64::total_cmp)?;
    let distance = |s: &SizeCost| (s.compact_at as f64 / current.max(1) as f64).ln().abs();
    let best =
        sizes.iter().filter(|s| s.cost_usd <= cheapest * NOISE).min_by(|a, b| distance(a).total_cmp(&distance(b)))?;
    let days = (calls[calls.len() - 1].at - calls[0].at).num_minutes() as f64 / (24.0 * 60.0);
    Some(ContextAdvice {
        current,
        best: best.compact_at,
        calls: calls.len() as u32,
        days,
        repeated_after_compaction: again.0,
        sizes,
    })
}

/// `chat`'s advice from its current session's last week.
pub async fn for_chat(hub: &Hub, chat: &str) -> Result<Option<ContextAdvice>> {
    let instance = hub.store.instance(chat).await?;
    let applied = hub.applied().await;
    let Some(def) = applied.defs.agents.get(&instance.definition) else { return Ok(None) };
    let session = match tokio::fs::read_to_string(hub.paths.sessions(chat).join(&instance.session_file)).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let window = match applied.defs.model_for(&def.spec) {
        Some(route) => crate::model_catalog::known(hub, &route).await.and_then(|k| k.context_length),
        None => None,
    };
    let current = crate::agent_env::compact_at(&instance, def, &applied);
    // Sessions run to tens of MB: parsed off the async threads.
    let since = Utc::now() - Duration::days(7);
    tokio::task::spawn_blocking(move || analyse(&session, since, current, window))
        .await
        .map_err(|e| crate::hub::HubError::Io(std::io::Error::other(e)))
}

/// The suggestion an advice makes, if the best size saves at least `savings` percent.
pub fn suggestion(a: &ContextAdvice, savings: u32) -> Option<(f64, f64)> {
    let (now, best) = (a.current_cost()?, a.cost(a.best)?);
    let worth = 1.0 - f64::from(savings) / 100.0;
    (savings > 0 && a.best != a.current && best <= now * worth).then_some((now, best))
}

/// `chat`'s context size changed (by the user or an answered suggestion): no suggestion until its
/// new size has two weeks of calls.
pub async fn changed(hub: &Hub, chat: &str) -> Result<()> {
    hub.store.kv_set(CHANGED, chat, &Utc::now().to_rfc3339()).await?;
    Ok(())
}

fn at(map: &std::collections::HashMap<String, String>, chat: &str) -> Option<DateTime<Utc>> {
    map.get(chat).and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map(|t| t.with_timezone(&Utc))
}

/// Once a day, look at each chat that ran enough calls; suggest a size where one would have cost
/// clearly less, on the chat. A chat is asked at most once a week.
pub async fn sweep(hub: &Arc<Hub>, now: DateTime<Utc>) -> Result<()> {
    let savings = hub.applied().await.defs.instance.defaults.suggest_context_savings;
    if savings == 0 {
        return Ok(());
    }
    let checked: std::collections::HashMap<String, String> = hub.store.kv_list(CHECKED).await?.into_iter().collect();
    let changed: std::collections::HashMap<String, String> = hub.store.kv_list(CHANGED).await?.into_iter().collect();
    let open: Vec<Dialog> = hub.store.open_dialogs().await?;
    for instance in hub.store.live_instances().await? {
        if at(&checked, &instance.name).is_some_and(|t| now < t)
            || at(&changed, &instance.name).is_some_and(|t| now - t < Duration::days(14))
            || open.iter().any(|d| chat_of(d) == Some(instance.name.as_str()))
        {
            continue;
        }
        hub.store.kv_set(CHECKED, &instance.name, &(now + Duration::days(1)).to_rfc3339()).await?;
        let Some(advice) = for_chat(hub, &instance.name).await? else { continue };
        let Some((now_cost, best_cost)) = suggestion(&advice, savings) else { continue };
        let k = |t: u64| format!("{}k", t / 1000);
        let rows: Vec<String> = advice
            .sizes
            .iter()
            .map(|s| {
                let mark = if s.compact_at == advice.current { " (now)" } else { "" };
                format!("| {}{mark} | ${:.2} | {} |", k(s.compact_at), s.cost_usd, s.compactions)
            })
            .collect();
        let d = hub
            .store
            .create_dialog(&NewDialog {
                // On the chat itself: it is about that chat.
                instance: instance.name.clone(),
                kind: DialogKind::Question,
                question: format!(
                    "{} would cost about {:.0}% less with a context size of {}",
                    instance.title.as_deref().unwrap_or(&instance.name),
                    (1.0 - best_cost / now_cost) * 100.0,
                    k(advice.best)
                ),
                detail: Some(format!(
                    "Its {} model calls of the last {:.0} days, replayed at each size (every call resends the context; \
                     each compaction costs a summary, a cold cache and the {:.1} tool calls it usually repeats after one):\n\n\
                     | Context size | Cost | Compactions |\n|---|---|---|\n{}\n\n\
                     Answer `set {}` to change it (the chat menu's Context size… changes it any time), or `keep`.",
                    advice.calls,
                    advice.days.max(1.0),
                    advice.repeated_after_compaction,
                    rows.join("\n"),
                    k(advice.best),
                )),
                ctas: vec![format!("set {}", k(advice.best)), "keep".into()],
                about: vec![format!("{ABOUT}{}", instance.name)],
                priority: Priority::Normal,
                proposal_id: None,
                spec: None,
                expires_at: Some(now + Duration::days(3)),
            })
            .await?;
        // Not asked again for a week, whatever the answer (`keep`: a month).
        hub.store.kv_set(CHECKED, &instance.name, &(now + Duration::days(7)).to_rfc3339()).await?;
        crate::notify::dialog_opened(hub, &d).await;
    }
    Ok(())
}

/// The chat a context-size request is about.
pub fn chat_of(d: &Dialog) -> Option<&str> {
    d.about.iter().find_map(|a| a.strip_prefix(ABOUT))
}

/// The user answered a context-size request: `set <N>k` applies it, anything else keeps the size
/// and the chat is not asked again for a month.
pub async fn answered(hub: &Hub, d: &Dialog, answer: &str) -> Result<()> {
    let Some(chat) = chat_of(d) else { return Ok(()) };
    let Some(k) = answer.trim().strip_prefix("set ").and_then(|s| s.trim().trim_end_matches('k').parse::<u64>().ok())
    else {
        let later = Utc::now() + Duration::days(30);
        hub.store.kv_set(CHECKED, chat, &later.to_rfc3339()).await?;
        return Ok(());
    };
    hub.store.set_compact_at(chat, Some(k * 1000)).await?;
    changed(hub, chat).await?;
    crate::agents::write_pi_settings(hub, chat).await?;
    hub.dispatch_notify.notify_one();
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A session of `calls` model calls, one user turn per `per_turn` calls, its context growing
    /// by `growth` tokens a call from a 20k system prompt, compacted at `compact_at`; after each
    /// compaction the chat repeats `repeats` tool calls from before it.
    pub fn session(calls: usize, per_turn: usize, growth: u64, compact_at: u64, repeats: usize) -> String {
        let start = Utc::now() - Duration::days(2);
        let (mut lines, mut context, mut since_compaction) = (Vec::new(), 20_000u64, usize::MAX);
        for i in 0..calls {
            let at = (start + Duration::minutes(i as i64)).to_rfc3339();
            if i % per_turn == 0 {
                lines.push(serde_json::json!({"type": "message", "timestamp": at, "message": {"role": "user", "content": "go"}}));
            }
            let tool = if since_compaction < repeats { format!("old{since_compaction}") } else { format!("new{i}") };
            since_compaction = since_compaction.saturating_add(1);
            lines.push(serde_json::json!({"type": "message", "timestamp": at, "message": {
                "role": "assistant",
                "content": [{"type": "toolCall", "name": "read", "arguments": {"path": tool}}],
                "usage": {"input": 500, "cacheRead": context - 500, "output": 300, "reasoning": 200,
                          "cost": {"input": 500.0 * 5e-8, "cacheRead": (context - 500) as f64 * 1.5e-8, "output": 300.0 * 5e-7}},
            }}));
            context += growth;
            if context > compact_at {
                lines.push(serde_json::json!({"type": "compaction", "timestamp": at, "summary": "s".repeat(12_000)}));
                for r in 0..repeats {
                    lines.push(serde_json::json!({"type": "message", "timestamp": at, "message": {"role": "assistant",
                        "content": [{"type": "toolCall", "name": "read", "arguments": {"path": format!("old{r}")}}]}}));
                }
                context = 20_000 + 3_000 + compact_at / 6;
                since_compaction = 0;
            }
        }
        lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n")
    }

    fn advise(s: &str, current: u64) -> ContextAdvice {
        analyse(s, Utc::now() - Duration::days(7), current, Some(1_000_000)).unwrap()
    }

    #[test]
    fn a_chat_that_repeats_nothing_is_cheapest_small() {
        let a = advise(&session(600, 3, 1_500, 500_000, 0), 500_000);
        assert!(a.best <= 100_000, "{a:?}");
        let (now, best) = suggestion(&a, 20).unwrap();
        assert!(best < now * 0.6, "{now} {best}");
    }

    #[test]
    fn a_chat_that_redoes_work_after_compacting_is_cheapest_larger() {
        let cheap = advise(&session(600, 3, 1_500, 60_000, 0), 60_000).best;
        let redo = advise(&session(600, 3, 1_500, 60_000, 12), 60_000);
        assert!(redo.repeated_after_compaction > 5.0, "{redo:?}");
        assert!(redo.best > cheap, "{} vs {cheap}", redo.best);
    }

    #[test]
    fn too_few_calls_say_nothing_and_no_size_reaches_the_window() {
        assert!(analyse(&session(50, 3, 1_500, 60_000, 0), Utc::now() - Duration::days(7), 60_000, None).is_none());
        let a =
            analyse(&session(300, 3, 1_500, 60_000, 0), Utc::now() - Duration::days(7), 60_000, Some(128_000)).unwrap();
        assert!(a.sizes.iter().all(|s| s.compact_at <= 112_000), "{a:?}");
    }
}
