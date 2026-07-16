# Covalt Provider Plugins

Official provider plugin index for [Covalt Desktop](https://github.com/sethburkart123/agno-app-electron).

Index URL (consumed by the app):  
https://raw.githubusercontent.com/sethburkart123/covalt-provider-plugins/main/index.json

## Plugins

| Plugin | Provider id | Runtime |
|--------|-------------|---------|
| `claude-oauth` | `anthropic-oauth` | Rust binary |
| `gemini-cli-oauth` | `google-gemini-cli` | Python |
| `windsurf-oauth` | `windsurf` | Rust binary |
| `xai-oauth` | `xai-oauth` | Rust binary |

Rust plugins are standalone crates (`provider.yaml` + `src/` binary). The remaining Python plugin is **yaml decl + thin Python**:
- `provider.yaml` — manifest metadata
- `api/provider.py` — `PLUGIN = Provider(...)` via `covalt.provider` (`oauth.*`, `prepare`)

## Structure

- `index.json` — plugin store index (`sources[]` with `repoUrl`, `pluginPath`, `trackingRef`)
- `plugins/<id>/` — one directory per plugin (`provider.yaml` + Rust crate or `api/provider.py`)

## Adding a plugin

1. Create `plugins/<your-plugin-id>/` with `provider.yaml` and a Rust crate or `api/provider.py`
2. Add an entry to `index.json` `sources`
3. Open a pull request

## Install from the app

Settings → Provider plugins → refresh the official index → install **Claude OAuth**, **Gemini CLI OAuth**, **Windsurf OAuth**, or **xAI OAuth**.

**Gemini CLI OAuth** needs `GEMINI_CLI_OAUTH_CLIENT_ID` and `GEMINI_CLI_OAUTH_CLIENT_SECRET` in the app environment (public values from [Gemini CLI](https://github.com/google-gemini/gemini-cli); see `plugins/gemini-cli-oauth/README.md`).
