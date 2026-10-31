//! Signing in: one-time codes and tokens give a device an HttpOnly cookie; cookie requests need
//! the `x-botler` header; devices are listed and signed out.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use botler_hub::auth::Principal;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn send(e: &common::Env, req: Request<Body>) -> (StatusCode, Option<String>, Value) {
    let resp = botler_hub::api::router(e.hub.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let cookie = resp.headers().get(header::SET_COOKIE).map(|v| v.to_str().unwrap().to_owned());
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    (status, cookie, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn post(path: &str) -> axum::http::request::Builder {
    Request::builder().method("POST").uri(path).header("content-type", "application/json")
}

#[tokio::test]
async fn codes_and_tokens_sign_devices_in_with_a_cookie() {
    let e = common::env().await;
    let sign_in = |secret: &str| {
        let body = json!({"secret": secret, "name": "Phone"});
        post("/api/v1/login").body(Body::from(body.to_string())).unwrap()
    };
    let (status, _, _) = send(&e, sign_in("WRONG234")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (code, _) = botler_hub::login::create_code(&e.hub).await.unwrap();
    let (status, cookie, body) = send(&e, sign_in(&format!("{}-{}", &code[..4], code[4..].to_lowercase()))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let cookie = cookie.unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"), "{cookie}");
    let value = cookie.split(';').next().unwrap().to_owned();
    assert_eq!(send(&e, sign_in(&code)).await.0, StatusCode::UNAUTHORIZED, "a code works once");

    let get = |path: &str, csrf: bool| {
        let mut b = Request::builder().uri(path).header(header::COOKIE, &value);
        if csrf {
            b = b.header("x-botler", "1");
        }
        b.body(Body::empty()).unwrap()
    };
    assert_eq!(send(&e, get("/api/v1/status", false)).await.0, StatusCode::FORBIDDEN, "the header is required");
    assert_eq!(send(&e, get("/api/v1/status", true)).await.0, StatusCode::OK);
    let (_, _, devices) = send(&e, get("/api/v1/devices", true)).await;
    assert_eq!((devices[0]["name"].as_str(), devices[0]["current"].as_bool()), (Some("Phone"), Some(true)));

    // A user token signs in too; signing a device out ends its cookie.
    let token = e.hub.tokens.issue(&Principal::User, chrono::Duration::hours(1), chrono::Utc::now());
    assert_eq!(send(&e, sign_in(&token)).await.0, StatusCode::OK);
    let id = devices[0]["id"].as_str().unwrap();
    let del = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/devices/{id}"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&e, del).await.2, json!({"revoked": true}));
    assert_eq!(send(&e, get("/api/v1/status", true)).await.0, StatusCode::UNAUTHORIZED);
}
