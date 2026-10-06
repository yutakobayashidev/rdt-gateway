use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Stdio},
};

fn cli() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rdt"));
    command.env_remove("RDT_GATEWAY_URL");
    command
}
fn serve(body: serde_json::Value) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = vec![0; 16384];
        let len = stream.read(&mut request).unwrap();
        let body = body.to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nx-reddit-fetched-at: 2026-10-06T00:00:00Z\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        String::from_utf8(request[..len].to_vec()).unwrap()
    });
    (url, handle)
}
#[test]
fn empty_search_is_json_success_and_diagnostics_stay_on_stderr() {
    let (url, server) = serve(json!({"kind":"Listing","data":{"children":[],"after":null}}));
    let output = cli()
        .args(["--url", &url, "search", "private-query", "--verbose"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"], json!([]));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("gateway") && stderr.contains("elapsed"));
    assert!(!stderr.contains("private-query"));
    assert!(server.join().unwrap().contains("search.json"));
}
#[test]
fn reads_one_post_url_from_stdin() {
    let (url, server) = serve(
        json!([{"kind":"Listing","data":{"children":[{"kind":"t3","data":{"id":"abc123","title":"A title","permalink":"/r/test/comments/abc123/title/","subreddit":"test","created_utc":1,"selftext":"Body"}}]}},{"kind":"Listing","data":{"children":[]}}]),
    );
    let mut child = cli()
        .args(["--url", &url, "read", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"https://www.reddit.com/r/test/comments/abc123/title/\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["post"]["id"], "abc123");
    assert_eq!(value["data"]["comments"], json!([]));
    assert!(server.join().unwrap().contains("comments/abc123.json"));
}
#[test]
fn invalid_input_exits_two_without_stdout() {
    for args in [
        vec!["post", "../search"],
        vec!["comments", "abc", "--max-requests", "2"],
        vec!["post", "abc", "--json", "--text"],
    ] {
        let output = cli().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
    let mut child = cli()
        .args(["post", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"abc def\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}
#[test]
fn text_and_completions_work_without_interaction() {
    let (url, server) = serve(json!({"kind":"Listing","data":{"children":[],"after":null}}));
    let output = cli()
        .args(["--url", &url, "posts", "all", "--text", "--no-color"])
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("data:") && !text.contains('\u{1b}'));
    server.join().unwrap();
    for shell in ["bash", "zsh", "fish"] {
        let output = cli().args(["completions", shell]).output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8(output.stdout).unwrap().contains("rdt"));
    }
}

#[cfg(unix)]
#[test]
fn ctrl_c_exits_promptly_while_waiting_for_stdin() {
    let mut child = cli()
        .args(["post", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Keep the input pipe open so Ctrl-C must interrupt the pending read.
    let _stdin = child.stdin.take().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(250));
    assert!(Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap()
        .success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(status.code(), Some(130));
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("CLI did not stop after Ctrl-C");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn help_takes_priority_over_invalid_or_missing_arguments() {
    for args in [
        vec!["--invalid", "--help"],
        vec!["read", "--depth", "99", "--help"],
        vec!["user", "posts", "--sort", "invalid", "-h"],
        vec!["post", "--url", "--help"],
    ] {
        let output = cli().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8(output.stdout).unwrap().contains("Usage:"));
    }
}
#[test]
fn enumerated_options_reject_unknown_values() {
    for args in [
        vec!["posts", "all", "--sort", "unknown"],
        vec!["search", "test", "--time", "forever"],
        vec!["comments", "abc", "--sort", "hot"],
        vec!["completions", "powershell"],
    ] {
        let output = cli().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn sort_choices_are_specific_to_each_operation() {
    for (args, excluded, included) in [
        (vec!["search", "--help"], "rising", "relevance"),
        (vec!["posts", "--help"], "relevance", "controversial"),
        (vec!["user", "posts", "--help"], "comments", "controversial"),
    ] {
        let output = cli().args(args).output().unwrap();
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        let choices = help
            .lines()
            .find(|line| line.contains("possible values:") && line.contains(included))
            .expect("sort choices");
        assert!(!choices.contains(excluded), "{help}");
    }
    for args in [
        vec!["search", "nix", "--sort", "rising"],
        vec!["posts", "all", "--sort", "relevance"],
        vec!["user", "posts", "test", "--sort", "comments"],
    ] {
        let output = cli().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
}

#[test]
fn nested_help_includes_global_flags_and_full_command_path() {
    for (args, usage) in [
        (vec!["read", "--help"], "Usage: rdt read"),
        (vec!["user", "posts", "--help"], "Usage: rdt user posts"),
    ] {
        let output = cli().args(args).output().unwrap();
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains(usage), "{help}");
        for flag in ["--url", "--json", "--text", "--verbose", "--no-color"] {
            assert!(help.contains(flag), "missing {flag}: {help}");
        }
    }
}
