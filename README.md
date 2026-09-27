# tkda-desktop-cli

Rust operator CLI for a Takoda laptop/desktop appliance.

This CLI is deliberately local-only. It talks to the authenticated loopback API owned by `tkda-desktop-daemon` (default `http://127.0.0.1:18087`) and never launches arbitrary processes itself.

## Why this exists

`tkda-cli` is the full Takoda product CLI and can also talk to the hosted control plane. `tkda-desktop-cli` is the smaller machine-operator surface that can be bundled with `tkda-desktop-infra` and continue to work when the cloud API is unavailable.

The daemon remains the single lifecycle authority for:

- local runs;
- headed/headless policy;
- Playwright, Puppeteer and Selenium capability admission;
- keep-awake state;
- desktop app registration/control;
- local Scintilla-backed worker/process orchestration.

## Authentication

Set exactly one of:

```sh
export TKDA_LOCAL_CONTROL_TOKEN='...at-least-32-characters...'
# or
export TKDA_LOCAL_CONTROL_TOKEN_FILE="$HOME/.config/takoda/local-control.token"
```

The daemon URL is required to be credential-free loopback HTTP.

## Examples

```sh
cargo run -- --command=status
cargo run -- --command=doctor

cargo run -- \
  --command=run \
  --prompt='Open example.com and return the page title' \
  --language=typescript \
  --browser-engine=playwright \
  --headed=true

cargo run -- --command=keep-awake --keep-awake=true
cargo run -- --command=get --run-id='<uuid>'
cargo run -- --command=cancel --run-id='<uuid>'

cargo run -- --command=apps
cargo run -- --command=app-command --app=rust --app-action=focus
cargo run -- --command=app-command --app=flutter --app-action=open_run --app-target='<run-id>'
```

The `apps` and `app-command` commands require the daemon desktop-app control-bus routes.
