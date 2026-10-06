//! SDK domain models and shared gateway error representations.
use serde::{Deserialize, Serialize};

/// Original Reddit JSON and acquisition time, retained on cache hits.
#[derive(Debug, Serialize)]
pub struct RawResponse {
    pub data: serde_json::Value,
    pub fetched_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Envelope<T> {
    pub data: T,
    pub meta: Meta,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Meta {
    pub fetched_at: String,
    pub next_cursor: Option<String>,
    pub truncated: bool,
    pub truncation_reasons: Vec<String>,
    /// SDK content fetches, including cache hits; authentication is not counted.
    #[serde(default)]
    pub requests: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial_error: Option<ApiError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ContentStatus {
    Available,
    Deleted,
    Removed,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Post {
    pub id: String,
    pub permalink: String,
    pub subreddit: String,
    pub author: Option<String>,
    pub title: String,
    pub body_markdown: String,
    pub body_truncated: bool,
    pub url: Option<String>,
    pub score: Option<i64>,
    pub num_comments: Option<u64>,
    pub created_at: String,
    pub nsfw: bool,
    pub spoiler: bool,
    pub content_status: ContentStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Comment {
    pub id: String,
    pub parent_id: String,
    /// Canonical citation URL when supplied by Reddit.
    pub permalink: Option<String>,
    pub author: Option<String>,
    pub body_markdown: String,
    pub body_truncated: bool,
    pub score: Option<i64>,
    pub created_at: String,
    pub content_status: ContentStatus,
    pub replies: Vec<Comment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Thread {
    pub post: Post,
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ErrorEnvelope {
    pub error: ApiError,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Subreddit {
    pub id: String,
    pub name: String,
    pub permalink: String,
    pub title: String,
    pub description_markdown: String,
    pub body_truncated: bool,
    pub subscribers: Option<u64>,
    pub nsfw: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WikiPage {
    pub subreddit: String,
    pub page: String,
    pub permalink: String,
    pub body_markdown: String,
    pub body_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SubredditRule {
    pub name: String,
    pub description_markdown: String,
    pub body_truncated: bool,
    pub kind: String,
    pub permalink: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct User {
    pub name: String,
    pub permalink: String,
    pub description_markdown: String,
    pub body_truncated: bool,
    pub link_karma: Option<i64>,
    pub comment_karma: Option<i64>,
    pub created_at: Option<String>,
    pub suspended: bool,
}
