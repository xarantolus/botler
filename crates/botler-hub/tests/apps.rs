//! Apps: a service's web UI behind botler's sign-in, on the apps port: a ticket from the web app
//! sets a cookie for that app only, and requests reach the service without botler's credentials.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::routing::get;
use botler_hub::apps;
use tower::ServiceExt;

const APP: &str =
    "lifetime = \"service\"\ncommand = [\"ui\"]\nport = 8080\n[app]\ntitle = \"Home\"\npath = \"/start\"\n";

#[tokio::test]
async fn an_app_opens_with_a_ticket_and_reaches_its_service() {
    let e = common::env().await;
    let files = common::job("home", APP);
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    e.commit(&refs).await;

    botler_hub::services::reconcile(&e.hub).await.unwrap();
    let c = &e.fake.containers()[&botler_hub::services::container_name(&e.hub, "home")];
    let ports: Vec<_> = c.spec.ports.iter().map(|p| (p.bind.as_str(), p.host_port, p.port)).collect();
    assert_eq!(ports, [("127.0.0.1", apps::host_port(&e.hub.id, "home"), 8080)], "only the hub's proxy reaches it");

    // The service, as the hub reaches it: its port published on 127.0.0.1.
    let svc = axum::Router::new()
        .route(
            "/start",
            get(|headers: axum::http::HeaderMap| async move {
                let leaked = headers.contains_key(header::COOKIE) || headers.contains_key(header::AUTHORIZATION);
                let prefix =
                    headers.get("x-forwarded-prefix").map(|v| v.to_str().unwrap().to_owned()).unwrap_or_default();
                ([(header::SET_COOKIE, "sid=1; Path=/")], format!("hello {prefix} leaked={leaked}"))
            }),
        )
        .route("/page", get(|| async { "<p>a page</p>".repeat(100) }));
    let port = apps::host_port(&e.hub.id, "home");
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    tokio::spawn(async move { axum::serve(listener, svc).await.unwrap() });

    let router = apps::router(e.hub.clone());
    let send = |req: Request<Body>| router.clone().oneshot(req);
    let without = send(Request::get("/apps/home/start").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(without.status(), StatusCode::UNAUTHORIZED, "not without signing in");
    let stale = send(Request::get("/apps/home/__botler/enter?ticket=1.x").body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(stale.status(), StatusCode::UNAUTHORIZED);

    let ticket = apps::ticket(&e.hub, "home");
    let other = send(Request::get(apps::enter_path("other", &ticket)).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(other.status(), StatusCode::NOT_FOUND);
    let entered = send(Request::get(apps::enter_path("home", &ticket)).body(Body::empty()).unwrap()).await.unwrap();
    assert_eq!(entered.status(), StatusCode::SEE_OTHER);
    assert_eq!(entered.headers()[header::LOCATION], "/apps/home/start");
    let cookie = entered.headers()[header::SET_COOKIE].to_str().unwrap().to_owned();
    assert!(cookie.contains("Path=/apps/home/") && cookie.contains("HttpOnly"), "{cookie}");
    let value = cookie.split(';').next().unwrap().to_owned();

    let req = Request::get("/apps/home/start")
        .header(header::COOKIE, format!("{value}; botler_session=x"))
        .header(header::AUTHORIZATION, "Bearer secret")
        .body(Body::empty())
        .unwrap();
    let resp = send(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::SET_COOKIE], "sid=1; Path=/apps/home/", "the app's cookie stays under it");
    let body = axum::body::to_bytes(resp.into_body(), 1 << 16).await.unwrap();
    assert_eq!(std::str::from_utf8(&body).unwrap(), "hello /apps/home leaked=false");

    // Compressed for a browser that takes it.
    let req = Request::get("/apps/home/page")
        .header(header::COOKIE, &value)
        .header(header::ACCEPT_ENCODING, "gzip")
        .body(Body::empty())
        .unwrap();
    let resp = send(req).await.unwrap();
    assert_eq!(resp.headers()[header::CONTENT_ENCODING], "gzip");

    // Another app's cookie does not open this one.
    let elsewhere =
        format!("{}={}", value.split('=').next().unwrap().replace("home", "other"), value.split_once('=').unwrap().1);
    let req = Request::get("/apps/home/start").header(header::COOKIE, elsewhere).body(Body::empty()).unwrap();
    assert_eq!(send(req).await.unwrap().status(), StatusCode::UNAUTHORIZED);
}
