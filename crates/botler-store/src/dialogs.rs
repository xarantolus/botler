//! Dialogs: questions and approvals waiting on the user.
//!
//! A dialog is answered only while open. Events touching one of its `about` targets abort it, and
//! a late answer is rejected, because the user answered a world state that no longer exists.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Error, Result, Store, new_id, parse_opt_ts, parse_ts, text_enum, ts};

text_enum!(DialogKind {
    Question = "question",
    Approval = "approval",
});

text_enum!(DialogStatus {
    Open = "open",
    Answered = "answered",
    Aborted = "aborted",
    Expired = "expired",
});

text_enum!(Priority {
    /// Work is blocked on the user: push.
    Blocking = "blocking",
    Normal = "normal",
    /// Digest material, never pushed.
    Info = "info",
});

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Dialog {
    pub id: String,
    pub instance: String,
    pub kind: DialogKind,
    pub question: String,
    pub detail: Option<String>,
    pub ctas: Vec<String>,
    pub about: Vec<String>,
    pub priority: Priority,
    pub proposal_id: Option<String>,
    pub status: DialogStatus,
    pub answer: Option<String>,
    pub abort_reason: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct NewDialog {
    pub instance: String,
    pub kind: DialogKind,
    pub question: String,
    pub detail: Option<String>,
    pub ctas: Vec<String>,
    pub about: Vec<String>,
    pub priority: Priority,
    pub proposal_id: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

fn row(r: &SqliteRow) -> Result<Dialog> {
    const T: &str = "dialogs";
    let list = |col: &str| -> Result<Vec<String>> {
        serde_json::from_str(r.get(col)).map_err(|e| Error::Corrupt { table: T, detail: e.to_string() })
    };
    Ok(Dialog {
        id: r.get("id"),
        instance: r.get("instance"),
        kind: DialogKind::parse(T, r.get("kind"))?,
        question: r.get("question"),
        detail: r.get("detail"),
        ctas: list("ctas")?,
        about: list("about")?,
        priority: Priority::parse(T, r.get("priority"))?,
        proposal_id: r.get("proposal_id"),
        status: DialogStatus::parse(T, r.get("status"))?,
        answer: r.get("answer"),
        abort_reason: r.get("abort_reason"),
        expires_at: parse_opt_ts(T, r.get("expires_at"))?,
        created_at: parse_ts(T, r.get("created_at"))?,
        resolved_at: parse_opt_ts(T, r.get("resolved_at"))?,
    })
}

impl Store {
    pub async fn create_dialog(&self, d: &NewDialog) -> Result<Dialog> {
        let id = new_id();
        sqlx::query(
            "INSERT INTO dialogs (id, instance, kind, question, detail, ctas, about, priority, proposal_id, status, expires_at, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'open', ?, ?)",
        )
        .bind(&id)
        .bind(&d.instance)
        .bind(d.kind.as_str())
        .bind(&d.question)
        .bind(&d.detail)
        .bind(crate::encode("dialog ctas", &d.ctas)?)
        .bind(crate::encode("dialog about", &d.about)?)
        .bind(d.priority.as_str())
        .bind(&d.proposal_id)
        .bind(d.expires_at.map(ts))
        .bind(ts(Utc::now()))
        .execute(&self.pool)
        .await?;
        self.dialog(&id).await
    }

    pub async fn dialog(&self, id: &str) -> Result<Dialog> {
        let r = sqlx::query("SELECT * FROM dialogs WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        row(&r.ok_or_else(|| crate::not_found("dialog", id))?)
    }

    pub async fn open_dialogs(&self) -> Result<Vec<Dialog>> {
        let rows = sqlx::query("SELECT * FROM dialogs WHERE status = 'open' ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row).collect()
    }

    pub async fn recent_dialogs(&self, limit: u32) -> Result<Vec<Dialog>> {
        let rows = sqlx::query("SELECT * FROM dialogs ORDER BY created_at DESC LIMIT ?")
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row).collect()
    }

    /// Answer an open dialog. Fails with `Conflict` (and the reason) if it is no longer open.
    pub async fn answer_dialog(&self, id: &str, answer: &str) -> Result<Dialog> {
        let d = self.dialog(id).await?;
        if d.status != DialogStatus::Open {
            return Err(Error::DialogClosed { status: d.status, reason: d.abort_reason });
        }
        if !d.ctas.is_empty() && !d.ctas.iter().any(|c| c == answer) {
            return Err(Error::AnswerNotAllowed { answer: answer.into(), allowed: d.ctas });
        }
        let r = sqlx::query(
            "UPDATE dialogs SET status = 'answered', answer = ?, resolved_at = ? WHERE id = ? AND status = 'open'",
        )
        .bind(answer)
        .bind(ts(Utc::now()))
        .bind(id)
        .execute(&self.pool)
        .await?;
        if r.rows_affected() == 0 {
            // Closed between our read and the update (an abort raced the answer).
            let now = self.dialog(id).await?;
            return Err(Error::DialogClosed { status: now.status, reason: now.abort_reason });
        }
        self.dialog(id).await
    }

    /// Close an open dialog as aborted or expired. Returns false if it was not open.
    pub async fn close_dialog(&self, id: &str, status: DialogStatus, reason: &str) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE dialogs SET status = ?, abort_reason = ?, resolved_at = ? WHERE id = ? AND status = 'open'",
        )
        .bind(status.as_str())
        .bind(reason)
        .bind(ts(Utc::now()))
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }
}
