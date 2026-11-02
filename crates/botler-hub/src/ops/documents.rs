//! Documents for the user to read (Markdown, by name): what an agent or job writes for the user to
//! come back to, e.g. a shortlist it keeps adding to or a report. The app lists them.

use std::sync::Arc;

use botler_store::Document;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Caller, Op, OpError, Who};
use crate::hub::Hub;

/// Bodies larger than this belong in a file.
const MAX_BODY: usize = 256 * 1024;

/// Where the app shows document `name`.
pub fn document_path(name: &str) -> String {
    format!("/documents/{name}")
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct WriteDocument {
    /// Its id: lowercase words joined by dashes, e.g. `flat-shortlist`. Writing an existing one
    /// replaces it, so read it first to add to it.
    pub name: String,
    pub title: String,
    /// The whole document, in Markdown.
    pub body: String,
}

#[derive(Debug, Serialize, TS)]
pub struct DocumentWritten {
    pub name: String,
    /// Where the user reads it in the app; link it when you mention it.
    pub path: String,
    /// It existed before.
    pub updated: bool,
    /// A trial run: recorded, not carried out.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub dry_run: bool,
}

impl Op for WriteDocument {
    const NAME: &'static str = "write_document";
    const DESCRIPTION: &'static str = "Write a document for the user to read in the app (Documents), in Markdown: a report, a shortlist you keep adding to, a plan. Writing the same name again replaces it, so read_document first to add to it. Prefer this over a memory note or a file when the user is the reader.";
    const WHO: Who = Who::Everyone;
    type Output = DocumentWritten;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<DocumentWritten, OpError> {
        let ok = (2..=60).contains(&self.name.len())
            && botler_core::defs::valid_name(&self.name)
            && !self.name.ends_with('-')
            && !self.name.contains("--");
        if !ok {
            return Err(OpError::BadRequest(
                "name must be 2-60 lowercase letters/digits in dash-separated words".into(),
            ));
        }
        let title = self.title.trim();
        if title.is_empty() || title.len() > 120 {
            return Err(OpError::BadRequest("title must be 1-120 characters".into()));
        }
        if self.body.len() > MAX_BODY {
            return Err(OpError::BadRequest(format!(
                "the body is over {} KiB; keep large data in files",
                MAX_BODY / 1024
            )));
        }
        let path = document_path(&self.name);
        let updated = hub.store.document(&self.name).await?.is_some();
        if let Some(trial) = crate::trials::of_caller(hub, caller).await? {
            let effect = serde_json::json!({ "name": self.name, "title": title, "bytes": self.body.len() });
            crate::trials::record(hub, &trial, Self::NAME, &effect).await?;
            return Ok(DocumentWritten { name: self.name, path, updated, dry_run: true });
        }
        hub.store.write_document(&self.name, title, &self.body, &caller.source().key()).await?;
        Ok(DocumentWritten { name: self.name, path, updated, dry_run: false })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct ReadDocument {
    /// Its name, or its link as the app shows it (`/documents/<name>`, also a full URL).
    pub name: String,
}

/// The document name in `name`, also when it is given as its link (`/documents/<name>`, a full
/// URL with a query or fragment).
fn document_name(name: &str) -> &str {
    let name = name.trim();
    let name = name.rsplit_once("/documents/").map_or(name, |(_, rest)| rest);
    let name = name.strip_prefix("documents/").unwrap_or(name);
    name.split(['?', '#', '/']).next().unwrap_or(name)
}

impl Op for ReadDocument {
    const NAME: &'static str = "read_document";
    const DESCRIPTION: &'static str = "Read a document (its title, Markdown body and who wrote it last), by name or by its link: a `/documents/<name>` link (also as a full URL) is a document, read here, not a file.";
    const WHO: Who = Who::Everyone;
    type Output = Document;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<Document, OpError> {
        let name = document_name(&self.name);
        hub.store.document(name).await?.ok_or_else(|| OpError::BadRequest(format!("no document {name}")))
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct ListDocuments {}

#[derive(Debug, Serialize, TS)]
pub struct DocumentSummary {
    pub name: String,
    pub title: String,
    pub author: String,
    pub bytes: usize,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl Op for ListDocuments {
    const NAME: &'static str = "list_documents";
    const DESCRIPTION: &'static str = "The documents there are, most recently updated first (without their bodies).";
    const WHO: Who = Who::Everyone;
    type Output = Vec<DocumentSummary>;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<Vec<DocumentSummary>, OpError> {
        Ok(hub
            .store
            .documents()
            .await?
            .into_iter()
            .map(|d| DocumentSummary {
                bytes: d.body.len(),
                name: d.name,
                title: d.title,
                author: d.author,
                updated_at: d.updated_at,
            })
            .collect())
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct DeleteDocument {
    pub name: String,
}

#[derive(Debug, Serialize, TS)]
pub struct DocumentDeleted {
    pub deleted: bool,
}

impl Op for DeleteDocument {
    const NAME: &'static str = "delete_document";
    const DESCRIPTION: &'static str = "Delete a document.";
    const WHO: Who = Who::User;
    type Output = DocumentDeleted;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<DocumentDeleted, OpError> {
        Ok(DocumentDeleted { deleted: hub.store.delete_document(&self.name).await? })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_link_names_its_document() {
        for given in [
            "flat-budget",
            " /documents/flat-budget ",
            "documents/flat-budget",
            "http://myhost:7420/documents/flat-budget",
            "http://myhost:7420/documents/flat-budget?x=1#rent",
            "/documents/flat-budget/",
        ] {
            assert_eq!(super::document_name(given), "flat-budget", "{given}");
        }
    }
}
