use super::*;
use axum::{
    extract::State as AxumState,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[derive(Clone)]
struct Mock {
    auth_calls: Arc<AtomicUsize>,
    api_calls: Arc<AtomicUsize>,
    status: u16,
}
async fn authentication(
    AxumState(mock): AxumState<Mock>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    assert_eq!(body, json!({"scopes": ["*", "email", "pii"]}));
    mock.auth_calls.fetch_add(1, Ordering::SeqCst);
    Json(json!({"access_token": "test-token", "expires_in": 3600}))
}
async fn listing(
    AxumState(mock): AxumState<Mock>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    assert_eq!(headers.get("authorization").unwrap(), "Bearer test-token");
    mock.api_calls.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::from_u16(mock.status).unwrap(),
        [("retry-after", "120")],
        if mock.status == 404 {
            "<html>not found</html>"
        } else {
            "{\"data\":{\"children\":[]}}"
        },
    )
}
async fn mock(status: u16) -> (Upstream, Mock, tokio::task::JoinHandle<()>) {
    let state = Mock {
        auth_calls: Arc::new(AtomicUsize::new(0)),
        api_calls: Arc::new(AtomicUsize::new(0)),
        status,
    };
    let app = Router::new()
        .route("/auth", post(authentication))
        .route("/listing", get(listing))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
    let upstream = Upstream {
        client: Client::builder()
            .redirect(wreq::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap(),
        state: Mutex::new(State {
            headers: device::headers(),
            token: None,
            cooldown: None,
        }),
        auth_url: format!("{base}/auth"),
        api_url: base,
    };
    (upstream, state, task)
}

#[test]
fn rejects_invalid_auth_fields_without_leaking_token() {
    for value in [
        json!({}),
        json!({"access_token":"secret", "expires_in":0}),
        json!({"access_token":"", "expires_in":10}),
    ] {
        let error = parse_token(value).err().unwrap();
        assert_eq!(error.code, "upstream_auth");
        assert!(!error.message.contains("secret"));
    }
    assert!(parse_token(json!({"access_token":"short", "expires_in":1})).is_ok());
}

#[tokio::test]
async fn reuses_token_and_refreshes_only_after_expiry() {
    let (upstream, state, task) = mock(200).await;
    upstream.json("/listing").await.unwrap();
    upstream.json("/listing").await.unwrap();
    assert_eq!(state.auth_calls.load(Ordering::SeqCst), 1);
    upstream.state.lock().await.token.as_mut().unwrap().expires = Instant::now();
    upstream.json("/listing").await.unwrap();
    assert_eq!(state.auth_calls.load(Ordering::SeqCst), 2);
    task.abort();
}

#[tokio::test]
async fn rate_limit_preserves_token_and_blocks_more_requests() {
    let (upstream, state, task) = mock(429).await;
    let error = upstream.json("/listing").await.unwrap_err();
    assert_eq!(error.status, 429);
    assert_eq!(error.retry_after_seconds, Some(120));
    assert_eq!(upstream.json("/listing").await.unwrap_err().status, 429);
    assert_eq!(state.auth_calls.load(Ordering::SeqCst), 1);
    assert_eq!(state.api_calls.load(Ordering::SeqCst), 1);
    assert!(upstream.state.lock().await.token.is_some());
    task.abort();
}

#[tokio::test]
async fn unauthorized_invalidates_token_without_immediate_retry() {
    let (upstream, state, task) = mock(401).await;
    assert_eq!(upstream.json("/listing").await.unwrap_err().status, 503);
    assert_eq!(upstream.json("/listing").await.unwrap_err().status, 503);
    assert_eq!(state.auth_calls.load(Ordering::SeqCst), 1);
    assert_eq!(state.api_calls.load(Ordering::SeqCst), 1);
    assert!(upstream.state.lock().await.token.is_none());
    task.abort();
}

#[tokio::test]
async fn unexpected_redirect_is_rejected() {
    let (upstream, _, task) = mock(302).await;
    assert_eq!(
        upstream.json("/listing").await.unwrap_err().code,
        "upstream_redirect"
    );
    task.abort();
}

#[tokio::test]
async fn classifies_http_error_before_parsing_html_body() {
    let (upstream, _, task) = mock(404).await;
    let error = upstream.json("/listing").await.unwrap_err();
    assert_eq!(error.status, 404);
    assert_eq!(error.code, "not_found");
    task.abort();
}

#[tokio::test]
async fn refuses_absolute_and_authority_paths_before_network() {
    let (upstream, state, task) = mock(200).await;
    for path in [
        "https://example.com",
        "//example.com",
        "/\\example.com",
        "/x#fragment",
    ] {
        assert_eq!(upstream.json(path).await.unwrap_err().status, 400);
    }
    assert_eq!(state.auth_calls.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn distinguishes_content_restrictions_from_unknown_access_blocks() {
    for (body, expected) in [
        ("<html>blocked</html>", 503),
        (r#"{"reason":"private"}"#, 403),
    ] {
        let app = Router::new().route(
            "/",
            get(move || async move { (StatusCode::FORBIDDEN, body) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
        let response = Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap();
        let error = decode(response).await.unwrap_err();
        assert_eq!(error.status, expected);
        assert_eq!(error.retryable, expected == 503);
        task.abort();
    }
}

#[tokio::test]
async fn enforces_eight_mib_response_body_limit() {
    const EIGHT_MIB: usize = 8 * 1024 * 1024;
    let app = Router::new()
        .route(
            "/within",
            get(|| async { format!("\"{}\"", "a".repeat(EIGHT_MIB - 2)) }),
        )
        .route(
            "/over",
            get(|| async { format!("\"{}\"", "a".repeat(EIGHT_MIB - 1)) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async { axum::serve(listener, app).await.unwrap() });
    let client = Client::builder().no_proxy().build().unwrap();

    let response = client.get(format!("{base}/within")).send().await.unwrap();
    let value = decode(response).await.unwrap();
    assert_eq!(value.as_str().unwrap().len(), EIGHT_MIB - 2);

    let response = client.get(format!("{base}/over")).send().await.unwrap();
    let error = decode(response).await.unwrap_err();
    assert_eq!(error.status, 502);
    assert_eq!(error.code, "upstream_body_too_large");
    task.abort();
}

#[tokio::test(start_paused = true)]
async fn deadline_includes_waiting_for_upstream_lock() {
    let (upstream, state, task) = mock(200).await;
    let _held_lock = upstream.state.lock().await;
    let started = tokio::time::Instant::now();

    let error = upstream.json("/listing").await.unwrap_err();

    assert_eq!(error.status, 504);
    assert_eq!(error.code, "upstream_timeout");
    assert_eq!(started.elapsed(), Duration::from_secs(30));
    assert_eq!(state.auth_calls.load(Ordering::SeqCst), 0);
    assert_eq!(state.api_calls.load(Ordering::SeqCst), 0);
    task.abort();
}
