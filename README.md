# Craton

Deterministic infrastructure supervision for a home ARM64 server.

## What is Craton

Craton is a production-oriented Rust project for operating a self-hosted Linux server with systemd. It is built around `cratond`, a local daemon that monitors service health, applies dependency-aware recovery policy, runs a crash-safe backup state machine, watches disk pressure, and exposes a localhost HTTP API for operator tooling.

The project exists for the class of servers that are important enough to automate carefully, but small enough that complexity is a liability. The target environment is a home server or personal VPS where correctness, recoverability, and operator clarity matter more than feature sprawl.

`cratond` is paired with `cratonctl`, a thin operator CLI layered on top of the daemon API. The CLI does not edit daemon state files, does not bypass policy, and does not become a second controller. The daemon remains the source of truth.

## Architecture

Craton is intentionally conservative:

- Single-writer mutable state in the control loop
- Reducer / effect separation
- No shell execution, no async runtime, no plugin system
- Deterministic, crash-safe, explainable behavior
- Four long-lived threads, bounded queues and bounded histories

The formal specification lives in [docs/architecture/CONSTITUTION.md](docs/architecture/CONSTITUTION.md). The broader architectural walkthrough lives in [docs/architecture/ARCHITECTURE.md](docs/architecture/ARCHITECTURE.md).

## Features

- Health monitoring with dependency-aware recovery
- Circuit breaker and flapping control
- Backup FSM with `restic` and startup crash recovery
- Disk monitoring and cleanup
- Durable `ntfy` outbox with replay on restart
- Maintenance mode and breaker / flapping reset actions
- Bearer-authenticated mutating API
- systemd `sd_notify` and watchdog integration
- Typed health endpoint and typed snapshot fields for operator tooling

## cratonctl

`cratonctl` is the operator CLI for `cratond`.

Common commands:

```bash
cratonctl health
cratonctl status
cratonctl services
cratonctl doctor
cratonctl trigger recovery
cratonctl init
```

Mutating commands require a Bearer token. Read-only commands do not.

Full reference: [docs/cratonctl.md](docs/cratonctl.md)

## Installation

### Prerequisites

- Rust toolchain
- `aarch64-unknown-linux-gnu` target for ARM64 deploys
- `cargo-zigbuild` for cross-compilation from x86_64 Windows/Linux hosts
- ARM64 Linux server with systemd

### First-time setup

For a fresh server, install the binaries and run:

```bash
sudo cratonctl init
```

This prepares:

- `/etc/craton/config.toml`
- `/var/lib/craton/remediation-token`
- `/etc/systemd/system/cratond.service`
- runtime and state directories

It does not start or enable the daemon automatically.

### Deploy

This repository includes deploy helpers:

- [scripts/deploy.ps1](scripts/deploy.ps1) for Windows / PowerShell
- [scripts/install-remote.sh](scripts/install-remote.sh) for the Linux server

Detailed deploy instructions: [docs/deploy.md](docs/deploy.md)

## Configuration

The main config file is:

```text
/etc/craton/config.toml
```

Primary sections:

- `[daemon]`
- `[ntfy]`
- `[backup]`
- `[backup.retention]`
- `[disk]`
- `[updates]`
- `[ai]`
- `[[service]]`
- `[service.probe]`

Reference example: [config.example.toml](config.example.toml)

Formal behavioral contracts are documented in [docs/architecture/CONSTITUTION.md](docs/architecture/CONSTITUTION.md).

## API

`cratond` binds to `127.0.0.1:18800` by default.

| Method | Path | Auth | Purpose |
|---|---|---|---|
| `GET` | `/health` | No | Typed daemon health |
| `GET` | `/api/v1/state` | No | Current snapshot |
| `GET` | `/api/v1/history/recovery` | No | Recovery history |
| `GET` | `/api/v1/history/backup` | No | Backup history |
| `GET` | `/api/v1/history/remediation` | No | Remediation history |
| `GET` | `/api/v1/diagnose/{service}` | No | Service diagnostics for configured services only |
| `POST` | `/trigger/{task}` | Bearer | Trigger daemon task |
| `POST` | `/api/v1/remediate` | Bearer | Request remediation action |

Mutating endpoints no longer return `202 Accepted`. They now wait for control-loop acknowledgement and return:

- `200` when the request was accepted by daemon policy
- `409` when policy rejects the request
- `503` when the control loop is unavailable
- `504` when the control loop does not acknowledge in time

`/health` now checks:

- snapshot freshness
- outbox overflow
- probe-cycle freshness
- shutdown state
- daemon degradation reasons

## Runtime and Safety Notes

- Existing remediation token read failures are fatal on startup
- Missing runtime/state directories are fatal on startup
- Signal adapter startup failure is fatal
- Critical file writes require directory `fsync`; rename without directory sync is not treated as success
- Bearer token comparison is constant-time
- `/api/v1/state` no longer exposes `start_mono`
- Snapshot includes `startup_kind` and notification channel status fields

## Project Status

Deployed and running in production on a home ARM64 server. Current work is focused on hardening, operator UX, and documentation accuracy rather than broad feature expansion.

## Documentation

- [docs/architecture/CONSTITUTION.md](docs/architecture/CONSTITUTION.md)
- [docs/architecture/ARCHITECTURE.md](docs/architecture/ARCHITECTURE.md)
- [docs/cratonctl.md](docs/cratonctl.md)
- [docs/deploy.md](docs/deploy.md)
- [OPERATOR.md](OPERATOR.md)

## License

MIT

## Crafted by Cherry 🍒
