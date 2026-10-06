use crate::{normalize, Client, Comment, Envelope, Error, Post, Transport};

#[derive(Debug, Default)]
pub struct SearchOptions {
    pub subreddit: Option<String>,
    pub sort: Option<String>,
    pub time: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Default)]
pub struct ListOptions {
    pub sort: Option<String>,
    pub time: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Default)]
pub struct CommentOptions {
    pub sort: Option<String>,
    pub depth: Option<u32>,
    pub limit: Option<u32>,
}

const TIMES: &[&str] = &["hour", "day", "week", "month", "year", "all"];
type Query = Vec<(String, String)>;

fn choice<'a>(
    value: Option<&'a str>,
    default: &'a str,
    allowed: &[&str],
) -> Result<&'a str, Error> {
    let value = value.unwrap_or(default);
    if !allowed.contains(&value) {
        return Err(Error::invalid(format!("Unsupported value: {value}")));
    }
    Ok(value)
}

fn number(value: Option<u32>, default: u32, max: u32) -> Result<usize, Error> {
    let value = value.unwrap_or(default);
    if value == 0 || value > max {
        return Err(Error::invalid(format!("Value must be between 1 and {max}")));
    }
    Ok(value as usize)
}

fn identifier(value: &str, subreddit: bool) -> Result<(), Error> {
    let max = if subreddit { 32 } else { 16 };
    if value.is_empty()
        || value.len() > max
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || (subreddit && c == b'_'))
    {
        return Err(Error::invalid(if subreddit {
            "subreddit must contain 1–32 letters, digits, or underscores"
        } else {
            "id must contain 1–16 letters or digits"
        }));
    }
    Ok(())
}

fn pagination(query: &mut Query, cursor: Option<String>) -> Result<(), Error> {
    if let Some(cursor) = cursor {
        identifier(
            cursor
                .strip_prefix("t3_")
                .ok_or_else(|| Error::invalid("Invalid cursor"))?,
            false,
        )?;
        query.push(("after".into(), cursor));
    }
    Ok(())
}

fn query() -> Query {
    vec![("raw_json".into(), "1".into())]
}

impl<T: Transport> Client<T> {
    pub async fn search(
        &self,
        text: &str,
        options: SearchOptions,
    ) -> Result<Envelope<Vec<Post>>, Error> {
        if text.trim().is_empty() || text.chars().count() > 512 {
            return Err(Error::invalid("q must contain 1–512 characters"));
        }
        let sort = choice(
            options.sort.as_deref(),
            "relevance",
            &["relevance", "hot", "top", "new", "comments"],
        )?;
        let time = choice(options.time.as_deref(), "all", TIMES)?;
        let limit = number(options.limit, 20, 100)?;
        let mut query = query();
        query.extend([
            ("q".into(), text.into()),
            ("sort".into(), sort.into()),
            ("t".into(), time.into()),
            ("limit".into(), limit.to_string()),
            ("type".into(), "link".into()),
        ]);
        pagination(&mut query, options.cursor)?;
        let path = if let Some(name) = options.subreddit {
            identifier(&name, true)?;
            query.push(("restrict_sr".into(), "on".into()));
            format!("/r/{name}/search.json")
        } else {
            "/search.json".into()
        };
        let raw = self.raw_get(&path, &query).await?;
        normalize::listing(&raw.data, &raw.fetched_at, limit)
    }

    pub async fn list_posts(
        &self,
        name: &str,
        options: ListOptions,
    ) -> Result<Envelope<Vec<Post>>, Error> {
        identifier(name, true)?;
        let sort = choice(options.sort.as_deref(), "hot", &["hot", "new", "top"])?;
        let time = choice(options.time.as_deref(), "all", TIMES)?;
        if options.time.is_some() && sort != "top" {
            return Err(Error::invalid("time is only supported with sort=top"));
        }
        let limit = number(options.limit, 20, 100)?;
        let mut query = query();
        query.push(("limit".into(), limit.to_string()));
        if sort == "top" {
            query.push(("t".into(), time.into()));
        }
        pagination(&mut query, options.cursor)?;
        let raw = self
            .raw_get(&format!("/r/{name}/{sort}.json"), &query)
            .await?;
        normalize::listing(&raw.data, &raw.fetched_at, limit)
    }

    pub async fn post(&self, id: &str) -> Result<Envelope<Post>, Error> {
        identifier(id, false)?;
        let mut query = query();
        query.push(("limit".into(), "1".into()));
        let raw = self
            .raw_get(&format!("/comments/{id}.json"), &query)
            .await?;
        normalize::single(&raw.data, &raw.fetched_at)
    }

    pub async fn comments(
        &self,
        id: &str,
        options: CommentOptions,
    ) -> Result<Envelope<Vec<Comment>>, Error> {
        identifier(id, false)?;
        let sort = choice(
            options.sort.as_deref(),
            "confidence",
            &["confidence", "top", "new", "controversial", "old", "qa"],
        )?;
        let depth = number(options.depth, 3, 8)?;
        let limit = number(options.limit, 50, 200)?;
        let mut query = query();
        query.extend([
            ("sort".into(), sort.into()),
            ("limit".into(), limit.to_string()),
        ]);
        let raw = self
            .raw_get(&format!("/comments/{id}.json"), &query)
            .await?;
        normalize::comments(&raw.data, &raw.fetched_at, depth, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn listing() -> Value {
        json!({"kind":"Listing","data":{"after":"t3_next","children":[
            {"kind":"t3","data":{"id":"abc","permalink":"/r/rust/comments/abc/title/",
             "subreddit":"rust","title":"Test","selftext":"**body**","created_utc":1700000000}}
        ]}})
    }

    #[derive(Clone)]
    struct FixtureTransport;

    impl Transport for FixtureTransport {
        async fn raw_get(
            &self,
            path: &str,
            query: &[(String, String)],
        ) -> Result<crate::RawResponse, Error> {
            assert_eq!(path, "/r/rust/top.json");
            assert_eq!(
                query,
                &[
                    ("raw_json".into(), "1".into()),
                    ("limit".into(), "2".into()),
                    ("t".into(), "week".into()),
                    ("after".into(), "t3_prev".into()),
                ]
            );
            Ok(crate::RawResponse {
                data: listing(),
                fetched_at: "2026-10-06T00:00:00Z".into(),
            })
        }
    }

    #[tokio::test]
    async fn alternate_transport_uses_the_same_mapping_and_normalization() {
        let client = Client::with_transport(FixtureTransport);
        let result = client
            .list_posts(
                "rust",
                ListOptions {
                    sort: Some("top".into()),
                    time: Some("week".into()),
                    limit: Some(2),
                    cursor: Some("t3_prev".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(result.data[0].id, "abc");
        assert_eq!(result.data[0].body_markdown, "**body**");
        assert_eq!(
            result.data[0].permalink,
            "https://www.reddit.com/r/rust/comments/abc/title/"
        );
        assert_eq!(result.meta.next_cursor.as_deref(), Some("t3_next"));
        assert_eq!(result.meta.fetched_at, "2026-10-06T00:00:00Z");
    }

    async fn fixture(
        path: &str,
        expected: &[(&str, &str)],
        body: Value,
    ) -> (Client, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = Client::new(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let path = path.to_owned();
        let expected: std::collections::HashMap<String, String> = expected
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut headers = Vec::new();
            while !headers.ends_with(b"\r\n\r\n") {
                headers.push(stream.read_u8().await.unwrap());
            }
            let request = String::from_utf8(headers).unwrap();
            let target = request.split_whitespace().nth(1).unwrap();
            let url = url::Url::parse(&format!("http://localhost{target}")).unwrap();
            assert_eq!(url.path(), path);
            assert_eq!(
                url.query_pairs()
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect::<std::collections::HashMap<_, _>>(),
                expected
            );
            let body = body.to_string();
            let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nx-reddit-fetched-at: 2026-10-06T00:00:00Z\r\nConnection: close\r\n\r\n{body}", body.len());
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        (client, task)
    }

    #[tokio::test]
    async fn sdk_maps_all_operations_and_normalizes_raw_responses() {
        let (client, server) = fixture(
            "/reddit/r/NixOS/search.json",
            &[
                ("raw_json", "1"),
                ("q", "nix & flakes"),
                ("sort", "relevance"),
                ("t", "all"),
                ("limit", "2"),
                ("type", "link"),
                ("restrict_sr", "on"),
                ("after", "t3_prev"),
            ],
            listing(),
        )
        .await;
        let result = client
            .search(
                "nix & flakes",
                SearchOptions {
                    subreddit: Some("NixOS".into()),
                    limit: Some(2),
                    cursor: Some("t3_prev".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(result.data[0].body_markdown, "**body**");
        assert_eq!(result.meta.next_cursor.as_deref(), Some("t3_next"));
        assert_eq!(result.meta.fetched_at, "2026-10-06T00:00:00Z");

        let (client, server) = fixture(
            "/reddit/r/rust/top.json",
            &[("raw_json", "1"), ("limit", "20"), ("t", "week")],
            listing(),
        )
        .await;
        assert_eq!(
            client
                .list_posts(
                    "rust",
                    ListOptions {
                        sort: Some("top".into()),
                        time: Some("week".into()),
                        ..Default::default()
                    }
                )
                .await
                .unwrap()
                .data[0]
                .id,
            "abc"
        );
        server.await.unwrap();

        let thread = json!([listing(), {"kind":"Listing","data":{"children":[{"kind":"more","data":{"count":5}}]}}]);
        let (client, server) = fixture(
            "/reddit/comments/abc.json",
            &[("raw_json", "1"), ("limit", "1")],
            thread.clone(),
        )
        .await;
        assert_eq!(client.post("abc").await.unwrap().data.id, "abc");
        server.await.unwrap();
        let (client, server) = fixture(
            "/reddit/comments/abc.json",
            &[("raw_json", "1"), ("sort", "confidence"), ("limit", "50")],
            thread,
        )
        .await;
        let comments = client
            .comments("abc", CommentOptions::default())
            .await
            .unwrap();
        assert_eq!(comments.meta.truncation_reasons, vec!["more"]);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_options_fail_locally_before_network() {
        let client = Client::new("http://127.0.0.1:1").unwrap();
        assert!(matches!(
            client.search("", SearchOptions::default()).await,
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            client
                .search(
                    "x",
                    SearchOptions {
                        cursor: Some("bad".into()),
                        ..Default::default()
                    }
                )
                .await,
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            client
                .list_posts(
                    "rust",
                    ListOptions {
                        time: Some("week".into()),
                        ..Default::default()
                    }
                )
                .await,
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            client.post("../abc").await,
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            client
                .comments(
                    "abc",
                    CommentOptions {
                        depth: Some(0),
                        ..Default::default()
                    }
                )
                .await,
            Err(Error::InvalidArgument(_))
        ));
        assert!(matches!(
            client
                .comments(
                    "abc",
                    CommentOptions {
                        limit: Some(201),
                        ..Default::default()
                    }
                )
                .await,
            Err(Error::InvalidArgument(_))
        ));
    }

    #[tokio::test]
    async fn raw_access_preserves_unknown_fields_and_large_bodies_without_model_validation() {
        let body = json!({"new_reddit_field":{"arbitrary":[1,true,null]},"selftext":"a".repeat(1024*1024+1)});
        let (client, server) =
            fixture("/reddit/new.json", &[("future_param", "yes")], body.clone()).await;
        let result = client
            .raw_get("/new.json", &[("future_param".into(), "yes".into())])
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(result.data, body);
        assert_eq!(result.fetched_at, "2026-10-06T00:00:00Z");
    }

    #[tokio::test]
    async fn malformed_domain_schema_is_a_client_error() {
        let (client, server) = fixture(
            "/reddit/comments/abc.json",
            &[("raw_json", "1"), ("limit", "1")],
            json!({"future":"schema"}),
        )
        .await;
        assert!(matches!(
            client.post("abc").await,
            Err(Error::InvalidSchema(_))
        ));
        server.await.unwrap();
    }
}
