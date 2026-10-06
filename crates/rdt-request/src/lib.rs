// Transport adapted from redlib-org/redlib; AGPL-3.0-only. See NOTICE.
mod device;
mod oauth_resources;

use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::Mutex;
use wreq::{Client, EmulationFactory, Response};
use wreq_util::{Emulation, EmulationOS, EmulationOption};

const DEADLINE: Duration = Duration::from_secs(30);
const MAX_BODY: usize = 8 * 1024 * 1024;
const AUTH_URL: &str = "https://www.reddit.com/auth/v2/oauth/access-token/loid";
const API_URL: &str = "https://oauth.reddit.com";

#[derive(Clone, Debug)]
pub struct Error {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
    pub retry_after_seconds: Option<u64>,
}
impl Error {
    fn new(status: u16, code: &'static str, message: &str, retryable: bool) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retryable,
            retry_after_seconds: None,
        }
    }
    fn auth() -> Self {
        Self::new(
            503,
            "upstream_auth",
            "Reddit authentication is temporarily unavailable",
            true,
        )
    }
    fn network() -> Self {
        Self::new(502, "upstream_network", "Reddit request failed", true)
    }
    fn invalid() -> Self {
        Self::new(
            502,
            "upstream_invalid_response",
            "Reddit returned an invalid response",
            false,
        )
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}

struct Token {
    value: String,
    expires: Instant,
}
struct State {
    headers: HashMap<String, String>,
    token: Option<Token>,
    cooldown: Option<(Instant, Error)>,
}
impl State {
    fn pause(&mut self, mut error: Error, seconds: u64) -> Error {
        // Upstream delay values are untrusted. Bound the duration to one day.
        let seconds = seconds.clamp(1, 86_400);
        error.retry_after_seconds = Some(seconds);
        self.cooldown = Some((Instant::now() + Duration::from_secs(seconds), error.clone()));
        error
    }
    fn ready(&self) -> Result<(), Error> {
        if let Some((until, error)) = &self.cooldown {
            if *until > Instant::now() {
                let mut error = error.clone();
                error.retry_after_seconds = Some(
                    until
                        .saturating_duration_since(Instant::now())
                        .as_secs()
                        .saturating_add(1),
                );
                return Err(error);
            }
        }
        Ok(())
    }
}

pub struct Upstream {
    client: Client,
    state: Mutex<State>,
    auth_url: String,
    api_url: String,
}
impl Upstream {
    pub async fn new() -> Result<Self, Error> {
        // Same emulation choices as the pinned Redlib revision, selected once per process.
        let selection = fastrand::usize(..);
        let emulation = EmulationOption::builder()
            .emulation([Emulation::Chrome145, Emulation::Firefox147][selection % 2])
            .emulation_os([EmulationOS::Android, EmulationOS::Windows][selection % 2])
            .build()
            .emulation();
        let client = Client::builder()
            .emulation(emulation)
            .redirect(wreq::redirect::Policy::none())
            .timeout(DEADLINE)
            .build()
            .map_err(|_| Error::network())?;
        Ok(Self {
            client,
            state: Mutex::new(State {
                headers: device::headers(),
                token: None,
                cooldown: None,
            }),
            auth_url: AUTH_URL.into(),
            api_url: API_URL.into(),
        })
    }

    pub async fn json(&self, path: &str) -> Result<Value, Error> {
        if !path.starts_with('/')
            || path.starts_with("//")
            || path.contains(['\\', '#', '\r', '\n'])
        {
            return Err(Error::new(
                400,
                "invalid_path",
                "Expected a relative Reddit API path",
                false,
            ));
        }
        tokio::time::timeout(DEADLINE, self.request(path))
            .await
            .map_err(|_| Error::new(504, "upstream_timeout", "Reddit request timed out", true))?
    }

    async fn request(&self, path: &str) -> Result<Value, Error> {
        // Serial upstream operations keep token refresh and cooldown state consistent.
        let mut state = self.state.lock().await;
        state.ready()?;
        if state
            .token
            .as_ref()
            .is_none_or(|token| token.expires <= Instant::now())
        {
            // Also survives cancellation during authentication, preventing retry storms.
            state.pause(Error::auth(), 30);
            if let Err(error) = self.authenticate(&mut state).await {
                let delay = error.retry_after_seconds.unwrap_or(30);
                return Err(state.pause(error, delay));
            }
            state.cooldown = None;
        }
        let token = &state.token.as_ref().ok_or_else(Error::auth)?.value;
        let mut request = self
            .client
            .get(format!("{}{path}", self.api_url))
            .bearer_auth(token);
        for (key, value) in &state.headers {
            request = request.header(key, value);
        }
        let response = request.send().await.map_err(|_| Error::network())?;
        let status = response.status().as_u16();
        let delay = retry_after(&response);
        let exhausted = response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<f64>().ok())
            .is_some_and(|n| n <= 0.0);
        if status == 401 {
            state.token = None;
            return Err(state.pause(Error::auth(), 30));
        }
        if status == 429 || (status == 403 && response.headers().contains_key("retry-after")) {
            return Err(state.pause(
                Error::new(
                    429,
                    "upstream_rate_limited",
                    "Reddit rate limit reached",
                    true,
                ),
                delay,
            ));
        }
        if exhausted {
            state.pause(
                Error::new(
                    429,
                    "upstream_rate_limited",
                    "Reddit rate limit reached",
                    true,
                ),
                delay,
            );
        }
        decode(response).await
    }

    async fn authenticate(&self, state: &mut State) -> Result<(), Error> {
        let mut request = self
            .client
            .post(&self.auth_url)
            .header(
                "Authorization",
                format!("Basic {}", STANDARD.encode("ohXpoqrZYub1kg:")),
            )
            .json(&json!({"scopes": ["*", "email", "pii"]}));
        for (key, value) in &state.headers {
            request = request.header(key, value);
        }
        let response = request.send().await.map_err(|_| Error::auth())?;
        if response.status().as_u16() == 429
            || (response.status().as_u16() == 403 && response.headers().contains_key("retry-after"))
        {
            let mut error = Error::new(
                429,
                "upstream_rate_limited",
                "Reddit authentication rate limit reached",
                true,
            );
            error.retry_after_seconds = Some(retry_after(&response));
            return Err(error);
        }
        if !response.status().is_success() {
            return Err(Error::auth());
        }
        let mut additional = HashMap::new();
        for key in ["x-reddit-loid", "x-reddit-session"] {
            if let Some(value) = response.headers().get(key).and_then(|h| h.to_str().ok()) {
                additional.insert(key.to_string(), value.to_string());
            }
        }
        let token = parse_token(decode(response).await.map_err(|_| Error::auth())?)?;
        state.headers.extend(additional);
        state.token = Some(token);
        Ok(())
    }
}

fn parse_token(value: Value) -> Result<Token, Error> {
    let token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(Error::auth)?;
    let lifetime = value
        .get("expires_in")
        .and_then(Value::as_u64)
        .filter(|v| *v > 0)
        .ok_or_else(Error::auth)?;
    // Avoid subtraction underflow for short-lived tokens and Instant overflow for bad responses.
    let lifetime = lifetime.min(86_400);
    let lifetime = lifetime.saturating_sub((lifetime / 10).min(120));
    Ok(Token {
        value: token.into(),
        expires: Instant::now() + Duration::from_secs(lifetime),
    })
}

fn retry_after(response: &Response) -> u64 {
    let header = response
        .headers()
        .get("retry-after")
        .and_then(|h| h.to_str().ok());
    let delay = header
        .and_then(|s| {
            s.parse::<u64>().ok().or_else(|| {
                httpdate::parse_http_date(s)
                    .ok()
                    .and_then(|t| t.duration_since(SystemTime::now()).ok())
                    .map(|d| d.as_secs().saturating_add(1))
            })
        })
        .or_else(|| {
            response
                .headers()
                .get("x-ratelimit-reset")
                .and_then(|h| h.to_str().ok())
                .and_then(|s| s.parse::<f64>().ok())
                .filter(|n| n.is_finite() && *n > 0.0)
                .map(|n| n.ceil() as u64)
        });
    delay.unwrap_or(60).clamp(1, 86_400)
}

async fn decode(response: Response) -> Result<Value, Error> {
    let status = response.status().as_u16();
    if status == 403 {
        let bytes = read_body(response).await?;
        let reason = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v.get("reason").and_then(Value::as_str).map(str::to_owned));
        return Err(
            if matches!(
                reason.as_deref(),
                Some("private" | "quarantined" | "gated" | "banned")
            ) {
                Error::new(
                    403,
                    "content_unavailable",
                    "Reddit restricts access to this content",
                    false,
                )
            } else {
                Error::new(
                    503,
                    "upstream_unavailable",
                    "Reddit denied upstream access",
                    true,
                )
            },
        );
    }
    if !(200..300).contains(&status) {
        return Err(match status {
            404 => Error::new(404, "not_found", "Reddit resource not found", false),
            300..=399 => Error::new(
                502,
                "upstream_redirect",
                "Reddit returned an unexpected redirect",
                false,
            ),
            500..=599 => Error::new(
                503,
                "upstream_unavailable",
                "Reddit is temporarily unavailable",
                true,
            ),
            _ => Error::new(502, "upstream_error", "Reddit rejected the request", false),
        });
    }
    serde_json::from_slice(&read_body(response).await?).map_err(|_| Error::invalid())
}

async fn read_body(response: Response) -> Result<Vec<u8>, Error> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Error::network())?;
        if body.len().saturating_add(chunk.len()) > MAX_BODY {
            return Err(Error::new(
                502,
                "upstream_body_too_large",
                "Reddit response exceeded the size limit",
                false,
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests;
