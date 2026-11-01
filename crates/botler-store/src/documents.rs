//! Documents agents write for the user (Markdown), by name.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store, parse_ts, ts};

#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
pub struct Document {
    /// Its id, e.g. `flat-shortlist`.
    pub name: String,
    pub title: String,
    pub body: String,
    /// The body before the last write.
    pub previous_body: Option<String>,
    /// Who wrote it last: a chat, or `user`.
    pub author: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn row(r: &SqliteRow) -> Result<Document> {
    const T: &str = "documents";
    Ok(Document {
        name: r.get("name"),
        title: r.get("title"),
        body: r.get("body"),
        previous_body: r.get("previous_body"),
        author: r.get("author"),
        created_at: parse_ts(T, r.get("created_at"))?,
        updated_at: parse_ts(T, r.get("updated_at"))?,
    })
}

impl Store {
    /// Create document `name`, or replace its title and body (keeping the old body).
    pub async fn write_document(&self, name: &str, title: &str, body: &str, author: &str) -> Result<Document> {
        let now = ts(Utc::now());
        sqlx::query(
            "INSERT INTO documents (name, title, body, author, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (name) DO UPDATE SET previous_body = documents.body, title = excluded.title,
                 body = excluded.body, author = excluded.author, updated_at = excluded.updated_at",
        )
        .bind(name)
        .bind(title)
        .bind(body)
        .bind(author)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        self.document(name).await?.ok_or_else(|| crate::not_found("document", name))
    }

    pub async fn document(&self, name: &str) -> Result<Option<Document>> {
        let r = sqlx::query("SELECT * FROM documents WHERE name = ?").bind(name).fetch_optional(&self.pool).await?;
        r.as_ref().map(row).transpose()
    }

    /// All documents, most recently updated first.
    pub async fn documents(&self) -> Result<Vec<Document>> {
        let rows = sqlx::query("SELECT * FROM documents ORDER BY updated_at DESC").fetch_all(&self.pool).await?;
        rows.iter().map(row).collect()
    }

    /// Returns whether it existed.
    pub async fn delete_document(&self, name: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM documents WHERE name = ?").bind(name).execute(&self.pool).await?;
        Ok(r.rows_affected() == 1)
    }
}
