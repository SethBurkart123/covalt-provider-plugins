from __future__ import annotations

from covalt.provider_sdk import PluginCapabilities, ProviderPlugin

PLUGIN = ProviderPlugin(
    id="google_gemini_cli",
    dialect="google-code-assist",
    base_url="https://cloudcode-pa.googleapis.com",
    capabilities=PluginCapabilities(),
)
