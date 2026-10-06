//! Reddit SDK shared by the independently installable CLI and MCP server.
//! The gateway transports raw JSON; this library owns request mapping and models.
mod normalize;
mod reddit;
pub use rdt_gateway_types::{Comment, Envelope, Post, RawResponse};
pub use reddit::{CommentOptions, ListOptions, SearchOptions};
use std::time::Duration;

use rdt_gateway_types::ErrorEnvelope;
use reqwest::{StatusCode, Url};

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid gateway URL: expected an http(s) origin URL without credentials, path, query, or fragment")]
    InvalidBaseUrl,
    #[error("invalid gateway API path")]
    InvalidPath,
    #[error("invalid Reddit argument: {0}")]
    InvalidArgument(String),
    #[error("invalid Reddit response: {0}")]
    InvalidSchema(String),
    #[error("gateway response has no valid fetch timestamp")]
    InvalidMetadata,
    #[error("gateway request failed: {0}")]
    Transport(reqwest::Error),
    #[error("gateway response exceeds the 8 MiB client limit")]
    ResponseTooLarge,
    #[error("gateway returned invalid JSON")]
    InvalidJson,
    #[error("gateway returned HTTP {status}: {code}: {message} (retryable: {retryable})")]
    Gateway {
        status: StatusCode,
        code: String,
        message: String,
        retryable: bool,
        retry_after_seconds: Option<u64>,
    },
    #[error("gateway returned HTTP {0} without a structured error")]
    Http(StatusCode),
}

impl Error {
    fn schema(message: impl Into<String>) -> Self {
        Self::InvalidSchema(message.into())
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self::InvalidArgument(message.into())
    }
}

/// Supplies original Reddit JSON and acquisition metadata to the SDK.
pub trait Transport: Clone + Send + Sync + 'static {
    fn raw_get(
        &self,
        path: &str,
        query: &[(String, String)],
    ) -> impl std::future::Future<Output = Result<RawResponse, Error>> + Send;
}

#[derive(Clone)]
pub struct Client<T = HttpTransport> {
    transport: T,
}

impl Client<HttpTransport> {
    pub fn new(base_url: &str) -> Result<Self, Error> {
        Ok(Self::with_transport(HttpTransport::new(base_url)?))
    }
}

impl<T: Transport> Client<T> {
    pub fn with_transport(transport: T) -> Self {
        Self { transport }
    }

    pub async fn raw_get(
        &self,
        path: &str,
        query: &[(String, String)],
    ) -> Result<RawResponse, Error> {
        self.transport.raw_get(path, query).await
    }
}

/// Connects the SDK to a remote gateway over HTTP.
#[derive(Clone)]
pub struct HttpTransport {
    base_url: Url,
    http: reqwest::Client,
}

impl HttpTransport {
    pub fn new(base_url: &str) -> Result<Self, Error> {
        let base_url = Url::parse(base_url).map_err(|_| Error::InvalidBaseUrl)?;
        if !matches!(base_url.scheme(), "http" | "https")
            || base_url.host_str().is_none()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
            || base_url.path() != "/"
        {
            return Err(Error::InvalidBaseUrl);
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(35))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(transport_error)?;
        Ok(Self { base_url, http })
    }
}

impl Transport for HttpTransport {
    async fn raw_get(&self, path: &str, query: &[(String, String)]) -> Result<RawResponse, Error> {
        if !path.starts_with('/')
            || path.starts_with("//")
            || !path
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"/_-.".contains(&c))
            || path
                .split('/')
                .skip(1)
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err(Error::InvalidPath);
        }
        let url = self
            .base_url
            .join(&format!("/reddit{path}"))
            .map_err(|_| Error::InvalidPath)?;
        let mut response = self
            .http
            .get(url)
            .query(query)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        let fetched_at = response
            .headers()
            .get("x-reddit-fetched-at")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if response
            .content_length()
            .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
        {
            return Err(Error::ResponseTooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(Error::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return match serde_json::from_slice::<ErrorEnvelope>(&body) {
                Ok(envelope) => Err(Error::Gateway {
                    status,
                    code: envelope.error.code,
                    message: envelope.error.message,
                    retryable: envelope.error.retryable,
                    retry_after_seconds: envelope.error.retry_after_seconds,
                }),
                Err(_) => Err(Error::Http(status)),
            };
        }
        let data = serde_json::from_slice(&body).map_err(|_| Error::InvalidJson)?;
        let fetched_at = fetched_at
            .filter(|v| chrono::DateTime::parse_from_rfc3339(v).is_ok())
            .ok_or(Error::InvalidMetadata)?;
        Ok(RawResponse { data, fetched_at })
    }
}

fn transport_error(error: reqwest::Error) -> Error {
    // Query strings can contain private research terms; omit the request URL.
    Error::Transport(error.without_url())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn read_headers(stream: &mut tokio::net::TcpStream) {
        let mut headers = Vec::new();
        while !headers.ends_with(b"\r\n\r\n") {
            assert!(headers.len() < 8192, "test request headers too large");
            headers.push(stream.read_u8().await.unwrap());
        }
    }

    #[test]
    fn rejects_ambiguous_or_credentialed_origins() {
        for url in [
            "file:///tmp/x",
            "http://a:b@localhost",
            "http://localhost/?secret=x",
            "http://localhost/#x",
            "http://localhost/prefix",
            "invalid",
        ] {
            assert!(
                matches!(Client::new(url), Err(Error::InvalidBaseUrl)),
                "{url}"
            );
        }
        assert!(Client::new("http://127.0.0.1:8787").is_ok());
    }

    async fn serve_once(status: &str, body: &str) -> Client {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\nx-reddit-fetched-at: 2026-10-06T00:00:00Z\r\n\r\n{body}",
            body.len()
        );
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_headers(&mut stream).await;
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        Client::new(&format!("http://{address}")).unwrap()
    }

    #[tokio::test]
    async fn preserves_structured_gateway_errors() {
        let client = serve_once(
            "429 Too Many Requests",
            r#"{"error":{"code":"rate_limited","message":"Try later","retryable":true}}"#,
        )
        .await;
        let error = client.raw_get("/search.json", &[]).await.unwrap_err();
        assert!(matches!(
            error,
            Error::Gateway {
                status: StatusCode::TOO_MANY_REQUESTS,
                retryable: true,
                ..
            }
        ));
        assert!(error.to_string().contains("rate_limited: Try later"));
    }

    #[tokio::test]
    async fn rejects_non_json_success_and_unstructured_error() {
        let client = serve_once("200 OK", "<html>oops</html>").await;
        assert!(matches!(
            client.raw_get("/search.json", &[]).await,
            Err(Error::InvalidJson)
        ));
        let client = serve_once("502 Bad Gateway", "<html>oops</html>").await;
        assert!(matches!(
            client.raw_get("/search.json", &[]).await,
            Err(Error::Http(StatusCode::BAD_GATEWAY))
        ));
    }

    #[tokio::test]
    async fn rejects_oversized_response_before_buffering() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_headers(&mut stream).await;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8388609\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let client = Client::new(&format!("http://{address}")).unwrap();
        assert!(matches!(
            client.raw_get("/search.json", &[]).await,
            Err(Error::ResponseTooLarge)
        ));
    }

    #[tokio::test]
    async fn refuses_origin_overrides_without_requesting_them() {
        let client = Client::new("http://127.0.0.1:1").unwrap();
        for path in [
            "//example.com/x",
            "http://example.com/x",
            "/\\example.com",
            "/search.json?q=oops",
            "/../health/live",
            "/%2e%2e/health/live",
        ] {
            assert!(matches!(
                client.raw_get(path, &[]).await,
                Err(Error::InvalidPath)
            ));
        }
    }
}
