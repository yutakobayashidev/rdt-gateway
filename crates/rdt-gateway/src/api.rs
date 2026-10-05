use crate::{error::Error, upstream::Upstream};
use axum::{
    extract::{Path, RawQuery, State as Extract},
    http::{HeaderMap, StatusCode},
    routing::get,
    Json, Router,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, Semaphore};

const CACHE_TTL: Duration = Duration::from_secs(30);
const CACHE_ENTRIES: usize = 100;
const CACHE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone)]
struct Cached {
    value: Value,
    fetched_at: String,
    inserted: Instant,
    size: usize,
}

enum Backend {
    Reddit(Box<Upstream>),
    #[cfg(test)]
    Fixture(Value, Mutex<Vec<String>>),
}

#[derive(Clone)]
pub struct State(Arc<Inner>);
struct Inner {
    backend: Backend,
    cache: Mutex<HashMap<String, Cached>>,
    permits: Semaphore,
    health: Mutex<Health>,
}
#[derive(Default)]
struct Health {
    last_success: Option<(Instant, String)>,
    last_error: Option<String>,
}

impl State {
    pub fn new(upstream: Upstream) -> Self {
        Self::from_backend(Backend::Reddit(Box::new(upstream)))
    }
    fn from_backend(backend: Backend) -> Self {
        Self(Arc::new(Inner {
            backend,
            cache: Mutex::new(HashMap::new()),
            permits: Semaphore::new(2),
            health: Mutex::new(Health::default()),
        }))
    }
    async fn fetch(&self, path: String) -> Result<Cached, Error> {
        {
            let cache = self.0.cache.lock().await;
            if let Some(value) = cache
                .get(&path)
                .filter(|v| v.inserted.elapsed() < CACHE_TTL)
            {
                return Ok(value.clone());
            }
        }
        let _permit = self.0.permits.try_acquire().map_err(|_| {
            let mut error = Error::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "Gateway is busy; retry later",
            );
            error.body.retry_after_seconds = Some(1);
            error
        })?;
        let result = match &self.0.backend {
            Backend::Reddit(upstream) => upstream.json(&path).await.map_err(Error::from),
            #[cfg(test)]
            Backend::Fixture(value, calls) => {
                calls.lock().await.push(path.clone());
                Ok(value.clone())
            }
        };
        let value = match result {
            Ok(value) => value,
            Err(e) => {
                self.0.health.lock().await.last_error = Some(e.body.code.clone());
                tracing::warn!(code = %e.body.code, "upstream request failed");
                return Err(e);
            }
        };
        let fetched_at = chrono::Utc::now().to_rfc3339();
        {
            let mut health = self.0.health.lock().await;
            health.last_success = Some((Instant::now(), fetched_at.clone()));
            health.last_error = None;
        }
        let cached = Cached {
            size: serde_json::to_vec(&value)
                .map_err(|_| Error::schema("Invalid JSON"))?
                .len(),
            value,
            fetched_at,
            inserted: Instant::now(),
        };
        let mut cache = self.0.cache.lock().await;
        cache.retain(|_, entry| entry.inserted.elapsed() < CACHE_TTL);
        while !cache.is_empty()
            && (cache.len() >= CACHE_ENTRIES
                || cache.values().map(|e| e.size).sum::<usize>() + cached.size > CACHE_BYTES)
        {
            if let Some(key) = cache
                .iter()
                .min_by_key(|(_, v)| v.inserted)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&key);
            }
        }
        cache.insert(path, cached.clone());
        Ok(cached)
    }
}

pub fn router(state: State) -> Router {
    Router::new()
        .route("/reddit/{*path}", get(reddit))
        .route("/health/live", get(|| async { Json(json!({"live":true})) }))
        .route("/health/ready", get(ready))
        .fallback(|| async { Error::new(StatusCode::NOT_FOUND, "not_found", "Unknown endpoint") })
        .method_not_allowed_fallback(|| async {
            Error::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "Only GET is supported",
            )
        })
        .with_state(state)
}

async fn ready(Extract(state): Extract<State>) -> (StatusCode, Json<Value>) {
    let health = state.0.health.lock().await;
    let ready = health.last_error.is_none()
        && health
            .last_success
            .as_ref()
            .is_some_and(|(at, _)| at.elapsed() < Duration::from_secs(300));
    (
        if ready {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        Json(json!({
            "ready":ready,"last_upstream_success":health.last_success.as_ref().map(|(_,time)|time),
            "last_error":health.last_error,"freshness_seconds":300
        })),
    )
}

// Restrict the destination to relative, read-only Reddit JSON routes. Queries
// remain opaque: repeated and newly introduced Reddit parameters are preserved.
fn upstream_path(path: &str, query: Option<&str>) -> Result<String, Error> {
    if path.is_empty()
        || path.len() > 4096
        || !path.ends_with(".json")
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'/' | b'.'))
        || path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        || path.split('/').next().is_some_and(|segment| {
            segment.eq_ignore_ascii_case("api") || segment.eq_ignore_ascii_case("api.json")
        })
    {
        return Err(Error::invalid(
            "Expected a relative read-only Reddit .json path",
        ));
    }
    if query.is_some_and(|query| query.len() > 4096) {
        return Err(Error::invalid("Query string is too long"));
    }
    Ok(match query {
        Some(query) => format!("/{path}?{query}"),
        None => format!("/{path}"),
    })
}

async fn reddit(
    Extract(state): Extract<State>,
    Path(path): Path<String>,
    RawQuery(query): RawQuery,
) -> Result<(HeaderMap, Json<Value>), Error> {
    let result = state.fetch(upstream_path(&path, query.as_deref())?).await?;
    let mut headers = HeaderMap::new();
    headers.insert("x-reddit-fetched-at", result.fetched_at.parse().unwrap());
    Ok((headers, Json(result.value)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn fixture(value: Value) -> State {
        State::from_backend(Backend::Fixture(value, Mutex::new(Vec::new())))
    }
    fn state() -> State {
        fixture(json!({"kind":"Listing","data":{"children":[],"after":"t3_abc"}}))
    }
    async fn calls(state: &State) -> Vec<String> {
        match &state.0.backend {
            Backend::Fixture(_, calls) => calls.lock().await.clone(),
            _ => unreachable!(),
        }
    }
    async fn request(state: State, uri: &str) -> (StatusCode, HeaderMap, Value) {
        let response = router(state)
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, serde_json::from_slice(&body).unwrap())
    }
    #[tokio::test]
    async fn cache_preserves_fetch_time_and_raw_response() {
        let state = state();
        let (status, first_headers, first) =
            request(state.clone(), "/reddit/search.json?q=rust").await;
        let (_, second_headers, second) =
            request(state.clone(), "/reddit/search.json?q=rust").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(first, second);
        assert_eq!(first["data"]["after"], "t3_abc");
        assert_eq!(
            first_headers["x-reddit-fetched-at"],
            second_headers["x-reddit-fetched-at"]
        );
        chrono::DateTime::parse_from_rfc3339(
            first_headers["x-reddit-fetched-at"].to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(calls(&state).await.len(), 1);
        state
            .0
            .cache
            .lock()
            .await
            .values_mut()
            .for_each(|v| v.inserted -= Duration::from_secs(31));
        request(state.clone(), "/reddit/search.json?q=rust").await;
        assert_eq!(calls(&state).await.len(), 2);
    }
    #[tokio::test]
    async fn arbitrary_json_and_long_text_are_preserved_and_mark_ready() {
        let value = json!({"unknown_future_field":{"body":"あ".repeat(200_000)}, "items":[null, false, 123]});
        let state = fixture(value.clone());
        let (status, _, returned) = request(state.clone(), "/reddit/example.json").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(returned, value);
        assert_eq!(
            request(state.clone(), "/health/ready").await.0,
            StatusCode::OK
        );
        assert_eq!(state.0.cache.lock().await.len(), 1);
    }
    #[tokio::test]
    async fn query_is_forwarded_without_remapping_or_injected_defaults() {
        let state = state();
        request(
            state.clone(),
            "/reddit/search.json?q=a%26b&q=c+d&future=1&after=t3_abc&raw_json=0",
        )
        .await;
        request(state.clone(), "/reddit/r/rust/new.json").await;
        assert_eq!(
            calls(&state).await,
            [
                "/search.json?q=a%26b&q=c+d&future=1&after=t3_abc&raw_json=0",
                "/r/rust/new.json",
            ]
        );
    }
    #[tokio::test]
    async fn validation_precedes_upstream() {
        let state = state();
        for uri in [
            "/reddit/api/vote.json",
            "/reddit/api.json",
            "/reddit/search",
            "/reddit//search.json",
            "/reddit/r/../search.json",
            "/reddit/r/%2e%2e/search.json",
            "/reddit/https:%2f%2fexample.com/search.json",
            "/reddit/foo%3fbar.json",
            "/reddit/foo%5cbar.json",
        ] {
            let (status, _, body) = request(state.clone(), uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["error"]["code"], "invalid_request");
        }
        let oversized = format!("/reddit/search.json?q={}", "x".repeat(4096));
        assert_eq!(
            request(state.clone(), &oversized).await.0,
            StatusCode::BAD_REQUEST
        );
        assert!(calls(&state).await.is_empty());
        assert_eq!(
            request(state.clone(), "/v1/search?q=rust").await.0,
            StatusCode::NOT_FOUND
        );
    }
    #[tokio::test]
    async fn write_methods_never_reach_upstream() {
        let state = state();
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/reddit/search.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert!(calls(&state).await.is_empty());
    }
    #[tokio::test]
    async fn health_does_not_fetch_and_stale_success_is_not_ready() {
        let state = state();
        assert_eq!(
            request(state.clone(), "/health/live").await.0,
            StatusCode::OK
        );
        assert_eq!(
            request(state.clone(), "/health/ready").await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(calls(&state).await.is_empty());
        request(state.clone(), "/reddit/search.json?q=rust").await;
        assert_eq!(
            request(state.clone(), "/health/ready").await.0,
            StatusCode::OK
        );
        state.0.health.lock().await.last_success.as_mut().unwrap().0 -= Duration::from_secs(301);
        assert_eq!(
            request(state.clone(), "/health/ready").await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
