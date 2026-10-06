# rdt-gateway

A headless, read-only gateway for public Reddit data, with a standalone HTTP server, CLI, and self-contained MCP server. Uses transport code adapted from Redlib; no Reddit account, API key, or HTML scraping is required.

- **rdt-request** owns anonymous Reddit authentication and upstream transport.
- **rdt-gateway** provides a shared request service with caching and rate limits, an Axum router, and a standalone HTTP executable.
- **rdt-cli** provides JSON output from the terminal.
- **rdt-mcp** bundles the request service and exposes search, post listings, posts, and comments over stdio.

The CLI and MCP server share a Rust SDK that maps requests and normalizes responses. The SDK also offers `raw_get()` for unmodified JSON structure. Existing Reddit SDK compatibility is not guaranteed.

## Package layout

Inspired by [twitter_api_safe_relay](https://github.com/fa0311/twitter_api_safe_relay): a reusable request package, a server library with its own executable, and an MCP package that composes them.

| Crate | Responsibility |
| --- | --- |
| `rdt-request` | Direct Reddit transport, independent of HTTP serving |
| `rdt-gateway` | Shared request state/cache, HTTP router, and daemon |
| `rdt-gateway-types` | Shared responses and errors |
| `rdt-gateway-client` | Transport-independent request mapping and normalization, plus the HTTP client |
| `rdt-cli` | CLI using the HTTP client |
| `rdt-mcp` | MCP library and executable, with an embedded request service |

In embedded mode, MCP calls the shared service directly. When HTTP is enabled, both interfaces share authentication, cache, and rate-limit state. Normalization stays in the client library; the gateway returns Reddit JSON.

## Quick start

From a checkout, with Nix flakes enabled (currently x86_64 Linux):

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

## MCP

```sh
nix build .#rdt-mcp --out-link result-mcp
```

Configure your MCP client to launch the absolute path to `result-mcp/bin/rdt-mcp` using stdio. It runs on its own; no separate gateway or CLI is required.

To also serve the HTTP gateway from the same process, pass `--listen 127.0.0.1:8787`. To use an existing gateway instead, pass `--gateway-url http://127.0.0.1:8787` or set `RDT_GATEWAY_URL`. Remote mode and `--listen` are mutually exclusive. Unset `RDT_GATEWAY_URL` when using embedded mode.

Diagnostics go to stderr. Closing the MCP session or sending SIGINT/SIGTERM stops the process and its optional HTTP listener.

Tools: `reddit_search`, `reddit_list_posts`, `reddit_get_post`, and `reddit_get_comments`. Results include citation URLs and metadata indicating omitted content.

## Raw HTTP

```sh
curl 'http://127.0.0.1:8787/reddit/r/rust/hot.json?limit=5&raw_json=1'
```

`GET /reddit/{path}` forwards relative `.json` paths and query parameters to Reddit. `/api/*` paths are excluded. Successful responses preserve Reddit's JSON structure; `x-reddit-fetched-at` records the original fetch time. Health endpoints are `/health/live` and `/health/ready`.

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

## License

[AGPL-3.0-only](LICENSE). See [NOTICE](NOTICE) for Redlib attribution and source provenance.
