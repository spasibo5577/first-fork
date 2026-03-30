# CRATONCTL Design Document

## 1. Product / UX Design

### 1.1 Role

`cratonctl` is a thin operator CLI for `cratond`.

Its job is to:
- inspect daemon health and current state
- inspect services and histories
- trigger safe daemon actions
- request remediation through the daemon
- help diagnose a service via daemon-provided diagnostics
- expose maintenance and backup-related actions without bypassing the daemon

It is not a controller. It is an operator tool layered on top of the existing localhost HTTP API.

### 1.2 Operator Jobs

Primary operator jobs:
- "Is the daemon healthy?"
- "What is broken right now?"
- "What changed recently?"
- "Diagnose service X"
- "Trigger recovery / backup / disk monitor now"
- "Restart service X through daemon policy path"
- "Set or clear maintenance"
- "Clear breaker / flapping state"

### 1.3 UX Principles

`cratonctl` should be:
- concise by default
- predictable
- scriptable
- read-only by default
- explicit for mutating actions
- helpful on failures without being chatty

UX expectations:
- human-readable output by default
- stable `--json` mode for automation
- table output for lists
- one command = one obvious action
- no hidden retries for mutating actions
- no surprise writes to local files

### 1.4 Scope by Phase

MVP:
- `health`
- `status`
- `services`
- `service <id>`
- `history recovery|backup|remediation`
- `diagnose <service>`
- `trigger <task>`
- `restart <service>`
- `maintenance set|clear`
- `breaker clear <service>`
- `flapping clear <service>`
- `backup run`
- `backup unlock`
- `disk cleanup`
- `--json`, `--quiet`, `--no-color`

Phase 2:
- `watch`
- richer service summaries
- better relative-time rendering
- machine-readable error codes
- shell completions
- auth/config introspection command

Nice-to-have:
- `backup status`
- `maintenance list`
- `status --watch`
- `--output table|json`
- clipboard-friendly output modes
- markdown incident/history export

## 2. CLI Command Model

### Global Flags

Global flags:
- `--url <url>`: daemon base URL
- `--token <token>`: bearer token for mutating requests
- `--json`: machine-readable output
- `--quiet`: suppress human chatter, print only essential result
- `--no-color`: disable ANSI color
- `--timeout <duration>`: request timeout for this invocation

### Read-only Commands

#### `cratonctl health`

Purpose:
- quick daemon health check

HTTP:
- `GET /health`

Flags:
- global flags only

Output:
- human: one-line status + reason
- json: raw normalized health object

Exit codes:
- `0` healthy
- `1` daemon responded but unhealthy/unavailable
- `2` transport/client error

#### `cratonctl status`

Purpose:
- top-level summary: daemon health + key counts + current backup phase

HTTP:
- `GET /health`
- `GET /api/v1/state`

Flags:
- `--json`

Output:
- human: summary block
- json: combined object `{health, state_summary}`

Exit codes:
- `0` request succeeded
- `2` transport/client error

#### `cratonctl services`

Purpose:
- list services and current statuses

HTTP:
- `GET /api/v1/state`

Flags:
- `--json`
- `--wide`

Output:
- human: table `ID | Status | Breaker | Maintenance | Last restart`
- json: array of service objects

Exit codes:
- `0` success
- `2` transport/client error

#### `cratonctl service <id>`

Purpose:
- inspect one service in detail

HTTP:
- `GET /api/v1/state`

Flags:
- `--json`

Output:
- human: structured block for that service
- json: one service object

Exit codes:
- `0` found
- `1` service not found
- `2` transport/client error

#### `cratonctl history recovery`
#### `cratonctl history backup`
#### `cratonctl history remediation`

Purpose:
- show typed history arrays

HTTP:
- `GET /api/v1/history/recovery`
- `GET /api/v1/history/backup`
- `GET /api/v1/history/remediation`

Flags:
- `--json`
- `--limit <n>`

Output:
- human: table
- json: raw array

Exit codes:
- `0` success
- `2` transport/client error

#### `cratonctl diagnose <service>`

Purpose:
- show daemon-provided diagnosis for a service

HTTP:
- `GET /api/v1/diagnose/{service}`

Flags:
- `--json`

Output:
- human: service, unit, active flag, short sections
- json: raw diagnose payload

Exit codes:
- `0` success
- `1` service/diagnose error from daemon
- `2` transport/client error

### Mutating Commands

#### `cratonctl trigger <task>`

Purpose:
- trigger daemon task immediately

Supported tasks:
- `recovery`
- `backup`
- `disk-monitor`
- `apt-updates`
- `docker-updates`
- `daily-summary`

HTTP:
- `POST /trigger/{task}`

Flags:
- `--json`

Output:
- human: accepted/rejected
- json: daemon response

Exit codes:
- `0` accepted
- `1` daemon rejected / unavailable
- `2` transport/client error

#### `cratonctl restart <service>`

Purpose:
- request daemon-mediated restart of a service

HTTP:
- `POST /api/v1/remediate`
  - action = `RestartService`

Flags:
- `--reason <text>` optional
- `--json`

Output:
- human: accepted
- json: daemon response

Exit codes:
- `0` accepted
- `1` rejected / unauthorized
- `2` transport/client error

#### `cratonctl maintenance set <service> --for <duration> --reason <text>`

Purpose:
- request maintenance mode for a service

HTTP:
- `POST /api/v1/remediate`
  - action = `MarkMaintenance`

Flags:
- `--for <duration>` required in CLI UX, even if daemon currently has fixed 1h semantics
- `--reason <text>` required
- `--json`

Note:
- If daemon still only supports fixed maintenance duration, CLI should state that clearly and either:
  - accept only `1h`, or
  - warn that duration is normalized to daemon default

Exit codes:
- `0` accepted
- `1` rejected / unauthorized
- `2` transport/client error

#### `cratonctl maintenance clear <service>`

HTTP:
- `POST /api/v1/remediate`
  - action = `ClearMaintenance`

#### `cratonctl breaker clear <service>`

HTTP:
- `POST /api/v1/remediate`
  - action = `ClearBreaker`

#### `cratonctl flapping clear <service>`

HTTP:
- `POST /api/v1/remediate`
  - action = `ClearFlapping`

#### `cratonctl backup run`

HTTP:
- `POST /trigger/backup`

#### `cratonctl backup unlock`

HTTP:
- `POST /api/v1/remediate`
  - action = `ResticUnlock`

#### `cratonctl disk cleanup`

HTTP:
- `POST /api/v1/remediate`
  - action = `RunDiskCleanup`

### Exit Code Policy

Recommended coarse policy:
- `0`: successful command execution / accepted mutation
- `1`: daemon returned failure, unauthorized, not found, or unhealthy result
- `2`: client-side failure (invalid args, network error, parse error, timeout)

## 3. Output Design

### 3.1 Human Output

Default output should optimize for operators reading in a terminal:
- compact summary first
- tables for lists
- explicit labels for causes and recommendations

Use color sparingly:
- green: healthy / accepted
- yellow: warning / maintenance / degraded
- red: unavailable / failed / rejected

ASCII should remain readable with `--no-color`.

### 3.2 JSON Output

`--json` should:
- disable human tables and prose
- print a single JSON value to stdout
- keep stderr for real client errors only

Use `--json` for:
- scripting
- monitoring wrappers
- piping into `jq`

Do not add YAML in MVP:
- extra surface area
- unclear operator benefit
- JSON already covers automation

Recommendation:
- support `--json`
- do not support `--output yaml`
- optional later: `--output table|json`

### 3.3 `--quiet`

Use `--quiet` for mutating commands:
- print only essential acknowledgment on stdout
- keep stderr for errors

Examples:
- `cratonctl restart ntfy --quiet`
- `cratonctl trigger backup --quiet`

### 3.4 `--watch`

`--watch` is useful, but not MVP-critical.

Recommendation:
- add later for `status` and `services`
- interval default 2s or 5s
- incompatible with `--json` in MVP unless NDJSON is explicitly designed

### 3.5 Stderr Policy

Rules:
- stdout = command result
- stderr = transport/client/config/auth failures
- do not print tables to stderr
- do not print token, auth header, or local token path contents

## 4. Auth / Config Model

### 4.1 Data Needed

`cratonctl` needs:
- daemon URL
- bearer token for mutating requests

### 4.2 Source Priority

Recommended priority:
1. explicit CLI flags
2. env vars
3. config file
4. token autodiscovery file
5. hardcoded defaults

Proposed sources:
- `--url`
- `--token`
- `CRATONCTL_URL`
- `CRATONCTL_TOKEN`
- config file `~/.config/cratonctl/config.toml`
- token autodiscovery: `/var/lib/craton/remediation-token`
- default URL: `http://127.0.0.1:18800`

### 4.3 Safe Behavior

Rules:
- read-only commands do not require token
- mutating commands fail fast if no token is available
- token is never echoed back
- token is never printed in debug/error output
- "unauthorized" should mention missing/invalid token without revealing candidate values

Recommended UX:
- if token missing for mutating command:
  - stderr: `token required for mutating command; use --token, CRATONCTL_TOKEN, or token file`
  - exit `2`

### 4.4 Config File

Recommended optional config file:

```toml
url = "http://127.0.0.1:18800"
token_file = "/var/lib/craton/remediation-token"
default_output = "table"
no_color = false
timeout = "5s"
```

Do not store raw token in config by default. Prefer `token_file`.

## 5. Language Choice: Rust vs Go

### Rust

Pros:
- same ecosystem and repo as `cratond`
- consistent packaging story
- easy single-binary distribution
- strong DTO typing
- no second language in maintenance path

Cons:
- slower iteration than Go for an operator CLI
- table/rendering ergonomics can be a bit heavier

### Go

Pros:
- fast MVP velocity
- very comfortable for operator CLIs
- excellent standard library for HTTP + JSON + flags
- easy static binary distribution

Cons:
- second language in project
- split tooling and CI
- weaker alignment with existing codebase

### Recommendation

Recommended MVP language: **Rust**.

Why:
- `cratonctl` is thin, so implementation complexity is moderate
- staying in Rust keeps repo, CI, packaging, release flow, and maintenance simpler
- same team and same project context

If the goal were fastest possible throwaway prototype, Go would be defensible. For a deployable companion tool to `cratond`, Rust is the better long-term default.

## 6. Internal Architecture

Recommended shape:
- same repository
- separate binary: `cratonctl`
- minimal module split

Suggested structure:

```text
src/bin/cratonctl.rs
src/cratonctl/
  mod.rs
  cli.rs
  client.rs
  auth.rs
  output.rs
  error.rs
  dto.rs
  commands/
    health.rs
    status.rs
    services.rs
    history.rs
    diagnose.rs
    trigger.rs
    remediate.rs
```

Responsibilities:
- `cli.rs`: argument parsing and command dispatch
- `client.rs`: HTTP client wrapper
- `auth.rs`: URL/token/config loading
- `output.rs`: renderers for human/table/json
- `dto.rs`: typed response/request structs
- `commands/*`: command-level orchestration only

Recommendation:
- same repo, separate binary
- do not share internal daemon modules directly
- duplicate only public HTTP DTOs needed by the client

## 7. Constraints / Anti-goals

`cratonctl` must not:
- edit daemon state files directly
- write `maintenance.json` directly
- write `backup-state.json` directly
- call `systemctl` directly as a control path
- bypass daemon HTTP API
- store or print raw bearer token
- become a second controller with independent automation
- implement policy logic that competes with the daemon
- retry mutating actions in surprising ways

It is a client, not a new control loop.

## 8. Recommended Implementation Order

1. auth/config loader
2. HTTP client
3. `health`
4. `status`
5. `services` / `service`
6. `history *`
7. `diagnose`
8. mutating commands
9. polish: tables, colors, `--quiet`

