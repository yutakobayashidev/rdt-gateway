# rdt-gateway

A headless, read-only gateway for public Reddit data, with a standalone HTTP server, CLI, and self-contained MCP server. Uses transport code adapted from Redlib; no Reddit account, API key, or HTML scraping is required.

- **rdt-request** owns anonymous Reddit authentication and upstream transport.
- **rdt-gateway** provides a shared request service with caching and rate limits, an Axum router, and a standalone HTTP executable.
- **rdt** provides JSON or readable text from the terminal.
- **rdt-mcp** bundles the request service and exposes public posts, comments, communities, and user activity over stdio.

The CLI and MCP server share a Rust SDK that maps requests and normalizes responses. The SDK also offers `raw_get()` for unmodified JSON structure. Existing Reddit SDK compatibility is not guaranteed.

## Package layout

Inspired by [twitter_api_safe_relay](https://github.com/fa0311/twitter_api_safe_relay): a reusable request package, a server library with its own executable, and an MCP package that composes them.

| Crate | Responsibility |
| --- | --- |
| `rdt-request` | Direct Reddit transport, independent of HTTP serving |
| `rdt-gateway` | Shared request state/cache, HTTP router, and daemon |
| `rdt-gateway-types` | Shared responses and errors |
| `rdt-gateway-client` | Transport-independent request mapping and normalization, plus the HTTP client |
| `rdt-cli` | `rdt` CLI using the HTTP client |
| `rdt-mcp` | MCP library and executable, with an embedded request service |

In embedded mode, MCP calls the shared service directly. When HTTP is enabled, both interfaces share authentication, cache, and rate-limit state. Normalization stays in the client library; the gateway returns Reddit JSON.

## Quick start

From a checkout, with Nix flakes enabled (gateway and MCP: x86_64 Linux):

```sh
nix run .#rdt-gateway
```

In another terminal:

```sh
nix run .#rdt-cli -- search 'nixos flakes' --limit 5
nix run .#rdt-cli -- posts rust --sort new
nix run .#rdt-cli -- post POST_ID
nix run .#rdt-cli -- comments POST_ID --depth 3 --limit 50
```

The gateway listens on `127.0.0.1:8787`. Set `RDT_GATEWAY_LISTEN` to change it, or `RDT_GATEWAY_URL` to point either client at another gateway. The HTTP endpoint has no authentication.

## CLI

The `rdt-cli` Nix package and app are available for x86_64 Linux, aarch64 Linux, and Apple Silicon macOS. They provide the `rdt` executable, which connects to a gateway over HTTP.

With `rdt` installed (or using `nix run .#rdt-cli --`):

```text
rdt [--url URL] [--json | --text] [--verbose] COMMAND
  search QUERY [--subreddit NAME]
  posts NAME
  post POST
  read POST
  comments POST
  subreddit search QUERY
  subreddit info NAME
  subreddit wiki NAME [--page index]
  subreddit rules NAME
  user info NAME
  user posts NAME
  user comments NAME
  completions bash|zsh|fish
```

Names accept `r/name` or `u/name` as appropriate. `POST` accepts a post ID, `t3_ID`, or Reddit post URL; `-` reads one value from stdin. Wiki pages can contain nested paths such as `faq/install`.

```sh
rdt subreddit search 'self hosting' --limit 5
rdt posts popular --sort top --time week
rdt user comments USERNAME --sort new
rdt read POST_URL --expand-more --max-requests 3 --limit 100 --text
rdt search 'nixos flakes' --limit 1 | jq -r '.data[0].id' | rdt read -
rdt completions zsh > _rdt
```

Search and listing commands accept `--limit` (default 20, maximum 100) and `--cursor` from `meta.next_cursor`; they do not automatically fetch subsequent pages. Post search defaults to relevance, subreddit posts to hot, and user history to new. Use `--help` for supported `--sort` values. `--time hour|day|week|month|year|all` filters searches or top/controversial listings.

`read` returns the post and comments together; `comments` returns only comments. Both accept `--depth` (default 3, maximum 8; top-level comments are depth 1) and a total comment `--limit` (default 50, maximum 200). `--expand-more` fetches omitted comments using `/api/morechildren.json`. Its `--max-requests` budget defaults to 3, maximum 10, and includes the initial post/comment request but excludes authentication. Without expansion, only the initial request is made.

JSON is the default, independent of TTY; `--json` makes it explicit and `--text` renders readable text. Results go to stdout and diagnostics to stderr. `--verbose` reports the gateway origin and elapsed time. Output is uncolored (`--no-color` and `NO_COLOR` are supported); commands never prompt. Gateway selection is `--url` > `RDT_GATEWAY_URL` > `http://127.0.0.1:8787`.

Responses use `{data, meta}` with citation URLs and fetch time. `meta.requests` counts SDK transport calls, including cache hits, not physical Reddit requests. `meta.truncated` and `truncation_reasons` describe omitted content. A failed comment expansion preserves the available result and adds `meta.partial_error`; the CLI exits 1. Exit codes are 0 for success (including empty results and requested limits), 1 for retrieval failures, 2 for invalid arguments, and 130 for interruption.

## MCP

```sh
nix build .#rdt-mcp --out-link result-mcp
```

Configure your MCP client to launch the absolute path to `result-mcp/bin/rdt-mcp` using stdio. It runs on its own; no separate gateway or CLI is required.

To also serve the HTTP gateway from the same process, pass `--listen 127.0.0.1:8787`. To use an existing gateway instead, pass `--gateway-url http://127.0.0.1:8787` or set `RDT_GATEWAY_URL`. Remote mode and `--listen` are mutually exclusive. Unset `RDT_GATEWAY_URL` when using embedded mode.

Diagnostics go to stderr. Closing the MCP session or sending SIGINT/SIGTERM stops the process and its optional HTTP listener.

The 12 tools share the CLI's SDK, limits, normalized responses, and partial-result metadata:

- Posts: `reddit_search`, `reddit_list_posts`, `reddit_get_post`, `reddit_read`, `reddit_get_comments`.
- Communities: `reddit_search_subreddits`, `reddit_get_subreddit`, `reddit_get_wiki`, `reddit_get_rules`.
- Users: `reddit_get_user`, `reddit_list_user_posts`, `reddit_list_user_comments`.

MCP uses typed arguments such as `expand_more` and `max_requests`; no CLI subprocess or raw-query tool is involved. Partial retrieval failures return the available data with the MCP error flag set.

## Raw HTTP

```sh
curl 'http://127.0.0.1:8787/r/rust/hot.json?limit=5&raw_json=1'
```

`GET /{path}` forwards relative `.json` paths and query parameters to Reddit, including `/api/*.json`. Upstream requests use GET with anonymous authentication and a fixed Reddit origin; write methods and invalid paths are rejected. Successful responses preserve Reddit's JSON structure; `x-reddit-fetched-at` records the original fetch time. `GET /health` returns `200` with `{ "ok": true }` when the gateway is running; it does not contact Reddit or report upstream availability. The gateway does not strip a `/reddit` prefix; update HTTP clients and the gateway together when migrating from prefixed URLs.

## NixOS

Add this repository as a flake input named `rdt-gateway`, then:

```nix
{
  imports = [ inputs.rdt-gateway.nixosModules.default ];
  services.rdt-gateway.enable = true;
}
```

### OpenAI Secure MCP Tunnel

Add [openai-secure-tunnel-nix](https://github.com/nakasyou/openai-secure-tunnel-nix) to your flake inputs:

```nix
inputs.rdt-gateway.url = "github:yutakobayashidev/rdt-gateway";
inputs.openai-secure-tunnel-nix = {
  url = "github:nakasyou/openai-secure-tunnel-nix";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

This module runs the self-contained MCP server through the tunnel. Pass `inputs` through `specialArgs`:

```nix
{ inputs, pkgs, ... }:
let
  packages = inputs.rdt-gateway.packages.${pkgs.stdenv.hostPlatform.system};
  tunnelService = "tunnel-client-rdt-gateway";
in
{
  imports = [
    inputs.openai-secure-tunnel-nix.nixosModules.tunnel-client
  ];

  services = {
    openai-tunnel-client.instances.rdt-gateway = {
      enable = true;
      settings = {
        config_version = 1;
        control_plane = {
          tunnel_id = "tunnel_YOUR_ID";
          api_key = "file:/run/credentials/${tunnelService}.service/api-key";
        };
        health.listen_addr = "127.0.0.1:18792";
        admin_ui.open_browser = false;
        mcp.commands = [{
          channel = "main";
          command = "${packages.rdt-mcp}/bin/rdt-mcp";
        }];
      };
    };
  };

  systemd.services.${tunnelService} = {
    serviceConfig.LoadCredential = [
      "api-key:/run/secrets/openai-tunnel-api-key"
    ];
  };
}
```

Replace `tunnel_YOUR_ID` with your tunnel ID. Provision the API key at `/run/secrets/openai-tunnel-api-key` using your secret manager; never put the key in a Nix expression. The tunnel launches the self-contained stdio MCP server; no separate gateway service or public listener is needed. See the [OpenAI guide](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels) for tunnel creation and ChatGPT setup.

## Updating app versions

From the checkout, run `bash scripts/update_oauth_resources.sh` with Bash, curl, ripgrep, and coreutils available (also included in `nix develop`). It fetches Android app versions from APKCombo and regenerates `crates/rdt-request/src/oauth_resources.rs`. Review the diff before committing. HTTP errors or unexpected page markup leave the existing file untouched.

Use `--output /tmp/oauth_resources.rs` to preview without replacing the checked-in file. Updater tests run with `python3 -m unittest discover -s scripts/tests`.

The **Update OAuth resources** workflow runs every Monday at 03:17 UTC (12:17 JST), or manually from Actions on `main`. It fetches versions, compiles the generated Rust file, and creates or updates a PR on `automation/update-oauth-resources` only when there is a diff. Fetch or validation failures stop the run without publishing changes.

Enable **Allow GitHub Actions to create and approve pull requests** in Settings → Actions → General → Workflow permissions. No extra secret is needed. PRs created with `GITHUB_TOKEN` do not trigger pull-request CI automatically; run **CI** manually on `automation/update-oauth-resources` before merging. Updates are never automatically merged.

## CI

GitHub Actions runs on pull requests and pushes to `main`. It runs the updater’s Python tests, workspace Rust tests, and a NixOS service smoke test, and builds all three executables. Dependencies come from the locked nixpkgs input; actions are pinned to commit SHAs.

Run `nix flake check -L` for workspace tests and the NixOS service smoke test (requires Linux with KVM), then `nix build --no-link .#rdt-gateway .#rdt-cli .#rdt-mcp` for the packages.

## License

[AGPL-3.0-only](LICENSE). See [NOTICE](NOTICE) for Redlib attribution and source provenance.
