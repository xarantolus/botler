//! Model usage per call, as reported by pi. Budgets are checked against these facts.

use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::{Result, Store, ts};

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, ts_rs::TS)]
pub struct UsageTotals {
    pub tokens: i64,
    pub cost_usd: f64,
    pub calls: i64,
}

impl Store {
    pub async fn record_usage(
        &self,
        instance: &str,
        at: DateTime<Utc>,
        model: &str,
        tokens: i64,
        cost_usd: f64,
    ) -> Result<()> {
        sqlx::query("INSERT INTO usage (instance, at, model, tokens, cost_usd) VALUES (?, ?, ?, ?, ?)")
            .bind(instance)
            .bind(ts(at))
            .bind(model)
            .bind(tokens)
            .bind(cost_usd)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Totals since `since`, for one instance or (with `None`) for everything.
    pub async fn usage_since(&self, instance: Option<&str>, since: DateTime<Utc>) -> Result<UsageTotals> {
        const SELECT: &str =
            "SELECT COALESCE(SUM(tokens), 0) AS t, COALESCE(SUM(cost_usd), 0.0) AS c, COUNT(*) AS n FROM usage";
        let r = match instance {
            Some(i) => {
                sqlx::query(sqlx::AssertSqlSafe(format!("{SELECT} WHERE instance = ? AND at >= ?")))
                    .bind(i)
                    .bind(ts(since))
                    .fetch_one(&self.pool)
                    .await?
            }
            None => {
                sqlx::query(sqlx::AssertSqlSafe(format!("{SELECT} WHERE at >= ?")))
                    .bind(ts(since))
                    .fetch_one(&self.pool)
                    .await?
            }
        };
        Ok(UsageTotals { tokens: r.get("t"), cost_usd: r.get("c"), calls: r.get("n") })
    }

    /// Totals since `since` for every instance that used a model, by instance name.
    pub async fn usage_by_instance(
        &self,
        since: DateTime<Utc>,
    ) -> Result<std::collections::BTreeMap<String, UsageTotals>> {
        let rows = sqlx::query(
            "SELECT instance, SUM(tokens) AS t, SUM(cost_usd) AS c, COUNT(*) AS n FROM usage WHERE at >= ? GROUP BY instance",
        )
        .bind(ts(since))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get("instance"), UsageTotals { tokens: r.get("t"), cost_usd: r.get("c"), calls: r.get("n") }))
            .collect())
    }
}
