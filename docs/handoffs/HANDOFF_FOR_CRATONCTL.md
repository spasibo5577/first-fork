# HANDOFF_FOR_CRATONCTL.md

## 1. Purpose

Build `cratonctl`, a thin operator CLI for `cratond`.

It must sit on top of the existing localhost HTTP API and must not bypass the daemon.

## 2. Chosen Language

Use **Rust** for MVP.

Why:
- same repository/toolchain as `cratond`
- simpler release and maintenance story
- thin HTTP client is not large enough to justify a second language

## 3. MVP Scope

Implement first:
- `health`
- `status`
- `services`
- `service <id>`
- `history recovery|backup|remediation`
- `diagnose <service>`
- `trigger <task>`
- `restart <service>`
- `maintenance set <service> --for 1h --reason "..."`
- `maintenance clear <service>`
- `breaker clear <service>`
- `flapping clear <service>`
- `backup run`
- `backup unlock`
- `disk cleanup`
- global flags `--url`, `--token`, `--json`, `--quiet`, `--no-color`

## 4. Suggested File Structure

```text
src/bin/cratonctl.rs
src/cratonctl/
  mod.rs
  cli.rs
  client.rs
  auth.rs
  dto.rs
  output.rs
  error.rs
  commands/
    health.rs
    status.rs
    services.rs
    history.rs
    diagnose.rs
    trigger.rs
    remediate.rs
```

## 5. First Commands to Implement

Order:
1. `cratonctl health`
2. `cratonctl status`
3. `cratonctl services`
4. `cratonctl service <id>`
5. `cratonctl history recovery`
6. `cratonctl history backup`
7. `cratonctl history remediation`
8. `cratonctl diagnose <service>`
9. `cratonctl trigger <task>`
10. mutating remediation wrappers

## 6. HTTP Contracts

Read-only:
- `GET /health`
- `GET /api/v1/state`
- `GET /api/v1/history/recovery`
- `GET /api/v1/history/backup`
- `GET /api/v1/history/remediation`
- `GET /api/v1/diagnose/{service}`

Mutating:
- `POST /trigger/{task}`
- `POST /api/v1/remediate`

For remediation, use daemon-supported actions:
- `RestartService`
- `ResticUnlock`
- `DockerRestart`
- `MarkMaintenance`
- `ClearMaintenance`
- `ClearFlapping`
- `RunDiskCleanup`
- `TriggerBackup`
- `ClearBreaker`

## 7. Output / Error Rules

Default:
- human-readable
- concise
- tables for lists

JSON mode:
- `--json`
- stdout only JSON
- stderr only for real client errors

Errors:
- do not print token
- do not print auth header
- distinguish daemon rejection from transport failure

Exit codes:
- `0` success
- `1` daemon returned rejection / unavailable result / not found
- `2` local validation / config / network / parse error

## 8. Auth Rules

URL priority:
1. `--url`
2. `CRATONCTL_URL`
3. config file
4. default `http://127.0.0.1:18800`

Token priority:
1. `--token`
2. `CRATONCTL_TOKEN`
3. config file token setting
4. autodiscovery token file `/var/lib/craton/remediation-token`

Read-only commands:
- no token required

Mutating commands:
- fail fast if token missing

## 9. What Not To Do

Do not:
- modify daemon state files directly
- call `systemctl` directly
- reimplement daemon policy
- add background automation
- add TUI before the CLI is solid
- add YAML output in MVP
- make `cratonctl` a second controller

## 10. Recommended Implementation Order

1. Define DTOs for `/health`, `/api/v1/state`, history arrays, and diagnose payload.
2. Implement small HTTP client wrapper with timeout and bearer support.
3. Implement auth/config source resolution.
4. Implement output renderer layer.
5. Ship read-only commands first.
6. Add mutating commands once auth is stable.
7. Add polish: colors, quiet mode, nicer tables.

