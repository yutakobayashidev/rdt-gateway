# rdt-gateway

A headless, read-only gateway for public Reddit data, with separate CLI and MCP clients. Uses transport code adapted from Redlib; no Reddit account, API key, or HTML scraping is required.

- **rdt-gateway** handles anonymous authentication, rate limits, and caching, returning Reddit JSON.
- **rdt-cli** provides JSON output from the terminal.
- **rdt-mcp** exposes search, post listings, posts, and comments over stdio.

The CLI and MCP server share a Rust SDK that maps requests and normalizes responses. The SDK also offers `raw_get()` for unmodified JSON structure. Existing Reddit SDK compatibility is not guaranteed.

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

Configure your MCP client to launch the absolute path to `result-mcp/bin/rdt-mcp` using stdio. Run the gateway separately; the CLI is not required.

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

## License

[AGPL-3.0-only](LICENSE). See [NOTICE](NOTICE) for Redlib attribution and source provenance.
