//! Convert original Reddit JSON into stable, presentation-independent data.
use crate::Error;
use chrono::{DateTime, SecondsFormat};
use rdt_gateway_types::{Comment, ContentStatus, Envelope, Meta, Post};
use serde_json::Value;

const BODY_CHARS: usize = 16_000;
const RESPONSE_BYTES: usize = 512 * 1024;

fn required<'a>(v: &'a Value, field: &str) -> Result<&'a str, Error> {
    v.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::schema(format!("Missing {field}")))
}

fn timestamp(v: &Value) -> Result<String, Error> {
    let seconds = v["created_utc"]
        .as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| Error::schema("Missing created_utc"))?;
    DateTime::from_timestamp(seconds as i64, 0)
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Secs, true))
        .ok_or_else(|| Error::schema("Invalid created_utc"))
}

fn author(v: &Value) -> Option<String> {
    v["author"]
        .as_str()
        .filter(|s| !s.is_empty() && *s != "[deleted]")
        .map(str::to_owned)
}

fn status(body: &str, v: &Value) -> ContentStatus {
    if body == "[deleted]" || v["removed_by_category"] == "deleted" {
        ContentStatus::Deleted
    } else if body == "[removed]" || v.get("removed_by_category").is_some_and(|c| !c.is_null()) {
        ContentStatus::Removed
    } else {
        ContentStatus::Available
    }
}

fn body(s: &str) -> (String, bool) {
    let mut chars = s.chars();
    let limited: String = chars.by_ref().take(BODY_CHARS).collect();
    (limited, chars.next().is_some())
}

fn score(v: &Value) -> Option<i64> {
    if v["score_hidden"].as_bool() == Some(true) || v["hide_score"].as_bool() == Some(true) {
        None
    } else {
        v["score"].as_i64()
    }
}

fn meta(fetched_at: &str, cursor: Option<String>) -> Meta {
    Meta {
        fetched_at: fetched_at.into(),
        next_cursor: cursor,
        truncated: false,
        truncation_reasons: vec![],
    }
}

fn reason(meta: &mut Meta, why: &str) {
    meta.truncated = true;
    if !meta.truncation_reasons.iter().any(|s| s == why) {
        meta.truncation_reasons.push(why.into());
    }
}

pub fn post(v: &Value) -> Result<Post, Error> {
    let original = match v.get("selftext") {
        Some(Value::String(text)) => text.as_str(),
        None if v["is_self"] != true => "",
        _ => return Err(Error::schema("Missing or invalid selftext")),
    };
    let (body_markdown, body_truncated) = body(original);
    let path = required(v, "permalink")?;
    if !path.starts_with('/') || path.starts_with("//") {
        return Err(Error::schema("Invalid permalink"));
    }
    Ok(Post {
        id: required(v, "id")?.into(),
        permalink: format!("https://www.reddit.com{path}"),
        subreddit: required(v, "subreddit")?.into(),
        author: author(v),
        title: required(v, "title")?.into(),
        body_markdown,
        body_truncated,
        url: v["url"].as_str().map(str::to_owned),
        score: score(v),
        num_comments: v["num_comments"].as_u64(),
        created_at: timestamp(v)?,
        nsfw: v["over_18"].as_bool().unwrap_or(false),
        spoiler: v["spoiler"].as_bool().unwrap_or(false),
        content_status: status(original, v),
    })
}

fn children(v: &Value) -> Result<&Vec<Value>, Error> {
    if v["kind"].as_str() != Some("Listing") {
        return Err(Error::schema("Expected Reddit Listing"));
    }
    v["data"]["children"]
        .as_array()
        .ok_or_else(|| Error::schema("Missing listing children"))
}

pub fn listing(v: &Value, fetched_at: &str, limit: usize) -> Result<Envelope<Vec<Post>>, Error> {
    let entries = children(v)?;
    let cursor = match v["data"].get("after") {
        None | Some(Value::Null) => None,
        Some(Value::String(cursor))
            if cursor.strip_prefix("t3_").is_some_and(|id| {
                !id.is_empty() && id.len() <= 16 && id.bytes().all(|b| b.is_ascii_alphanumeric())
            }) =>
        {
            Some(cursor.clone())
        }
        _ => return Err(Error::schema("Invalid listing cursor")),
    };
    let mut out = Envelope {
        data: Vec::new(),
        meta: meta(fetched_at, cursor),
    };
    for entry in entries.iter().take(limit) {
        if entry["kind"] != "t3" {
            return Err(Error::schema("Expected a post listing"));
        }
        let item = post(&entry["data"])?;
        if item.body_truncated {
            reason(&mut out.meta, "body_limit");
        }
        out.data.push(item);
    }
    if entries.len() > limit {
        reason(&mut out.meta, "limit");
        out.meta.next_cursor = out.data.last().map(|p| format!("t3_{}", p.id));
    }
    while serde_json::to_vec(&out)
        .map_err(|_| Error::schema("Cannot encode posts"))?
        .len()
        > RESPONSE_BYTES
    {
        out.data.pop();
        if out.data.is_empty() {
            return Err(Error::schema("Post exceeds response size limit"));
        }
        reason(&mut out.meta, "response_limit");
        out.meta.next_cursor = out.data.last().map(|p| format!("t3_{}", p.id));
    }
    Ok(out)
}

pub fn single(v: &Value, fetched_at: &str) -> Result<Envelope<Post>, Error> {
    let first = v
        .as_array()
        .and_then(|a| a.first())
        .ok_or_else(|| Error::schema("Expected post response"))?;
    let entries = children(first)?;
    let entry = entries
        .first()
        .filter(|e| e["kind"] == "t3")
        .ok_or_else(|| Error::schema("Post missing from response"))?;
    let data = post(&entry["data"])?;
    let mut meta = meta(fetched_at, None);
    if data.body_truncated {
        reason(&mut meta, "body_limit");
    }
    let out = Envelope { data, meta };
    if serde_json::to_vec(&out)
        .map_err(|_| Error::schema("Cannot encode post"))?
        .len()
        > RESPONSE_BYTES
    {
        return Err(Error::schema("Post exceeds response size limit"));
    }
    Ok(out)
}

pub fn comments(
    v: &Value,
    fetched_at: &str,
    depth: usize,
    limit: usize,
) -> Result<Envelope<Vec<Comment>>, Error> {
    let listing = v
        .as_array()
        .and_then(|a| a.get(1))
        .ok_or_else(|| Error::schema("Expected comments response"))?;
    let mut metadata = meta(fetched_at, None);
    let mut remaining = limit;
    let data = walk(children(listing)?, depth, &mut remaining, &mut metadata)?;
    let mut out = Envelope {
        data,
        meta: metadata,
    };
    while serde_json::to_vec(&out)
        .map_err(|_| Error::schema("Cannot encode comments"))?
        .len()
        > RESPONSE_BYTES
    {
        remove_last(&mut out.data);
        reason(&mut out.meta, "response_limit");
    }
    Ok(out)
}

fn remove_last(comments: &mut Vec<Comment>) {
    if let Some(last) = comments.last_mut() {
        if last.replies.is_empty() {
            comments.pop();
        } else {
            remove_last(&mut last.replies);
        }
    }
}

fn walk(
    entries: &[Value],
    depth: usize,
    remaining: &mut usize,
    meta: &mut Meta,
) -> Result<Vec<Comment>, Error> {
    let mut result = Vec::new();
    for entry in entries {
        if entry["kind"] == "more" {
            reason(meta, "more");
            continue;
        }
        if entry["kind"] != "t1" {
            return Err(Error::schema("Unexpected comment kind"));
        }
        if depth == 0 {
            reason(meta, "depth");
            continue;
        }
        if *remaining == 0 {
            reason(meta, "limit");
            continue;
        }
        let v = &entry["data"];
        let original = required(v, "body")?;
        let (body_markdown, body_truncated) = body(original);
        if body_truncated {
            reason(meta, "body_limit");
        }
        *remaining -= 1;
        let replies = match v.get("replies") {
            None | Some(Value::Null) => vec![],
            Some(Value::String(s)) if s.is_empty() => vec![],
            Some(replies) => walk(children(replies)?, depth - 1, remaining, meta)?,
        };
        result.push(Comment {
            id: required(v, "id")?.into(),
            parent_id: required(v, "parent_id")?.into(),
            author: author(v),
            body_markdown,
            body_truncated,
            score: score(v),
            created_at: timestamp(v)?,
            content_status: status(original, v),
            replies,
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn comment(id: &str, replies: Value) -> Value {
        json!({"kind":"t1","data":{"id":id,"parent_id":"t3_abc","author":"[deleted]","body":"**markdown**","score":3,"score_hidden":true,"created_utc":1700000000,"replies":replies}})
    }
    fn listing(children: Vec<Value>) -> Value {
        json!({"kind":"Listing","data":{"children":children}})
    }
    #[test]
    fn comment_budget_is_global_and_more_is_explicit() {
        let v = json!([
            {},
            listing(vec![
                comment("a", listing(vec![comment("b", json!(""))])),
                comment("c", json!("")),
                json!({"kind":"more","data":{}})
            ])
        ]);
        let out = comments(&v, "now", 3, 2).unwrap();
        assert_eq!(out.data.len(), 1);
        assert_eq!(out.data[0].replies.len(), 1);
        assert_eq!(out.data[0].score, None);
        assert_eq!(out.data[0].author, None);
        assert_eq!(out.data[0].body_markdown, "**markdown**");
        assert_eq!(out.meta.truncation_reasons, vec!["limit", "more"]);
    }
    #[test]
    fn depth_and_unicode_body_limits_are_reported() {
        let mut c = comment("a", listing(vec![comment("b", json!(""))]));
        c["data"]["body"] = json!("猫".repeat(BODY_CHARS + 1));
        let out = comments(&json!([{}, listing(vec![c])]), "now", 1, 10).unwrap();
        assert!(out.data[0].replies.is_empty());
        assert_eq!(out.data[0].body_markdown.chars().count(), BODY_CHARS);
        assert_eq!(out.meta.truncation_reasons, vec!["body_limit", "depth"]);
    }
    #[test]
    fn malformed_comment_is_not_silently_an_empty_result() {
        assert!(comments(&json!([{}, {}]), "now", 1, 10).is_err());
        assert!(comments(
            &json!([{}, listing(vec![json!({"kind":"wat"})])]),
            "now",
            1,
            10
        )
        .is_err());
    }
    #[test]
    fn deleted_and_removed_are_distinct() {
        assert_eq!(status("[deleted]", &json!({})), ContentStatus::Deleted);
        assert_eq!(
            status("", &json!({"removed_by_category":"deleted"})),
            ContentStatus::Deleted
        );
        assert_eq!(status("[removed]", &json!({})), ContentStatus::Removed);
    }
    fn post_entry(id: &str) -> Value {
        json!({"kind":"t3","data":{
            "id":id,"permalink":format!("/r/rust/comments/{id}/example/"),
            "subreddit":"rust","author":"alice","title":"A Rust example",
            "is_self":true,"selftext":"**Markdown**\n\n[reference](https://example.com/?a=1&b=2)",
            "selftext_html":"&lt;p&gt;Do not parse this&lt;/p&gt;",
            "url":"https://example.com/?a=1&b=2","score":12345,"num_comments":67,
            "created_utc":1700000000.0,"over_18":false,"spoiler":true,
            "removed_by_category":null
        }})
    }

    #[test]
    fn post_preserves_markdown_numeric_values_and_original_urls() {
        let source = post_entry("abc123");
        let out = single(
            &json!([listing(vec![source.clone()]), listing(vec![])]),
            "2026-10-05T00:00:00Z",
        )
        .unwrap();
        assert_eq!(out.data.body_markdown, source["data"]["selftext"]);
        assert_eq!(out.data.score, Some(12345));
        assert_eq!(out.data.num_comments, Some(67));
        assert_eq!(out.data.created_at, "2023-11-14T22:13:20Z");
        assert_eq!(
            out.data.permalink,
            "https://www.reddit.com/r/rust/comments/abc123/example/"
        );
        assert_eq!(
            out.data.url.as_deref(),
            Some("https://example.com/?a=1&b=2")
        );
        assert_eq!(out.data.content_status, ContentStatus::Available);
        assert!(out.data.spoiler);
        assert!(!out.meta.truncated);
        assert_eq!(out.meta.fetched_at, "2026-10-05T00:00:00Z");
    }

    #[test]
    fn post_requires_selftext_for_self_posts_and_rejects_malformed_values() {
        let mut entry = post_entry("abc");
        for malformed in [Value::Null, json!(42), json!({})] {
            entry["data"]["selftext"] = malformed;
            assert!(post(&entry["data"]).is_err());
        }
        entry["data"].as_object_mut().unwrap().remove("selftext");
        assert!(post(&entry["data"]).is_err());
        entry["data"]["is_self"] = json!(false);
        assert_eq!(post(&entry["data"]).unwrap().body_markdown, "");
        entry["data"]["removed_by_category"] = json!("deleted");
        let out = post(&entry["data"]).unwrap();
        assert_eq!(out.content_status, ContentStatus::Deleted);
        entry["data"]["hide_score"] = json!(true);
        assert_eq!(post(&entry["data"]).unwrap().score, None);
    }

    #[test]
    fn listing_cursor_must_be_usable_and_local_count_limit_keeps_last_returned_id() {
        let mut source = listing(vec![post_entry("a"), post_entry("b"), post_entry("c")]);
        for malformed in [json!(42), json!({}), json!(""), json!("bad"), json!("t3_")] {
            source["data"]["after"] = malformed;
            assert!(super::listing(&source, "now", 3).is_err());
        }
        source["data"]["after"] = Value::Null;
        assert_eq!(
            super::listing(&source, "now", 3).unwrap().meta.next_cursor,
            None
        );
        source["data"]["after"] = json!("t3_z");
        assert_eq!(
            super::listing(&source, "now", 3)
                .unwrap()
                .meta
                .next_cursor
                .as_deref(),
            Some("t3_z")
        );
        let out = super::listing(&source, "now", 2).unwrap();
        assert_eq!(
            out.data.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(out.meta.next_cursor.as_deref(), Some("t3_b"));
        assert_eq!(out.meta.truncation_reasons, vec!["limit"]);
    }

    #[test]
    fn listing_byte_budget_is_valid_json_and_resumes_after_last_retained_post() {
        let entries = (0..12)
            .map(|n| {
                let mut entry = post_entry(&n.to_string());
                entry["data"]["selftext"] = json!("猫".repeat(BODY_CHARS));
                entry
            })
            .collect();
        let mut source = listing(entries);
        source["data"]["after"] = json!("t3_z");
        let out = super::listing(&source, "now", 100).unwrap();
        let encoded = serde_json::to_vec(&out).unwrap();
        assert!(encoded.len() <= RESPONSE_BYTES);
        assert_eq!(
            serde_json::from_slice::<Envelope<Vec<Post>>>(&encoded).unwrap(),
            out
        );
        assert!(!out.data.is_empty() && out.data.len() < 12);
        assert_eq!(out.meta.truncation_reasons, vec!["response_limit"]);
        assert_eq!(
            out.meta.next_cursor,
            Some(format!("t3_{}", out.data.last().unwrap().id))
        );
        assert_eq!(
            out.data.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
            (0..out.data.len())
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
        );
        assert!(out.data.iter().all(|p| !p.body_truncated));
    }

    #[test]
    fn comment_byte_budget_keeps_a_depth_first_prefix_and_reports_omissions() {
        let mut input_order = Vec::new();
        let roots = (0..2)
            .map(|root| {
                let root_id = format!("a{root}");
                input_order.push(root_id.clone());
                let replies = (0..6)
                    .map(|child| {
                        let id = format!("b{root}{child}");
                        input_order.push(id.clone());
                        let mut entry = comment(&id, json!(""));
                        entry["data"]["body"] = json!("猫".repeat(BODY_CHARS));
                        entry
                    })
                    .collect();
                let mut entry = comment(&root_id, listing(replies));
                entry["data"]["body"] = json!("猫".repeat(BODY_CHARS));
                entry
            })
            .collect();
        let out = comments(
            &json!([listing(vec![post_entry("abc")]), listing(roots)]),
            "now",
            3,
            50,
        )
        .unwrap();
        let encoded = serde_json::to_vec(&out).unwrap();
        assert!(encoded.len() <= RESPONSE_BYTES);
        assert_eq!(
            serde_json::from_slice::<Envelope<Vec<Comment>>>(&encoded).unwrap(),
            out
        );
        let returned: Vec<_> = out
            .data
            .iter()
            .flat_map(|c| {
                std::iter::once(c.id.clone()).chain(c.replies.iter().map(|r| r.id.clone()))
            })
            .collect();
        assert!(!returned.is_empty() && returned.len() < input_order.len());
        assert_eq!(returned, input_order[..returned.len()]);
        assert_eq!(out.meta.truncation_reasons, vec!["response_limit"]);
        assert_eq!(out.meta.next_cursor, None);
    }
}
