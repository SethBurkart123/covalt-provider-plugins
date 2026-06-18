# Covalt Provider Plugins

Official provider plugin index for [Covalt Desktop](https://github.com/sethburkart123/agno-app-electron).

Index URL (consumed by the app):  
https://raw.githubusercontent.com/sethburkart123/covalt-provider-plugins/main/index.json

## Plugins

| Plugin | Provider id | Dialect |
|--------|-------------|---------|
| `claude-oauth` | `anthropic_oauth` | `anthropic-messages` |
| `gemini-cli-oauth` | `google_gemini_cli` | `google-code-assist` |

Each plugin is **yaml decl + thin Python** (Mode A — Rust dialects stream; plugins mutate per turn):
- `provider.yaml` — dialect, base URL, headers, OAuth variant
- `api/provider.py` — `PLUGIN = ProviderPlugin(...)` via `covalt.provider_sdk` (`prepare` hook)
- `plugin.py` — stream config, models, options (uses core `oauth_plugin_entry`)
- `oauth.py` — OAuth begin/refresh only (host keeps storage + callback server)

## Structure

- `index.json` — plugin store index (`sources[]` with `repoUrl`, `pluginPath`, `trackingRef`)
- `plugins/<id>/` — one directory per plugin (`provider.yaml` + entrypoint)

## Adding a plugin

1. Create `plugins/<your-plugin-id>/` with `provider.yaml` and `plugin.py`
2. Add an entry to `index.json` `sources`
3. Open a pull request

## Install from the app

Settings → Provider plugins → refresh the official index → install **Claude OAuth** or **Gemini CLI OAuth**.

**Gemini CLI OAuth** needs `GEMINI_CLI_OAUTH_CLIENT_ID` and `GEMINI_CLI_OAUTH_CLIENT_SECRET` in the app environment (public values from [Gemini CLI](https://github.com/google-gemini/gemini-cli); see `plugins/gemini-cli-oauth/README.md`).
