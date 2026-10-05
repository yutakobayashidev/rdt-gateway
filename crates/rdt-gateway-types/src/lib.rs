//! SDK domain models and shared gateway error representations.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Envelope<T> {
    pub data: T,
    pub meta: Meta,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Meta {
    pub fetched_at: String,
    pub next_cursor: Option<String>,
    pub truncated: bool,
    pub truncation_reasons: Vec<String>,
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
