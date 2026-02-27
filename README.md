# Covalt Provider Plugins

Official provider plugin index for Covalt Desktop.

## Structure

- `index.json` - Plugin index consumed by Covalt Desktop's plugin store
- `plugins/` - Individual provider plugin directories, each containing a `provider.yaml` manifest

## Adding a Plugin

1. Create a new directory under `plugins/` with your plugin id
2. Add a `provider.yaml` manifest (see existing plugins for examples)
3. Add your entry to `index.json`
4. Open a pull request
