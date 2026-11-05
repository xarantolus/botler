//! What models cost over a span of days (the Costs page): per day, per chat and per model.

use std::collections::BTreeMap;

use botler_store::UsageTotals;
use chrono::{DateTime, Days, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::hub::{Hub, Result};

/// The longest span asked for at once.
pub const MAX_DAYS: u64 = 366;

/// Days in the hub's time zone, both included.
#[derive(Debug, Deserialize, TS)]
pub struct CostsQuery {
    pub from: NaiveDate,
    pub to: NaiveDate,
}

#[derive(Debug, Serialize, TS)]
pub struct Costs {
    pub timezone: String,
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub total: UsageTotals,
    /// Every day of the span, the ones without calls too.
    pub days: Vec<DayCost>,
    /// The most expensive first. A job's calls through the model gateway are under `job:<name>`.
    pub chats: Vec<ChatCost>,
    pub models: Vec<ModelCost>,
}

#[derive(Debug, Serialize, TS)]
pub struct DayCost {
    pub day: NaiveDate,
    pub totals: UsageTotals,
}

#[derive(Debug, Serialize, TS)]
pub struct ChatCost {
    pub instance: String,
    /// `None` for a job, or a chat that was deleted.
    pub title: Option<String>,
    pub definition: Option<String>,
    pub totals: UsageTotals,
}

#[derive(Debug, Serialize, TS)]
pub struct ModelCost {
    pub model: String,
    pub totals: UsageTotals,
}

fn midnight(tz: Tz, day: NaiveDate) -> DateTime<Utc> {
    let t = day.and_hms_opt(0, 0, 0).unwrap_or_default();
    tz.from_local_datetime(&t).earliest().map_or_else(|| t.and_utc(), |t| t.with_timezone(&Utc))
}

fn add(t: &mut UsageTotals, tokens: i64, cost: f64) {
    t.tokens += tokens;
    t.cost_usd += cost;
    t.calls += 1;
}

impl CostsQuery {
    /// Why the span cannot be shown, if it cannot.
    pub fn problem(&self) -> Option<String> {
        let days = self.to.signed_duration_since(self.from).num_days() + 1;
        (days < 1 || days > MAX_DAYS as i64)
            .then(|| format!("from {} to {}: 1 to {MAX_DAYS} days, the first one first", self.from, self.to))
    }
}

/// The costs of `q`'s days (see [`CostsQuery::problem`]).
pub async fn costs(hub: &Hub, q: &CostsQuery) -> Result<Costs> {
    let tz = hub.applied().await.defs.timezone;
    let end = q.to.checked_add_days(Days::new(1)).unwrap_or(q.to);
    let rows = hub.store.usage_between(midnight(tz, q.from), midnight(tz, end)).await?;

    let mut total = UsageTotals::default();
    let mut days: BTreeMap<NaiveDate, UsageTotals> =
        q.from.iter_days().take_while(|d| *d <= q.to).map(|d| (d, UsageTotals::default())).collect();
    let mut chats: BTreeMap<String, UsageTotals> = BTreeMap::new();
    let mut models: BTreeMap<String, UsageTotals> = BTreeMap::new();
    for r in rows {
        add(&mut total, r.tokens, r.cost_usd);
        add(days.entry(r.at.with_timezone(&tz).date_naive()).or_default(), r.tokens, r.cost_usd);
        add(chats.entry(r.instance).or_default(), r.tokens, r.cost_usd);
        add(models.entry(r.model).or_default(), r.tokens, r.cost_usd);
    }

    let mut chat_costs = Vec::with_capacity(chats.len());
    for (instance, totals) in chats {
        let found = hub.store.find_instance(&instance).await?;
        chat_costs.push(ChatCost {
            title: found.as_ref().and_then(|i| i.title.clone()),
            definition: found.map(|i| i.definition),
            instance,
            totals,
        });
    }
    chat_costs.sort_by(|a, b| b.totals.cost_usd.total_cmp(&a.totals.cost_usd));
    let mut model_costs: Vec<ModelCost> =
        models.into_iter().map(|(model, totals)| ModelCost { model, totals }).collect();
    model_costs.sort_by(|a, b| b.totals.cost_usd.total_cmp(&a.totals.cost_usd));
    Ok(Costs {
        timezone: tz.name().into(),
        from: q.from,
        to: q.to,
        total,
        days: days.into_iter().map(|(day, totals)| DayCost { day, totals }).collect(),
        chats: chat_costs,
        models: model_costs,
    })
}
