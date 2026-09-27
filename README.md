# tkda-desktop-cli

Rust command-line client for Takoda's authenticated laptop/desktop control plane.

The CLI is deliberately a **client**, not a supervisor. It talks only to `tkda-desktop-daemon` on loopback. The daemon adapts Takoda run semantics onto the locally installed `scintilla-run/scintilla-desktop-infra` appliance; the CLI never launches browsers, workers, containers, Cloudflare Tunnel, Scintilla, or `tkda-main-server` directly.

## Local topology

```text
tkda-desktop-cli
       |
       | Bearer auth, loopback only
       v
http://127.0.0.1:18087
       |
tkda-desktop-daemon
       |
       +---- Takoda local supervisor :18088
       |
       +---- Scintilla desktop control plane :8765
```

## Commands

```sh
# token is secret state, not an argv flag
export TKDA_LOCAL_CONTROL_TOKEN_FILE="$HOME/.takoda/desktop/control-token"

cargo run -- --command=status

cargo run -- \
  --command=run \
  --run-file=examples/run.playwright.json

cargo run -- \
  --command=run-status \
  --run-id=00000000-0000-0000-0000-000000000000

cargo run -- \
  --command=run-cancel \
  --run-id=00000000-0000-0000-0000-000000000000

cargo run -- --command=keep-awake --enabled=true
```

`--command=run` forces `execution_target=local` and `placement_preference=desktop`, because a desktop-specific CLI must not silently send a supposedly local request to cloud execution.

## Security

- daemon URL must be a credential-free `http://` loopback origin;
- bearer token is accepted only through `TKDA_LOCAL_CONTROL_TOKEN` or `TKDA_LOCAL_CONTROL_TOKEN_FILE`, never a CLI flag;
- run files and responses are capped at 1 MiB;
- run IDs must be UUIDs;
- there is no shell/exec/process-start command;
- HTTP redirects are disabled;
- all mutation paths are fixed by the CLI implementation.

## Relationship to `tkda-cli`

`tkda-cli` remains Takoda's broad hosted/local product CLI. `tkda-desktop-cli` is the minimal workstation/appliance client used for local diagnostics, local runs, and desktop lifecycle UX. Both should converge on the same `tkda-desktop-daemon` contract rather than owning separate local supervisors.
