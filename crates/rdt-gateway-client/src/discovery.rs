//! Community discovery and public user activity.
use crate::{normalize, Client, Error, ListOptions, Transport};
use rdt_gateway_types::{Comment, Envelope, Meta, Post, Subreddit, SubredditRule, User, WikiPage};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Default)]
pub struct DiscoveryOptions {
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

type Query = Vec<(String, String)>;

pub(crate) fn name(value: &str, user: bool) -> Result<&str, Error> {
    let prefix = if user { "u/" } else { "r/" };
    let value = value.strip_prefix(prefix).unwrap_or(value);
    if value.is_empty()
        || value.len() > 32
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || (user && b == b'-'))
    {
        return Err(Error::invalid("Invalid subreddit or user name"));
    }
    Ok(value)
}

fn limit(value: Option<u32>) -> Result<usize, Error> {
    let n = value.unwrap_or(20);
    if !(1..=100).contains(&n) {
        return Err(Error::invalid("limit must be between 1 and 100"));
    }
    Ok(n as usize)
}

fn cursor(value: &str, kind: &str) -> bool {
    value.strip_prefix(kind).is_some_and(|id| {
        !id.is_empty() && id.len() <= 16 && id.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

fn query(limit: usize, after: Option<String>, kind: &str) -> Result<Query, Error> {
    let mut query = vec![
        ("raw_json".into(), "1".into()),
        ("limit".into(), limit.to_string()),
    ];
    if let Some(after) = after {
        if !cursor(&after, kind) {
            return Err(Error::invalid("Invalid cursor"));
        }
        query.push(("after".into(), after));
    }
    Ok(query)
}

fn metadata(fetched_at: &str) -> Meta {
    Meta {
        fetched_at: fetched_at.into(),
        requests: 1,
        ..Default::default()
    }
}

fn next(value: &Value, kind: &str) -> Result<Option<String>, Error> {
    match value["data"].get("after") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if cursor(s, kind) => Ok(Some(s.clone())),
        _ => Err(Error::schema("Invalid listing cursor")),
    }
}

fn bound<T: Serialize>(out: &mut Envelope<Vec<T>>, id: impl Fn(&T) -> String) -> Result<(), Error> {
    while serde_json::to_vec(out)
        .map_err(|_| Error::schema("Cannot encode response"))?
        .len()
        > 512 * 1024
    {
        out.data.pop();
        if out.data.is_empty() {
            return Err(Error::schema("Item exceeds response size limit"));
        }
        normalize::reason(&mut out.meta, "response_limit");
        out.meta.next_cursor = out.data.last().map(&id);
    }
    Ok(())
}

fn subreddit(v: &Value) -> Result<Subreddit, Error> {
    let display_name = normalize::required(v, "display_name")?;
    name(display_name, false).map_err(|_| Error::schema("Invalid subreddit name"))?;
    let (description_markdown, body_truncated) = normalize::body(
        v["description"]
            .as_str()
            .or_else(|| v["public_description"].as_str())
            .unwrap_or_default(),
    );
    Ok(Subreddit {
        id: normalize::required(v, "id")?.into(),
        name: display_name.into(),
        permalink: format!("https://www.reddit.com/r/{display_name}/"),
        title: v["title"].as_str().unwrap_or_default().into(),
        description_markdown,
        body_truncated,
        subscribers: v["subscribers"].as_u64(),
        nsfw: v["over18"].as_bool().unwrap_or(false),
    })
}

impl<T: Transport> Client<T> {
    pub async fn search_subreddits(
        &self,
        text: &str,
        options: DiscoveryOptions,
    ) -> Result<Envelope<Vec<Subreddit>>, Error> {
        if text.trim().is_empty() || text.chars().count() > 512 {
            return Err(Error::invalid("q must contain 1–512 characters"));
        }
        let limit = limit(options.limit)?;
        let mut query = query(limit, options.cursor, "t5_")?;
        query.push(("q".into(), text.into()));
        let raw = self.raw_get("/subreddits/search.json", &query).await?;
        let entries = normalize::children(&raw.data)?;
        let mut out = Envelope {
            data: Vec::new(),
            meta: metadata(&raw.fetched_at),
        };
        out.meta.next_cursor = next(&raw.data, "t5_")?;
        for entry in entries.iter().take(limit) {
            if entry["kind"] != "t5" {
                return Err(Error::schema("Expected subreddit"));
            }
            let item = subreddit(&entry["data"])?;
            if item.body_truncated {
                normalize::reason(&mut out.meta, "body_limit");
            }
            out.data.push(item);
        }
        if entries.len() > limit {
            normalize::reason(&mut out.meta, "limit");
            out.meta.next_cursor = out.data.last().map(|s| format!("t5_{}", s.id));
        }
        bound(&mut out, |s| format!("t5_{}", s.id))?;
        Ok(out)
    }

    pub async fn subreddit_info(&self, value: &str) -> Result<Envelope<Subreddit>, Error> {
        let name = name(value, false)?;
        let raw = self
            .raw_get(
                &format!("/r/{name}/about.json"),
                &[("raw_json".into(), "1".into())],
            )
            .await?;
        if raw.data["kind"] != "t5" {
            return Err(Error::schema("Expected subreddit"));
        }
        let data = subreddit(&raw.data["data"])?;
        let mut meta = metadata(&raw.fetched_at);
        if data.body_truncated {
            normalize::reason(&mut meta, "body_limit");
        }
        Ok(Envelope { data, meta })
    }

    pub async fn subreddit_wiki(
        &self,
        value: &str,
        page: &str,
    ) -> Result<Envelope<WikiPage>, Error> {
        let name = name(value, false)?;
        if page.len() > 256
            || page.split('/').any(|p| {
                p.is_empty()
                    || !p
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            })
        {
            return Err(Error::invalid("Invalid wiki page"));
        }
        let raw = self
            .raw_get(
                &format!("/r/{name}/wiki/{page}.json"),
                &[("raw_json".into(), "1".into())],
            )
            .await?;
        if raw.data["kind"] != "wikipage" {
            return Err(Error::schema("Expected wiki page"));
        }
        let (body_markdown, body_truncated) =
            normalize::body(normalize::required(&raw.data["data"], "content_md")?);
        let mut meta = metadata(&raw.fetched_at);
        if body_truncated {
            normalize::reason(&mut meta, "body_limit");
        }
        Ok(Envelope {
            data: WikiPage {
                subreddit: name.into(),
                page: page.into(),
                permalink: format!("https://www.reddit.com/r/{name}/wiki/{page}"),
                body_markdown,
                body_truncated,
            },
            meta,
        })
    }

    pub async fn subreddit_rules(
        &self,
        value: &str,
    ) -> Result<Envelope<Vec<SubredditRule>>, Error> {
        let name = name(value, false)?;
        let raw = self
            .raw_get(
                &format!("/r/{name}/about/rules.json"),
                &[("raw_json".into(), "1".into())],
            )
            .await?;
        let rules = raw.data["rules"]
            .as_array()
            .ok_or_else(|| Error::schema("Missing rules"))?;
        let mut out = Envelope {
            data: Vec::new(),
            meta: metadata(&raw.fetched_at),
        };
        for v in rules {
            let (description_markdown, body_truncated) =
                normalize::body(normalize::required(v, "description")?);
            if body_truncated {
                normalize::reason(&mut out.meta, "body_limit");
            }
            out.data.push(SubredditRule {
                name: normalize::required(v, "short_name")?.into(),
                description_markdown,
                body_truncated,
                kind: normalize::required(v, "kind")?.into(),
                permalink: format!("https://www.reddit.com/r/{name}/about/rules"),
            });
        }
        // Rules have no upstream pagination. Report a bounded partial result without inventing a cursor.
        bound(&mut out, |_| String::new())?;
        out.meta.next_cursor = None;
        Ok(out)
    }

    pub async fn user_info(&self, value: &str) -> Result<Envelope<User>, Error> {
        let requested = name(value, true)?;
        let raw = self
            .raw_get(
                &format!("/user/{requested}/about.json"),
                &[("raw_json".into(), "1".into())],
            )
            .await?;
        if raw.data["kind"] != "t2" {
            return Err(Error::schema("Expected user"));
        }
        let v = &raw.data["data"];
        let username = normalize::required(v, "name")?;
        name(username, true).map_err(|_| Error::schema("Invalid user name"))?;
        let (description_markdown, body_truncated) = normalize::body(
            v["subreddit"]["public_description"]
                .as_str()
                .unwrap_or_default(),
        );
        let mut meta = metadata(&raw.fetched_at);
        if body_truncated {
            normalize::reason(&mut meta, "body_limit");
        }
        Ok(Envelope {
            data: User {
                name: username.into(),
                permalink: format!("https://www.reddit.com/user/{username}/"),
                description_markdown,
                body_truncated,
                link_karma: v["link_karma"].as_i64(),
                comment_karma: v["comment_karma"].as_i64(),
                created_at: v
                    .get("created_utc")
                    .filter(|v| !v.is_null())
                    .map(|_| normalize::timestamp(v))
                    .transpose()?,
                suspended: v["is_suspended"].as_bool().unwrap_or(false),
            },
            meta,
        })
    }

    pub async fn user_posts(
        &self,
        value: &str,
        options: ListOptions,
    ) -> Result<Envelope<Vec<Post>>, Error> {
        let name = name(value, true)?;
        let (query, limit) = user_query(options, "t3_")?;
        let raw = self
            .raw_get(&format!("/user/{name}/submitted.json"), &query)
            .await?;
        normalize::listing(&raw.data, &raw.fetched_at, limit)
    }

    pub async fn user_comments(
        &self,
        value: &str,
        options: ListOptions,
    ) -> Result<Envelope<Vec<Comment>>, Error> {
        let name = name(value, true)?;
        let (query, limit) = user_query(options, "t1_")?;
        let raw = self
            .raw_get(&format!("/user/{name}/comments.json"), &query)
            .await?;
        let next_cursor = next(&raw.data, "t1_")?;
        let mut out = normalize::comments(
            &serde_json::json!([null, raw.data]),
            &raw.fetched_at,
            1,
            limit,
        )?;
        out.meta.next_cursor = if out
            .meta
            .truncation_reasons
            .iter()
            .any(|s| s == "limit" || s == "response_limit")
        {
            out.data.last().map(|c| format!("t1_{}", c.id))
        } else {
            next_cursor
        };
        Ok(out)
    }
}

fn user_query(options: ListOptions, kind: &str) -> Result<(Query, usize), Error> {
    let sort = options.sort.as_deref().unwrap_or("new");
    if !["hot", "new", "top", "controversial"].contains(&sort) {
        return Err(Error::invalid("Unsupported user listing sort"));
    }
    let limit = limit(options.limit)?;
    let mut query = query(limit, options.cursor, kind)?;
    query.push(("sort".into(), sort.into()));
    if let Some(time) = options.time {
        if !["top", "controversial"].contains(&sort)
            || !["hour", "day", "week", "month", "year", "all"].contains(&time.as_str())
        {
            return Err(Error::invalid(
                "time requires top/controversial and hour/day/week/month/year/all",
            ));
        }
        query.push(("t".into(), time));
    }
    Ok((query, limit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Clone)]
    struct Fixture {
        path: &'static str,
        query: Vec<(&'static str, &'static str)>,
        data: Value,
    }
    impl Transport for Fixture {
        async fn raw_get(
            &self,
            path: &str,
            query: &[(String, String)],
        ) -> Result<crate::RawResponse, Error> {
            assert_eq!(path, self.path);
            let actual: std::collections::BTreeMap<_, _> = query
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            assert_eq!(actual, self.query.iter().copied().collect());
            Ok(crate::RawResponse {
                data: self.data.clone(),
                fetched_at: "2026-10-06T00:00:00Z".into(),
            })
        }
    }
    fn client(
        path: &'static str,
        query: Vec<(&'static str, &'static str)>,
        data: Value,
    ) -> Client<Fixture> {
        Client::with_transport(Fixture { path, query, data })
    }
    fn community(id: &str) -> Value {
        json!({"kind":"t5", "data":{"id":id,"display_name":"rust","title":"Rust","description":"**markdown**","subscribers":12,"over18":true}})
    }
    fn listing(children: Vec<Value>, after: Value) -> Value {
        json!({"kind":"Listing","data":{"children":children,"after":after}})
    }

    #[tokio::test]
    async fn discovery_maps_queries_and_preserves_pagination() {
        let result = client(
            "/subreddits/search.json",
            vec![
                ("raw_json", "1"),
                ("limit", "1"),
                ("q", "rust & nix"),
                ("after", "t5_prev"),
            ],
            listing(vec![community("a"), community("b")], json!("t5_next")),
        )
        .search_subreddits(
            "rust & nix",
            DiscoveryOptions {
                limit: Some(1),
                cursor: Some("t5_prev".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.meta.next_cursor.as_deref(), Some("t5_a"));
        assert_eq!(result.meta.truncation_reasons, vec!["limit"]);
        assert_eq!(result.meta.requests, 1);
        assert_eq!(result.data[0].permalink, "https://www.reddit.com/r/rust/");
        assert_eq!(result.data[0].description_markdown, "**markdown**");
        assert!(result.data[0].nsfw);
        let result = client(
            "/r/rust/about.json",
            vec![("raw_json", "1")],
            community("a"),
        )
        .subreddit_info("r/rust")
        .await
        .unwrap();
        assert_eq!(result.data.subscribers, Some(12));
    }

    #[tokio::test]
    async fn wiki_and_rules_preserve_markdown_and_report_truncation() {
        let result = client(
            "/r/rust/wiki/faq/install.json",
            vec![("raw_json", "1")],
            json!({"kind":"wikipage","data":{"content_md":"猫".repeat(16001)}}),
        )
        .subreddit_wiki("rust", "faq/install")
        .await
        .unwrap();
        assert_eq!(result.data.body_markdown.chars().count(), 16000);
        assert_eq!(result.meta.truncation_reasons, vec!["body_limit"]);
        assert_eq!(
            result.data.permalink,
            "https://www.reddit.com/r/rust/wiki/faq/install"
        );
        let result = client(
            "/r/rust/about/rules.json",
            vec![("raw_json", "1")],
            json!({"rules":[{"short_name":"Be kind","description":"**Please**","kind":"all"}]}),
        )
        .subreddit_rules("rust")
        .await
        .unwrap();
        assert_eq!(result.data[0].description_markdown, "**Please**");
        assert_eq!(result.meta.next_cursor, None);
    }

    #[tokio::test]
    async fn user_info_accepts_hyphenated_names_and_missing_suspended_fields() {
        let result = client(
            "/user/some-user/about.json",
            vec![("raw_json", "1")],
            json!({"kind":"t2","data":{"name":"some-user","is_suspended":true}}),
        )
        .user_info("u/some-user")
        .await
        .unwrap();
        assert!(result.data.suspended);
        assert_eq!(result.data.created_at, None);
        assert_eq!(result.data.link_karma, None);
        assert_eq!(
            result.data.permalink,
            "https://www.reddit.com/user/some-user/"
        );
    }

    #[tokio::test]
    async fn user_history_uses_kind_specific_cursors_and_flat_comments() {
        let post = json!({"kind":"t3","data":{"id":"a","permalink":"/r/rust/comments/a/title/","subreddit":"rust","title":"Title","selftext":"body","created_utc":1700000000}});
        let result = client(
            "/user/some-user/submitted.json",
            vec![("raw_json", "1"), ("sort", "new"), ("limit", "20")],
            listing(vec![post], json!("t3_next")),
        )
        .user_posts("u/some-user", ListOptions::default())
        .await
        .unwrap();
        assert_eq!(result.data[0].id, "a");
        let comment = |id| json!({"kind":"t1","data":{"id":id,"parent_id":"t3_a","body":"[deleted]","author":"[deleted]","created_utc":1700000000}});
        let result = client(
            "/user/some-user/comments.json",
            vec![
                ("raw_json", "1"),
                ("sort", "top"),
                ("t", "week"),
                ("limit", "1"),
                ("after", "t1_prev"),
            ],
            listing(vec![comment("b"), comment("c")], json!("t1_next")),
        )
        .user_comments(
            "some-user",
            ListOptions {
                sort: Some("top".into()),
                time: Some("week".into()),
                limit: Some(1),
                cursor: Some("t1_prev".into()),
            },
        )
        .await
        .unwrap();
        assert_eq!(result.meta.next_cursor.as_deref(), Some("t1_b"));
        assert_eq!(
            result.data[0].content_status,
            rdt_gateway_types::ContentStatus::Deleted
        );
        assert!(result.data[0].replies.is_empty());
    }

    #[tokio::test]
    async fn invalid_arguments_fail_before_transport() {
        let c = client("never", vec![], Value::Null);
        for invalid in ["../x", "r/../../x", "a?x", "a/b", ""] {
            assert!(matches!(
                c.subreddit_info(invalid).await,
                Err(Error::InvalidArgument(_))
            ));
        }
        for invalid in ["../index", "/index", "index/", "a//b", "a.b", ""] {
            assert!(matches!(
                c.subreddit_wiki("rust", invalid).await,
                Err(Error::InvalidArgument(_))
            ));
        }
        assert!(matches!(
            c.search_subreddits(
                "x",
                DiscoveryOptions {
                    cursor: Some("t3_a".into()),
                    ..Default::default()
                }
            )
            .await,
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            c.user_comments(
                "alice",
                ListOptions {
                    cursor: Some("t3_a".into()),
                    ..Default::default()
                }
            )
            .await,
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            c.user_posts(
                "alice",
                ListOptions {
                    time: Some("week".into()),
                    ..Default::default()
                }
            )
            .await,
            Err(Error::InvalidArgument(_))
        ));
    }

    #[tokio::test]
    async fn malformed_upstream_is_not_an_empty_success() {
        assert!(matches!(
            client(
                "/subreddits/search.json",
                vec![("raw_json", "1"), ("limit", "20"), ("q", "rust")],
                listing(vec![community("a")], json!("t3_bad"))
            )
            .search_subreddits("rust", DiscoveryOptions::default())
            .await,
            Err(Error::InvalidSchema(_))
        ));
        assert!(matches!(
            client(
                "/r/rust/about/rules.json",
                vec![("raw_json", "1")],
                json!({})
            )
            .subreddit_rules("rust")
            .await,
            Err(Error::InvalidSchema(_))
        ));
    }
}
