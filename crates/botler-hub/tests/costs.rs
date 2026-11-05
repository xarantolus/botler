//! The Costs page: model spend over a span of days, split by the hub's time zone.

mod common;

use botler_hub::costs::{CostsQuery, costs};
use chrono::{NaiveDate, TimeZone, Utc};

#[tokio::test]
async fn spend_per_day_chat_and_model() {
    let e = common::env().await;
    let main = e.hub.repo.main().await.unwrap();
    let toml = e.hub.repo.read_file(&main, "botler.toml").await.unwrap().unwrap();
    e.commit(&[("botler.toml", &toml.replace("# timezone = \"Europe/Berlin\"", "timezone = \"Europe/Berlin\""))]).await;

    let at = |d, h| Utc.with_ymd_and_hms(2026, 11, d, h, 30, 0).unwrap();
    let s = &e.hub.store;
    // 23:30 UTC on the 1st is the 2nd in Berlin.
    s.record_usage("botler", at(1, 9), "m1", 100, 1.0).await.unwrap();
    s.record_usage("botler", at(1, 23), "m1", 50, 0.5).await.unwrap();
    s.record_usage("job:digest", at(2, 9), "m2", 10, 2.0).await.unwrap();
    s.record_usage("botler", at(5, 9), "m1", 1, 9.0).await.unwrap(); // after the span

    let day = |d| NaiveDate::from_ymd_opt(2026, 11, d).unwrap();
    let q = CostsQuery { from: day(1), to: day(3) };
    assert_eq!(q.problem(), None);
    let c = costs(&e.hub, &q).await.unwrap();
    assert_eq!(c.timezone, "Europe/Berlin");
    assert_eq!((c.total.calls, c.total.tokens, c.total.cost_usd), (3, 160, 3.5));
    let days: Vec<_> = c.days.iter().map(|d| (d.day, d.totals.calls, d.totals.cost_usd)).collect();
    assert_eq!(days, [(day(1), 1, 1.0), (day(2), 2, 2.5), (day(3), 0, 0.0)], "every day, empty ones too");
    let chats: Vec<_> =
        c.chats.iter().map(|c| (c.instance.as_str(), c.totals.cost_usd, c.definition.is_some())).collect();
    assert_eq!(chats, [("job:digest", 2.0, false), ("botler", 1.5, true)], "most expensive first");
    let models: Vec<_> = c.models.iter().map(|m| (m.model.as_str(), m.totals.calls)).collect();
    assert_eq!(models, [("m2", 1), ("m1", 2)]);

    assert!(CostsQuery { from: day(3), to: day(1) }.problem().is_some());
    assert!(CostsQuery { from: day(1), to: day(1) }.problem().is_none(), "one day");
}
