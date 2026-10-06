use crate::{normalize, Client, CommentOptions, Envelope, Error, Thread, Transport};
use rdt_gateway_types::ApiError;
use serde_json::{json, Value};
use std::collections::HashSet;

pub(crate) fn post_id(input: &str) -> Result<String, Error> {
    let id = if input.contains("://") {
        let url = url::Url::parse(input).map_err(|_| Error::invalid("Invalid Reddit post URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
        {
            return Err(Error::invalid("Expected a Reddit post URL"));
        }
        let mut parts: Vec<_> = url.path_segments().into_iter().flatten().collect();
        if parts.last() == Some(&"") {
            parts.pop();
        }
        match url.host_str() {
            Some("redd.it") if parts.len() == 1 => parts[0].to_owned(),
            Some(
                "reddit.com" | "www.reddit.com" | "old.reddit.com" | "new.reddit.com"
                | "m.reddit.com",
            ) => match parts.as_slice() {
                ["comments", id, ..]
                | ["r", _, "comments", id, ..]
                | ["user", _, "comments", id, ..]
                | ["u", _, "comments", id, ..] => (*id).to_owned(),
                _ => return Err(Error::invalid("URL must point to a Reddit post")),
            },
            _ => return Err(Error::invalid("URL must point to reddit.com or redd.it")),
        }
    } else {
        input.strip_prefix("t3_").unwrap_or(input).to_owned()
    };
    if !valid_id(&id) {
        return Err(Error::invalid(
            "Expected a post ID, t3_ ID, or Reddit post URL",
        ));
    }
    Ok(id)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 16 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn api_error(error: Error) -> ApiError {
    match error {
        Error::Gateway {
            code,
            message,
            retryable,
            retry_after_seconds,
            ..
        } => ApiError {
            code,
            message,
            retryable,
            retry_after_seconds,
        },
        error => ApiError {
            code: "comment_expansion_failed".into(),
            message: error.to_string(),
            retryable: matches!(error, Error::Transport(_)),
            retry_after_seconds: None,
        },
    }
}

impl<T: Transport> Client<T> {
    /// Read a post and bounded comments. Expansion uses GET /api/morechildren.json.
    pub async fn read(
        &self,
        input: &str,
        options: CommentOptions,
    ) -> Result<Envelope<Thread>, Error> {
        let id = post_id(input)?;
        let sort = options.sort.as_deref().unwrap_or("confidence");
        let sort = if sort == "best" { "confidence" } else { sort };
        if !["confidence", "top", "new", "controversial", "old", "qa"].contains(&sort) {
            return Err(Error::invalid("Unsupported comment sort"));
        }
        let depth = options.depth.unwrap_or(3) as usize;
        let limit = options.limit.unwrap_or(50) as usize;
        if !(1..=8).contains(&depth) || !(1..=200).contains(&limit) {
            return Err(Error::invalid("depth must be 1–8 and limit must be 1–200"));
        }
        if !options.expand_more && options.max_requests.is_some() {
            return Err(Error::invalid("max_requests requires expand_more"));
        }
        let budget = if options.expand_more {
            options.max_requests.unwrap_or(3)
        } else {
            1
        };
        if !(1..=10).contains(&budget) {
            return Err(Error::invalid("max_requests must be 1–10"));
        }
        let mut raw = self
            .raw_get(
                &format!("/comments/{id}.json"),
                &[
                    ("raw_json".into(), "1".into()),
                    ("sort".into(), sort.into()),
                    ("limit".into(), limit.to_string()),
                ],
            )
            .await?;
        let post = normalize::single(&raw.data, &raw.fetched_at)?;
        let mut comments = normalize::comments(&raw.data, &raw.fetched_at, depth, limit)?;
        let mut requests = 1;
        let mut attempted = HashSet::new();
        let mut partial_error = None;
        while requests < budget as usize && count(&comments.data) < limit {
            let ids = pending(&raw.data[1]["data"]["children"], depth, &attempted)?;
            if ids.is_empty() {
                break;
            }
            let ids: Vec<_> = ids.into_iter().take(100).collect();
            attempted.extend(ids.iter().cloned());
            requests += 1;
            let fetched = self
                .raw_get(
                    "/api/morechildren.json",
                    &[
                        ("api_type".into(), "json".into()),
                        ("raw_json".into(), "1".into()),
                        ("link_id".into(), format!("t3_{id}")),
                        ("children".into(), ids.join(",")),
                        ("sort".into(), sort.into()),
                        ("limit_children".into(), "false".into()),
                    ],
                )
                .await;
            let result = fetched.and_then(|more| {
                let things = more.data["json"]["data"]["things"]
                    .as_array()
                    .ok_or_else(|| Error::schema("Missing morechildren things"))?;
                if more.data["json"]["errors"]
                    .as_array()
                    .is_some_and(|e| !e.is_empty())
                {
                    return Err(Error::schema("Reddit rejected comment expansion"));
                }
                // Work on a copy: malformed expansion must not discard the valid initial tree.
                let mut updated = raw.data.clone();
                merge(
                    &mut updated[1]["data"]["children"],
                    things,
                    &format!("t3_{id}"),
                )?;
                let normalized = normalize::comments(&updated, &raw.fetched_at, depth, limit)?;
                raw.data = updated;
                Ok(normalized)
            });
            match result {
                Ok(value) => comments = value,
                Err(error) => {
                    partial_error = Some(api_error(error));
                    break;
                }
            }
        }
        if options.expand_more
            && requests == budget as usize
            && count(&comments.data) < limit
            && !pending(&raw.data[1]["data"]["children"], depth, &attempted)?.is_empty()
        {
            normalize::reason(&mut comments.meta, "request_limit");
        }
        if partial_error.is_some() {
            normalize::reason(&mut comments.meta, "upstream_error");
        }
        for reason in post.meta.truncation_reasons {
            normalize::reason(&mut comments.meta, &reason);
        }
        comments.meta.requests = requests;
        comments.meta.partial_error = partial_error;
        let mut result = Envelope {
            data: Thread {
                post: post.data,
                comments: comments.data,
            },
            meta: comments.meta,
        };
        while serde_json::to_vec(&result)
            .map_err(|_| Error::schema("Cannot encode thread"))?
            .len()
            > 512 * 1024
        {
            if result.data.comments.is_empty() {
                return Err(Error::schema("Post exceeds response size limit"));
            }
            normalize::remove_last(&mut result.data.comments);
            normalize::reason(&mut result.meta, "response_limit");
        }
        Ok(result)
    }
}

fn count(comments: &[crate::Comment]) -> usize {
    comments.iter().map(|c| 1 + count(&c.replies)).sum()
}

fn pending(
    entries: &Value,
    depth: usize,
    attempted: &HashSet<String>,
) -> Result<Vec<String>, Error> {
    let mut ids = Vec::new();
    if depth == 0 {
        return Ok(ids);
    }
    if let Some(entries) = entries.as_array() {
        for entry in entries {
            if entry["kind"] == "more" {
                if let Some(children) = entry["data"]["children"].as_array() {
                    for child in children {
                        let id = child
                            .as_str()
                            .filter(|id| valid_id(id))
                            .ok_or_else(|| Error::schema("Invalid morechildren ID"))?;
                        if !attempted.contains(id) && !ids.iter().any(|v| v == id) {
                            ids.push(id.into());
                        }
                    }
                }
            } else if entry["kind"] == "t1" {
                ids.extend(pending(
                    &entry["data"]["replies"]["data"]["children"],
                    depth - 1,
                    attempted,
                )?);
            }
        }
    }
    let mut seen = HashSet::new();
    ids.retain(|id| seen.insert(id.clone()));
    Ok(ids)
}

fn contains(entries: &Value, id: &str) -> bool {
    entries.as_array().is_some_and(|entries| {
        entries.iter().any(|entry| {
            entry["kind"] == "t1"
                && (entry["data"]["id"] == id
                    || contains(&entry["data"]["replies"]["data"]["children"], id))
        })
    })
}

fn attach(entries: &mut Value, thing: &Value, parent: &str, root: &str) -> bool {
    let Some(entries) = entries.as_array_mut() else {
        return false;
    };
    if parent == root {
        insert(entries, thing);
        return true;
    }
    for entry in entries {
        if entry["kind"] != "t1" {
            continue;
        }
        if parent
            .strip_prefix("t1_")
            .is_some_and(|id| entry["data"]["id"] == id)
        {
            if !entry["data"]["replies"].is_object() {
                entry["data"]["replies"] = json!({"kind":"Listing","data":{"children":[]}});
            }
            if let Some(children) = entry["data"]["replies"]["data"]["children"].as_array_mut() {
                insert(children, thing);
                return true;
            }
        }
        if entry["data"]["replies"].is_object()
            && attach(
                &mut entry["data"]["replies"]["data"]["children"],
                thing,
                parent,
                root,
            )
        {
            return true;
        }
    }
    false
}

fn insert(entries: &mut Vec<Value>, thing: &Value) {
    let position = entries
        .iter()
        .position(|entry| {
            entry["kind"] == "more"
                && entry["data"]["children"]
                    .as_array()
                    .is_some_and(|ids| ids.iter().any(|id| id == &thing["data"]["id"]))
        })
        .unwrap_or(entries.len());
    entries.insert(position, thing.clone());
}

fn prune_more(entries: &mut Value, received: &HashSet<String>) {
    if let Some(entries) = entries.as_array_mut() {
        entries.retain_mut(|entry| {
            if entry["kind"] == "more" {
                if let Some(ids) = entry["data"]["children"].as_array_mut() {
                    let was_nonempty = !ids.is_empty();
                    ids.retain(|id| !id.as_str().is_some_and(|id| received.contains(id)));
                    return !was_nonempty || !ids.is_empty();
                }
            } else if entry["data"]["replies"].is_object() {
                prune_more(&mut entry["data"]["replies"]["data"]["children"], received);
            }
            true
        });
    }
}

fn merge(entries: &mut Value, things: &[Value], root: &str) -> Result<(), Error> {
    let mut waiting = Vec::new();
    let mut received = HashSet::new();
    for thing in things {
        let mut thing = thing.clone();
        if thing["kind"] == "t1" {
            let id = thing["data"]["id"]
                .as_str()
                .filter(|id| valid_id(id))
                .ok_or_else(|| Error::schema("Invalid expanded comment ID"))?
                .to_owned();
            received.insert(id.clone());
            if contains(entries, &id) {
                continue;
            }
            thing["data"]["replies"] = Value::String(String::new());
        } else if thing["kind"] == "more" {
            pending(&json!([thing.clone()]), 1, &HashSet::new())?;
        } else {
            return Err(Error::schema("Unexpected expanded comment kind"));
        }
        if !thing["data"]["parent_id"].is_string() {
            return Err(Error::schema("Missing comment parent"));
        }
        waiting.push(thing);
    }
    while !waiting.is_empty() {
        let before = waiting.len();
        waiting.retain(|thing| {
            if thing["kind"] == "t1" && contains(entries, thing["data"]["id"].as_str().unwrap()) {
                return false;
            }
            !attach(
                entries,
                thing,
                thing["data"]["parent_id"].as_str().unwrap(),
                root,
            )
        });
        if waiting.len() == before {
            return Err(Error::schema("Expanded comments have unresolved parents"));
        }
    }
    prune_more(entries, &received);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Mock {
        responses: Arc<Mutex<VecDeque<Result<Value, Error>>>>,
        calls: Arc<Mutex<Vec<(String, Vec<(String, String)>)>>>,
    }
    impl Transport for Mock {
        async fn raw_get(
            &self,
            path: &str,
            query: &[(String, String)],
        ) -> Result<crate::RawResponse, Error> {
            self.calls.lock().unwrap().push((path.into(), query.into()));
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected request")
                .map(|data| crate::RawResponse {
                    data,
                    fetched_at: "2026-10-06T00:00:00Z".into(),
                })
        }
    }
    fn mock(responses: Vec<Result<Value, Error>>) -> (Client<Mock>, Mock) {
        let transport = Mock {
            responses: Arc::new(Mutex::new(responses.into())),
            calls: Default::default(),
        };
        (Client::with_transport(transport.clone()), transport)
    }
    fn comment(id: &str, parent: &str) -> Value {
        json!({"kind":"t1","data":{"id":id,"parent_id":parent,"body":"hello","created_utc":1700000000,"replies":""}})
    }
    fn more(parent: &str, ids: &[&str]) -> Value {
        json!({"kind":"more","data":{"parent_id":parent,"children":ids,"count":ids.len()}})
    }
    fn initial(children: Vec<Value>) -> Value {
        json!([
            {"kind":"Listing","data":{"children":[{"kind":"t3","data":{"id":"abc","permalink":"/r/rust/comments/abc/title/","subreddit":"rust","title":"Post","selftext":"Body","created_utc":1700000000}}]}},
            {"kind":"Listing","data":{"children":children}}
        ])
    }
    fn expanded(things: Vec<Value>) -> Value {
        json!({"json":{"errors":[],"data":{"things":things}}})
    }

    #[test]
    fn references_only_accept_reddit_post_locations() {
        for reference in [
            "abc",
            "t3_abc",
            "https://redd.it/abc",
            "https://redd.it/abc/",
            "https://www.reddit.com/r/rust/comments/abc/title/?sort=new",
            "https://old.reddit.com/comments/abc",
        ] {
            assert_eq!(post_id(reference).unwrap(), "abc");
        }
        for reference in [
            "",
            "t1_abc",
            "../abc",
            "https://evil.test/comments/abc",
            "https://reddit.com.evil.test/comments/abc",
            "https://secret@reddit.com/comments/abc",
            "https://reddit.com/search?q=abc",
            "https://reddit.com:9000/comments/abc",
        ] {
            assert!(post_id(reference).is_err(), "{reference}");
        }
    }

    #[tokio::test]
    async fn read_uses_one_response_for_post_and_comments() {
        let (client, mock) = mock(vec![Ok(initial(vec![
            comment("a", "t3_abc"),
            more("t3_abc", &["b"]),
        ]))]);
        let out = client
            .read("t3_abc", CommentOptions::default())
            .await
            .unwrap();
        assert_eq!(out.data.post.id, "abc");
        assert_eq!(out.data.comments.len(), 1);
        assert_eq!(out.meta.requests, 1);
        assert_eq!(out.meta.truncation_reasons, ["more"]);
        assert_eq!(mock.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn expansion_reassembles_out_of_order_replies_and_deduplicates() {
        let (client, mock) = mock(vec![
            Ok(initial(vec![
                comment("a", "t3_abc"),
                more("t3_abc", &["b", "c"]),
            ])),
            Ok(expanded(vec![
                comment("c", "t1_b"),
                comment("b", "t3_abc"),
                comment("b", "t3_abc"),
            ])),
        ]);
        let out = client
            .read(
                "abc",
                CommentOptions {
                    expand_more: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(out.data.comments.len(), 2);
        assert_eq!(out.data.comments[1].id, "b");
        assert_eq!(out.data.comments[1].replies[0].id, "c");
        assert!(!out.meta.truncated);
        assert_eq!(out.meta.requests, 2);
        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls[1].0, "/api/morechildren.json");
        assert!(calls[1].1.contains(&("children".into(), "b,c".into())));
        assert!(calls[1].1.contains(&("link_id".into(), "t3_abc".into())));
    }

    #[tokio::test]
    async fn request_budget_and_output_limit_stop_expansion() {
        let (client, mock) = mock(vec![Ok(initial(vec![more("t3_abc", &["b"])]))]);
        let out = client
            .read(
                "abc",
                CommentOptions {
                    expand_more: true,
                    max_requests: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(out
            .meta
            .truncation_reasons
            .contains(&"request_limit".into()));
        assert_eq!(mock.calls.lock().unwrap().len(), 1);
        let (client, mock) = self::mock(vec![Ok(initial(vec![
            comment("a", "t3_abc"),
            more("t3_abc", &["b"]),
        ]))]);
        let out = client
            .comments(
                "abc",
                CommentOptions {
                    expand_more: true,
                    limit: Some(1),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(out.data.len(), 1);
        assert_eq!(mock.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn expansion_failures_preserve_initial_data_and_report_partial_failure() {
        for response in [
            Err(Error::schema("bad response")),
            Ok(expanded(vec![comment("b", "t1_missing")])),
            Ok(json!({"json":{"errors":[["BAD_ID","bad", ""]],"data":{"things":[]}}})),
        ] {
            let (client, _) = mock(vec![
                Ok(initial(vec![
                    comment("a", "t3_abc"),
                    more("t3_abc", &["b"]),
                ])),
                response,
            ]);
            let out = client
                .read(
                    "abc",
                    CommentOptions {
                        expand_more: true,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            assert_eq!(out.data.comments.len(), 1);
            assert_eq!(out.data.comments[0].id, "a");
            assert!(out.meta.partial_error.is_some());
            assert_eq!(out.meta.requests, 2);
        }
    }

    #[tokio::test]
    async fn empty_expansion_does_not_retry_the_same_ids() {
        let (client, mock) = mock(vec![
            Ok(initial(vec![more("t3_abc", &["b"])])),
            Ok(expanded(vec![])),
        ]);
        let out = client
            .read(
                "abc",
                CommentOptions {
                    expand_more: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(mock.calls.lock().unwrap().len(), 2);
        assert!(out.meta.truncated);
    }

    #[tokio::test]
    async fn expanded_comments_keep_the_placeholder_position() {
        let (client, _) = mock(vec![
            Ok(initial(vec![
                more("t3_abc", &["a"]),
                comment("b", "t3_abc"),
            ])),
            Ok(expanded(vec![comment("a", "t3_abc")])),
        ]);
        let out = client
            .read(
                "abc",
                CommentOptions {
                    expand_more: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            out.data
                .comments
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[tokio::test]
    async fn malformed_expanded_placeholder_preserves_initial_result() {
        let (client, _) = mock(vec![
            Ok(initial(vec![
                comment("a", "t3_abc"),
                more("t3_abc", &["b"]),
            ])),
            Ok(expanded(vec![
                json!({"kind":"more","data":{"parent_id":"t3_abc","children":[false]}}),
            ])),
        ]);
        let out = client
            .read(
                "abc",
                CommentOptions {
                    expand_more: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(out.data.comments[0].id, "a");
        assert!(out.meta.partial_error.is_some());
    }

    #[tokio::test]
    async fn invalid_options_never_reach_transport() {
        let (client, mock) = mock(vec![]);
        for options in [
            CommentOptions {
                max_requests: Some(2),
                ..Default::default()
            },
            CommentOptions {
                expand_more: true,
                max_requests: Some(11),
                ..Default::default()
            },
            CommentOptions {
                depth: Some(0),
                ..Default::default()
            },
        ] {
            assert!(client.read("abc", options).await.is_err());
        }
        assert!(mock.calls.lock().unwrap().is_empty());
    }
}
