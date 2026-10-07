use crate::error::Error;
use axum::{
    extract::{Path, RawQuery, State as Extract},
    http::{HeaderMap, StatusCode},
    routing::get,
    Json, Router,
};
use rdt_request::Upstream;
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
}

impl State {
    pub fn new(upstream: Upstream) -> Self {
        Self::from_backend(Backend::Reddit(Box::new(upstream)))
    }

    /// Fetch through the same validation, cache and rate limits used by HTTP.
    pub async fn get(
        &self,
        path: &str,
        query: &[(String, String)],
    ) -> Result<rdt_gateway_types::RawResponse, Error> {
        let path = path
            .strip_prefix('/')
            .ok_or_else(|| Error::invalid("Expected an absolute Reddit .json path"))?;
        let query = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(query)
            .finish();
        let result = self
            .fetch(upstream_path(
                path,
                (!query.is_empty()).then_some(query.as_str()),
            )?)
            .await?;
        Ok(rdt_gateway_types::RawResponse {
            data: result.value,
            fetched_at: result.fetched_at,
        })
    }
    fn from_backend(backend: Backend) -> Self {
        Self(Arc::new(Inner {
            backend,
            cache: Mutex::new(HashMap::new()),
            permits: Semaphore::new(2),
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
                tracing::warn!(code = %e.body.code, "upstream request failed");
                return Err(e);
            }
        };
        let fetched_at = chrono::Utc::now().to_rfc3339();
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
        .route("/{*path}", get(reddit))
        .route("/health", get(|| async { Json(json!({"ok":true})) }))
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
    {
        return Err(Error::invalid("Expected a relative Reddit .json path"));
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
    async fn embedded_and_http_requests_share_cache_and_policy() {
        let state = state();
        let query = vec![
            ("q".into(), "rust & nix".into()),
            ("q".into(), "a=b".into()),
        ];
        let embedded = state.get("/search.json", &query).await.unwrap();
        let (status, headers, http) =
            request(state.clone(), "/search.json?q=rust+%26+nix&q=a%3Db").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(embedded.data, http);
        assert_eq!(embedded.fetched_at, headers["x-reddit-fetched-at"]);
        assert_eq!(calls(&state).await, ["/search.json?q=rust+%26+nix&q=a%3Db"]);
        for path in ["search.json", "//search.json", "/r/../search.json"] {
            assert!(state.get(path, &[]).await.is_err(), "{path}");
        }
        assert_eq!(calls(&state).await.len(), 1);
    }

    #[tokio::test]
    async fn cache_preserves_fetch_time_and_raw_response() {
        let state = state();
        let (status, first_headers, first) = request(state.clone(), "/search.json?q=rust").await;
        let (_, second_headers, second) = request(state.clone(), "/search.json?q=rust").await;
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
        request(state.clone(), "/search.json?q=rust").await;
        assert_eq!(calls(&state).await.len(), 2);
    }
    #[tokio::test]
    async fn arbitrary_json_and_long_text_are_preserved() {
        let value = json!({"unknown_future_field":{"body":"あ".repeat(200_000)}, "items":[null, false, 123]});
        let state = fixture(value.clone());
        let (status, _, returned) = request(state.clone(), "/example.json").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(returned, value);
        assert_eq!(state.0.cache.lock().await.len(), 1);
    }
    #[tokio::test]
    async fn query_is_forwarded_without_remapping_or_injected_defaults() {
        let state = state();
        request(
            state.clone(),
            "/search.json?q=a%26b&q=c+d&future=1&after=t3_abc&raw_json=0",
        )
        .await;
        request(state.clone(), "/r/rust/new.json").await;
        assert_eq!(
            calls(&state).await,
            [
                "/search.json?q=a%26b&q=c+d&future=1&after=t3_abc&raw_json=0",
                "/r/rust/new.json",
            ]
        );
    }
    #[tokio::test]
    async fn api_paths_are_forwarded_by_embedded_and_http_transports() {
        let state = state();
        for path in ["/api/info.json", "/api/morechildren.json"] {
            let query = vec![("id".into(), "t3_example".into())];
            let embedded = state.get(path, &query).await.unwrap();
            let uri = format!("{path}?id=t3_example");
            let (status, _, http) = request(state.clone(), &uri).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(embedded.data, http);
        }
        assert_eq!(
            calls(&state).await,
            [
                "/api/info.json?id=t3_example",
                "/api/morechildren.json?id=t3_example",
            ]
        );
    }
    #[tokio::test]
    async fn validation_precedes_upstream() {
        let state = state();
        for uri in [
            "/search",
            "//search.json",
            "/r/../search.json",
            "/r/%2e%2e/search.json",
            "/https:%2f%2fexample.com/search.json",
            "/foo%3fbar.json",
            "/foo%5cbar.json",
        ] {
            let (status, _, body) = request(state.clone(), uri).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(body["error"]["code"], "invalid_request");
        }
        let oversized = format!("/search.json?q={}", "x".repeat(4096));
        assert_eq!(
            request(state.clone(), &oversized).await.0,
            StatusCode::BAD_REQUEST
        );
        assert!(calls(&state).await.is_empty());
        assert_eq!(
            request(state.clone(), "/v1/search?q=rust").await.0,
            StatusCode::BAD_REQUEST
        );
    }
    #[tokio::test]
    async fn write_methods_never_reach_upstream() {
        let state = state();
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/vote.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert!(calls(&state).await.is_empty());
    }
    #[tokio::test]
    async fn health_does_not_fetch_upstream() {
        let state = state();
        let (status, _, body) = request(state.clone(), "/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({"ok": true}));
        assert!(calls(&state).await.is_empty());
    }
}
