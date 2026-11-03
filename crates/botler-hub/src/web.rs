//! The web app: the built frontend (`frontend/dist`, built by build.rs and compiled into the binary) at
//! `/`, with a fallback to `index.html` for the app's own routes, and the old single-page UI at
//! `/legacy`.

use axum::Json;
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use include_dir::{Dir, include_dir};

use crate::wire::ApiErrorBody;

static DIST: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../frontend/dist");

/// Paths owned by the API; unknown ones are a 404, never the app.
const API_PREFIXES: &[&str] = &["/api/", "/mcp", "/git/", "/healthz", "/hooks/"];

/// Hash of the frontend sources `dist` was built from (see `frontend/scripts/source-hash.mjs`).
pub fn built_from() -> Option<&'static str> {
    DIST.get_file(".source-hash").and_then(|f| f.contents_utf8()).map(str::trim)
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("json" | "map") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn not_found(what: String) -> Response {
    (StatusCode::NOT_FOUND, Json(ApiErrorBody { error: what, conflict: None })).into_response()
}

fn file(path: &str, body: &'static [u8]) -> Response {
    // Vite puts content-hashed files under assets/: they never change.
    let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type(path))),
            (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
        ],
        body,
    )
        .into_response()
}

/// Fallback for everything the API does not route.
pub async fn app(method: Method, uri: Uri) -> Response {
    let path = uri.path();
    if API_PREFIXES.iter().any(|p| path.starts_with(p) || path == p.trim_end_matches('/')) {
        return not_found(format!("no route {method} {path}"));
    }
    if method != Method::GET && method != Method::HEAD {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(ApiErrorBody { error: format!("{method} {path}"), conflict: None }),
        )
            .into_response();
    }
    let rel = path.trim_start_matches('/');
    let hidden = rel.split('/').any(|seg| seg.starts_with('.'));
    if !rel.is_empty()
        && !hidden
        && let Some(f) = DIST.get_file(rel)
    {
        return file(rel, f.contents());
    }
    // A missing file (it has an extension) is a 404; anything else is a route of the app.
    if rel.rsplit('/').next().is_some_and(|last| last.contains('.')) {
        return not_found(format!("no file {path}"));
    }
    match DIST.get_file("index.html") {
        Some(f) => file("index.html", f.contents()),
        None => not_found("the web app was not built into this binary (frontend/dist is empty)".into()),
    }
}

pub async fn legacy() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")],
        include_str!("../../../web/index.html"),
    )
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use sha2::{Digest, Sha256};

    use super::*;

    /// Files the build reads; must match `frontend/scripts/source-hash.mjs`.
    fn sources(root: &Path) -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    walk(&p, out);
                } else if !p.to_string_lossy().ends_with(".test.ts") {
                    out.push(p);
                }
            }
        }
        let mut out = Vec::new();
        for f in ["index.html", "package.json", "package-lock.json", "vite.config.ts", "tsconfig.json"] {
            out.push(root.join(f));
        }
        walk(&root.join("src"), &mut out);
        walk(&root.join("public"), &mut out);
        out
    }

    fn source_hash(root: &Path) -> String {
        let mut files: Vec<(String, PathBuf)> = sources(root)
            .into_iter()
            .map(|p| (p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"), p))
            .collect();
        files.sort();
        let mut h = Sha256::new();
        for (rel, p) in files {
            h.update(rel.as_bytes());
            h.update([0]);
            h.update(std::fs::read(&p).unwrap());
            h.update([0]);
        }
        hex::encode(h.finalize())
    }

    /// The embedded `frontend/dist` is built from the checkout's sources (build.rs).
    #[test]
    fn embedded_frontend_is_current() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../frontend");
        assert_eq!(
            built_from(),
            Some(source_hash(&root).as_str()),
            "frontend/dist is stale: build.rs rebuilds it (or run `npm run build` in frontend/)"
        );
    }

    #[tokio::test]
    async fn serves_app_routes_and_rejects_unknown_api_paths() {
        let index = app(Method::GET, Uri::from_static("/chat/main")).await;
        assert_eq!(index.status(), StatusCode::OK);
        assert_eq!(index.headers()[header::CONTENT_TYPE], "text/html; charset=utf-8");
        assert_eq!(app(Method::GET, Uri::from_static("/api/v1/nope")).await.status(), StatusCode::NOT_FOUND);
        assert_eq!(app(Method::GET, Uri::from_static("/assets/missing.js")).await.status(), StatusCode::NOT_FOUND);
        assert_eq!(app(Method::GET, Uri::from_static("/.source-hash")).await.status(), StatusCode::NOT_FOUND);
        assert_eq!(app(Method::POST, Uri::from_static("/x")).await.status(), StatusCode::METHOD_NOT_ALLOWED);
        let asset = DIST.get_dir("assets").and_then(|d| d.files().next()).expect("dist has assets");
        let path = format!("/{}", asset.path().display());
        let resp = app(Method::GET, path.parse().unwrap()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers()[header::CACHE_CONTROL].to_str().unwrap().contains("immutable"));
    }
}
