use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use isa_workspace::api::router;
use serde_json::json;
use tower::ServiceExt; // for `oneshot`

async fn app() -> Router {
    router()
}

#[tokio::test]
async fn snapshot_requires_label() {
    let app = app().await;

    let body = Body::from(json!({ "ttl": "24h" }).to_string());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/snapshot")
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn resume_requires_state_id() {
    let app = app().await;

    let body = Body::from(json!({ "mode": "collaborative" }).to_string());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/resume")
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn fork_requires_state_id() {
    let app = app().await;

    let body = Body::from(json!({ "label": "patched" }).to_string());
    let request = Request::builder()
        .method("POST")
        .uri("/v1/fork")
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
