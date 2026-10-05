use serde_json::{Value, json};
use std::process::Stdio;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines},
    net::TcpListener,
    process::{ChildStdout, Command},
    time::{Duration, timeout},
};

async fn response(lines: &mut Lines<BufReader<ChildStdout>>) -> Value {
    let line = timeout(Duration::from_secs(10), lines.next_line())
        .await
        .expect("MCP response timeout")
        .unwrap()
        .expect("MCP exited");
    serde_json::from_str(&line).expect("stdout must contain only JSON-RPC")
}

const FETCHED_AT: &str = "2026-10-05T12:00:00Z";

fn reddit_response(index: usize) -> Value {
    let listing = json!({"kind":"Listing","data":{"after":"t3_next", "children":[{"kind":"t3","data":{
        "id":"abc123", "permalink":"/r/rust/comments/abc123/title/", "subreddit":"rust", "author":"alice", "title":"Fixture", "selftext":"**original markdown**", "selftext_html":"do not parse", "is_self":true, "score":42, "num_comments":2, "created_utc":1700000000
    }}]}});
    if index < 2 {
        return listing;
    }
    json!([listing,{"kind":"Listing","data":{"children":[
        {"kind":"t1","data":{"id":"c1","parent_id":"t3_abc123","author":"bob","body":"Comment **markdown**","score":5,"created_utc":1700000000,"replies":""}},
        {"kind":"more","data":{"children":["c2"]}}
    ]}}])
}

fn expected_response(index: usize) -> Value {
    let post = json!({"id":"abc123", "permalink":"https://www.reddit.com/r/rust/comments/abc123/title/", "subreddit":"rust", "author":"alice", "title":"Fixture", "body_markdown":"**original markdown**", "body_truncated":false, "url":null, "score":42, "num_comments":2, "created_at":"2023-11-14T22:13:20Z", "nsfw":false, "spoiler":false, "content_status":"available"});
    let data = match index {
        0 | 1 => json!([post]),
        2 => post,
        _ => {
            json!([{"id":"c1","parent_id":"t3_abc123","author":"bob","body_markdown":"Comment **markdown**","body_truncated":false,"score":5,"created_at":"2023-11-14T22:13:20Z","content_status":"available","replies":[]}])
        }
    };
    json!({"data":data,"meta":{"fetched_at":FETCHED_AT,"next_cursor":if index < 2 {json!("t3_next")}else{Value::Null},"truncated":index == 3,"truncation_reasons":if index == 3 {json!(["more"])}else{json!([])}}})
}

#[tokio::test]
async fn stdio_protocol_routes_tools_and_preserves_gateway_errors() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mock = tokio::spawn(async move {
        let mut paths = Vec::new();
        for index in 0..5 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let byte = stream.read_u8().await.unwrap();
                request.push(byte);
                if request.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap();
            paths.push(request.lines().next().unwrap().to_owned());
            let (status, body) = if index == 4 {
                (
                    "429 Too Many Requests",
                    json!({"error":{"code":"rate_limited","message":"try later","retryable":true,"retry_after_seconds":30}}),
                )
            } else {
                ("200 OK", reddit_response(index))
            };
            let body = body.to_string();
            stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nX-Reddit-Fetched-At: {FETCHED_AT}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
        paths
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_rdt-mcp"))
        .env("RDT_GATEWAY_URL", url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    stdin.write_all(format!("{}\n", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})).as_bytes()).await.unwrap();
    let initialized = response(&mut lines).await;
    assert_eq!(initialized["id"], 1);
    assert!(initialized["result"]["capabilities"]["tools"].is_object());
    stdin.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n").await.unwrap();
    let listed = response(&mut lines).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 4);
    for tool in tools {
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
        assert_eq!(tool["inputSchema"]["type"], "object");
        assert_eq!(tool["outputSchema"]["type"], "object");
        assert!(tool["outputSchema"]["properties"]["data"].is_object());
        assert!(tool["outputSchema"]["properties"]["meta"].is_object());
        let required = tool["outputSchema"]["required"].as_array().unwrap();
        assert!(required.contains(&json!("data")) && required.contains(&json!("meta")));
        if tool["name"] == "reddit_get_post" || tool["name"] == "reddit_get_comments" {
            assert_eq!(
                tool["inputSchema"]["properties"]["id"]["pattern"],
                "^[A-Za-z0-9]{1,16}$"
            );
        }
        if tool["name"] == "reddit_list_posts" {
            assert_eq!(
                tool["inputSchema"]["properties"]["name"]["pattern"],
                "^[A-Za-z0-9_]{1,32}$"
            );
        }
    }
    let calls = [
        ("reddit_search", json!({"q":"nix & rust","limit":2})),
        ("reddit_list_posts", json!({"name":"nixos","sort":"new"})),
        ("reddit_get_post", json!({"id":"abc123"})),
        (
            "reddit_get_comments",
            json!({"id":"abc123","depth":2,"limit":4}),
        ),
        ("reddit_get_post", json!({"id":"failed"})),
    ];
    for (index, (name, arguments)) in calls.into_iter().enumerate() {
        stdin.write_all(format!("{}\n", json!({"jsonrpc":"2.0","id":index+3,"method":"tools/call","params":{"name":name,"arguments":arguments}})).as_bytes()).await.unwrap();
        let result = response(&mut lines).await;
        assert_eq!(result["id"], index + 3);
        assert_eq!(result["result"]["isError"], index == 4, "{result}");
        if index < 4 {
            assert_eq!(
                result["result"]["structuredContent"],
                expected_response(index)
            );
        } else {
            assert!(
                result["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("try later"),
                "{result}"
            );
        }
    }
    let paths = timeout(Duration::from_secs(10), mock)
        .await
        .unwrap()
        .unwrap();
    assert!(paths[0].starts_with("GET /reddit/search.json?"));
    assert!(paths[0].contains("q=nix+%26+rust"));
    assert!(paths[0].contains("limit=2"));
    assert!(paths[1].starts_with("GET /reddit/r/nixos/new.json?"));
    assert!(paths[2].starts_with("GET /reddit/comments/abc123.json?"));
    assert!(paths[3].starts_with("GET /reddit/comments/abc123.json?"));
    assert!(paths[3].contains("limit=4"));
    for path in &paths {
        assert!(path.contains("raw_json=1"), "{path}");
    }
    // The mock has closed: invalid segments must fail locally, without HTTP.
    for (name, arguments, message) in [
        ("reddit_get_post", json!({"id":"t3_abc"}), "id"),
        ("reddit_get_comments", json!({"id":"a".repeat(17)}), "id"),
        (
            "reddit_list_posts",
            json!({"name":"a".repeat(33)}),
            "subreddit",
        ),
        (
            "reddit_search",
            json!({"q":"x","subreddit":"../all"}),
            "subreddit",
        ),
    ] {
        stdin.write_all(format!("{}\n", json!({"jsonrpc":"2.0","id":20,"method":"tools/call","params":{"name":name,"arguments":arguments}})).as_bytes()).await.unwrap();
        let result = response(&mut lines).await;
        assert_eq!(result["result"]["isError"], true);
        assert!(
            result["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains(message),
            "{result}"
        );
    }
    drop(stdin);
    timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
}
