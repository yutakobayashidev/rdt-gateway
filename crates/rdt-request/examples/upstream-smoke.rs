use rdt_request::{Error, Upstream};

#[tokio::main]
async fn main() {
    if let Err(error) = smoke().await {
        eprintln!(
            "smoke failed: status={} code={} retryable={} retry_after={:?}",
            error.status, error.code, error.retryable, error.retry_after_seconds
        );
        std::process::exit(1);
    }
}

async fn smoke() -> Result<(), Error> {
    let upstream = Upstream::new().await?;
    let listing = upstream.json("/r/rust/hot.json?limit=1&raw_json=1").await?;
    let posts = listing["data"]["children"]
        .as_array()
        .expect("listing children");
    println!("listing posts={}", posts.len());
    let id = posts
        .first()
        .and_then(|post| post["data"]["id"].as_str())
        .expect("listing post id");
    assert!(!id.is_empty() && id.bytes().all(|c| c.is_ascii_alphanumeric()));
    let search = upstream
        .json("/search.json?q=rust&limit=1&raw_json=1&type=link")
        .await?;
    println!(
        "search posts={}",
        search["data"]["children"]
            .as_array()
            .expect("search children")
            .len()
    );
    let post = upstream
        .json(&format!("/comments/{id}.json?limit=5&raw_json=1"))
        .await?;
    println!(
        "post listings={} top_level_comments={}",
        post.as_array().expect("post listings").len(),
        post[1]["data"]["children"]
            .as_array()
            .expect("comment children")
            .len()
    );
    Ok(())
}
