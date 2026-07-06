# Windsurf OAuth (Rust)

Mode B provider plugin for Cognition (Windsurf) subscription models.

Ported from [opencode-windsurf-auth](https://github.com/rsvedant/opencode-windsurf-auth): OAuth via `RegisterUser`, inference via Connect-RPC `GetChatMessage`.

## Install

Add this repo as a provider plugin source in Covalt, or install from the official index entry `windsurf-oauth`. The host builds the binary with `cargo build --release` on install.

`covalt-provider-sdk` is pulled from [github.com/sethburkart123/covalt](https://github.com/sethburkart123/covalt) (pinned `rev` in `Cargo.toml`) so the build works no matter where the plugin is installed.

## Sign in

Settings → Providers → Windsurf → Sign in. Opens the Windsurf browser OAuth flow; credentials are stored encrypted by the host.

## Smoke test (after login)

Export your api key from provider settings (or read from the app's credential store), then:

```bash
cd plugins/windsurf-oauth
cargo run --release --bin windsurf-smoke -- swe-1.6 "hello from rust"
```

Environment:

- `WINDSURF_API_KEY` — the `devin-session-token$…` value from login
- `WINDSURF_API_SERVER_URL` — optional; defaults to `https://server.codeium.com`

## Models

Curated set: `swe-1.6`, `claude-opus-4.7`, `gpt-5.5`, `kimi-k2.6`, `gemini-3.5-flash`, `claude-opus-4.6`, `deepseek-v4`. Use the **Variant** control (or `model:variant` suffix) where supported.
