use http_body_util::{BodyExt, Full};
use hyper::{Request, body::Bytes};
use hyper_util::rt::TokioIo;
use nd_testkit::{ClaudeEndpoint, ModelReply, Route};
use serde_json::{Value, json};
use std::path::Path;

async fn request(socket: &Path, agent: Option<&str>, model: &str) -> (u16, Value) {
    let stream = tokio::net::UnixStream::connect(socket).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut req = Request::post("http://localhost/v1/messages?beta=true");
    if let Some(agent) = agent {
        req = req.header("x-claude-code-agent-id", agent);
    }
    let response = sender.send_request(req.header("content-type", "application/json").body(Full::new(Bytes::from(json!({
        "model": model, "messages": [{"role":"user","content":"hello"}], "max_tokens":64
    }).to_string()))).unwrap()).await.unwrap();
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn replies_route_by_agent_and_model_and_unplanned_requests_fail_closed() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = ClaudeEndpoint::bind(dir.path().join("model.sock"))
        .await
        .unwrap();
    endpoint.enqueue(Route::new(None, "haiku"), ModelReply::text("main"));
    endpoint.enqueue(
        Route::new(Some("worker"), "sonnet"),
        ModelReply::text("child"),
    );
    endpoint.enqueue(Route::new(None, "sonnet"), ModelReply::text("main-sonnet"));
    endpoint.enqueue(
        Route::new(Some("worker"), "haiku"),
        ModelReply::text("child-haiku"),
    );
    assert_eq!(
        request(endpoint.socket(), Some("worker"), "sonnet").await.1["content"][0]["text"],
        "child"
    );
    assert_eq!(
        request(endpoint.socket(), None, "sonnet").await.1["content"][0]["text"],
        "main-sonnet"
    );
    assert_eq!(
        request(endpoint.socket(), Some("worker"), "haiku").await.1["content"][0]["text"],
        "child-haiku"
    );
    assert_eq!(
        request(endpoint.socket(), None, "haiku").await.1["content"][0]["text"],
        "main"
    );
    assert_eq!(
        request(endpoint.socket(), Some("other"), "haiku").await.0,
        409
    );
    let requests = endpoint.requests();
    assert_eq!(requests.len(), 5);
    assert_eq!(requests[0].route, Route::new(Some("worker"), "sonnet"));
    assert_eq!(requests[0].body["messages"][0]["content"], "hello");
    assert_eq!(endpoint.count(&Route::new(None, "haiku")), 1);
}

#[tokio::test]
async fn held_requests_are_counted_without_blocking_other_agents_and_release_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = ClaudeEndpoint::bind(dir.path().join("model.sock"))
        .await
        .unwrap();
    let route = Route::new(None, "haiku");
    let gate = endpoint.enqueue_held(route.clone(), ModelReply::text("released"));
    let path = endpoint.socket().to_owned();
    let mut held = tokio::spawn(async move { request(&path, None, "haiku").await });
    endpoint
        .wait_for_requests(&route, 1, std::time::Duration::from_secs(2))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(40), &mut held)
            .await
            .is_err()
    );
    endpoint.enqueue(
        Route::new(Some("worker"), "haiku"),
        ModelReply::text("independent"),
    );
    assert_eq!(
        request(endpoint.socket(), Some("worker"), "haiku").await.1["content"][0]["text"],
        "independent"
    );
    gate.release();
    assert_eq!(held.await.unwrap().1["content"][0]["text"], "released");
    assert_eq!(endpoint.count(&route), 1);
    // A dropped gate fails the waiting request, never silently releases a planned success.
    let cancelled = endpoint.enqueue_held(route, ModelReply::text("must not appear"));
    drop(cancelled);
    assert_eq!(request(endpoint.socket(), None, "haiku").await.0, 503);
}

#[tokio::test]
async fn endpoint_shutdown_cancels_held_responses_even_when_caller_keeps_the_gate() {
    let dir = tempfile::tempdir().unwrap();
    let endpoint = ClaudeEndpoint::bind(dir.path().join("model.sock"))
        .await
        .unwrap();
    let route = Route::new(None, "haiku");
    let _gate = endpoint.enqueue_held(route.clone(), ModelReply::text("too late"));
    let socket = endpoint.socket().to_owned();
    let held = tokio::spawn(async move { request(&socket, None, "haiku").await });
    endpoint
        .wait_for_requests(&route, 1, std::time::Duration::from_secs(2))
        .await
        .unwrap();
    drop(endpoint);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), held)
            .await
            .unwrap()
            .unwrap()
            .0,
        503
    );
    assert!(!dir.path().join("model.sock").exists());
}
