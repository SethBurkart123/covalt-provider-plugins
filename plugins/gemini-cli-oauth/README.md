# Gemini CLI OAuth

Thin OAuth plugin for Cloud Code Assist (`google-code-assist` dialect).

## Credentials

Google ships public installed-app OAuth credentials in the [Gemini CLI](https://github.com/google-gemini/gemini-cli) project. Set them in your environment before signing in:

```bash
export GEMINI_CLI_OAUTH_CLIENT_ID="…"
export GEMINI_CLI_OAUTH_CLIENT_SECRET="…"
```

Copy the values from Gemini CLI’s OAuth config (same client id/secret the CLI uses). They are not user secrets — they identify the desktop app to Google — but GitHub push protection blocks committing them to this repo.

Optional: `GOOGLE_CLOUD_PROJECT` or `GOOGLE_CLOUD_PROJECT_ID` for paid-tier project selection during onboarding.
