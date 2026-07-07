# xAI OAuth (Rust)

Mode B provider plugin for Grok models via xAI OAuth (same flow as the official Grok CLI).

## Install

Install from the official index entry `xai-oauth`, or add this repo as a provider plugin source in Covalt. The host builds the binary with `cargo build --release` on install.

`covalt-provider-sdk` is pulled from [github.com/sethburkart123/covalt](https://github.com/sethburkart123/covalt) (pinned `rev` in `Cargo.toml`) so the build works no matter where the plugin is installed.

## Sign in

Settings → Providers → xAI (Grok) → Sign in. Opens the xAI OAuth page; credentials are stored encrypted by the host.

## Models

When signed in, `models()` fetches live catalogs from:

- `GET https://api.x.ai/v1/models` — subscription/API models (grok-4.3, grok-4.20-*, imagine models, etc.)
- `GET https://cli-chat-proxy.grok.com/v1/models` — CLI coding models (`grok-build`, `grok-composer-2.5-fast`)

No hardcoded model list. IDs are used exactly as returned by each endpoint.

## Inference routing

| Models | Base URL |
|--------|----------|
| `grok-build`, `grok-composer-2.5-fast` | `https://cli-chat-proxy.grok.com/v1` (+ Grok CLI headers) |
| Everything else from `api.x.ai` | `https://api.x.ai/v1` |

Streaming uses the `openai-responses` dialect.

## Local build

```bash
cd plugins/xai-oauth
cargo build --release
```