# Cratond

`cratond` — systemd-ориентированный Rust-демон для мониторинга и управления домашним ARM64 сервером. Он следит за сервисами, запускает recovery, управляет backup FSM через `restic`, контролирует диск, хранит истории и отдаёт локальный HTTP API.

`cratonctl` — тонкий operator CLI поверх этого API. Он не редактирует state-файлы напрямую, не вызывает `systemctl` в обход демона и не обходит архитектуру демона.

## Что есть в проекте

- `cratond`:
  - health monitoring сервисов
  - recovery и controlled restart
  - dependency-aware policy
  - breaker / flapping control
  - backup FSM с crash recovery
  - disk monitoring и cleanup
  - durable outbox для уведомлений
  - maintenance mode
  - remediation API с Bearer token
  - histories и diagnose endpoint
- `cratonctl`:
  - read-only статус и истории
  - trigger задач
  - maintenance / breaker / flapping команды
  - backup run / unlock
  - `doctor` preflight / sanity-check
  - JSON-режим для автоматизации

## Что это не делает

- не редактирует `backup-state.json`, `maintenance.json` и другие state-файлы вручную
- не даёт второму клиенту стать отдельным контроллером системы
- не предоставляет TUI, watch mode или autocomplete в текущем коде
- не документирует ZeroClaw как готовую интеграцию: в конфиге сейчас есть AI-поля с `picoclaw_url`

## Quick Start

### 1. Собрать бинарники

Нативно:

```bash
cargo build --release --bin cratond --bin cratonctl
```

Кросс-компиляция для ARM64:

```bash
rustup target add aarch64-unknown-linux-gnu
cargo install cross
cross build --release --target aarch64-unknown-linux-gnu --bin cratond --bin cratonctl
```

### 2. Подготовить сервер

```bash
sudo mkdir -p /etc/craton /var/lib/craton /run/craton
sudo install -m 0755 target/aarch64-unknown-linux-gnu/release/cratond /usr/local/bin/cratond
sudo install -m 0755 target/aarch64-unknown-linux-gnu/release/cratonctl /usr/local/bin/cratonctl
sudo install -m 0644 config.example.toml /etc/craton/config.toml
sudo install -m 0644 deploy/cratond.service /etc/systemd/system/cratond.service
sudo chmod 600 /etc/craton/config.toml
```

### 3. Отредактировать конфиг

Проверь минимум:

- `ntfy.url` и `ntfy.topic`
- `backup.restic_repo`
- `backup.restic_password_file`
- `backup.paths`
- список `[[service]]`
- `service.unit`, `service.kind`, `service.probe`

Актуальный пример лежит в [config.example.toml](/a:/cratond/config.example.toml).

### 4. Запустить демон

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now cratond
sudo systemctl status cratond
```

### 5. Сделать smoke-test

```bash
curl -s http://127.0.0.1:18800/health
cratonctl health
cratonctl status
cratonctl services
```

## Бинарники

- `cratond`: демон, читает `config.toml`, поднимает HTTP API и control loop
- `cratonctl`: CLI-клиент к уже работающему `cratond`

## `cratonctl`

`cratonctl` использует только существующий HTTP API `cratond`.
Это operator CLI, а не второй контроллер системы.

URL resolution:

1. `--url`
2. `CRATONCTL_URL`
3. `http://127.0.0.1:18800`

Token resolution для mutating-команд:

1. `--token`
2. `CRATONCTL_TOKEN`
3. `--token-file`
4. `/var/lib/craton/remediation-token`

Поддерживаются:

- human-readable output по умолчанию
- `--json` для script-friendly automation
- `--quiet` для краткого human output
- `--no-color` для полного отключения цвета

### Основные команды `cratonctl`

Read-only:

```bash
cratonctl health
cratonctl status
cratonctl services
cratonctl service ntfy
cratonctl history recovery
cratonctl history backup
cratonctl history remediation
cratonctl diagnose ntfy
cratonctl doctor
```

Mutating:

```bash
cratonctl restart ntfy
cratonctl maintenance set ntfy --reason "ручная диагностика"
cratonctl maintenance clear ntfy
cratonctl breaker clear ntfy
cratonctl flapping clear ntfy
cratonctl backup run
cratonctl backup unlock
cratonctl disk cleanup
cratonctl trigger recovery
cratonctl trigger backup
```

Подробности: [docs/cratonctl.md](/a:/cratond/docs/cratonctl.md).

### Что `cratonctl` не делает

- не редактирует daemon state вручную
- не дублирует daemon policy
- не предоставляет TUI, watch mode, completions или YAML output
- не поддерживает `https://` в текущем CLI

## Конфиг

Формат: TOML. По умолчанию демон стартует так:

```bash
cratond /etc/craton/config.toml
```

Ключевые секции:

- `[daemon]`: адрес API, watchdog, уровень логов
- `[ntfy]`: доставка operator-facing alert'ов
- `[backup]` и `[backup.retention]`: расписание и параметры `restic`
- `[disk]`: пороги warning / critical
- `[updates]`: scheduled tasks
- `[ai]`: текущие AI-поля, включая `picoclaw_url`
- `[[service]]`: сервисы, probe, dependencies, breaker, backup_stop

Пример:

```toml
[daemon]
listen = "127.0.0.1:18800"
watchdog = true
log_level = "info"

[[service]]
id = "ntfy"
name = "NTFY"
unit = "ntfy.service"
kind = "systemd"
severity = "critical"

[service.probe]
type = "http"
url = "http://127.0.0.1:8080/v1/health"
timeout_secs = 5
expect_status = 200
```

Для реального деплоя ориентируйся на [config.example.toml](/a:/cratond/config.example.toml).

## Systemd unit

Готовый unit: [deploy/cratond.service](/a:/cratond/deploy/cratond.service).

Текущий unit использует:

- `Type=notify`
- `ExecStart=/usr/local/bin/cratond /etc/craton/config.toml`
- `WatchdogSec=30`
- `StateDirectory=craton`
- `RuntimeDirectory=craton`
- `LogsDirectory=craton`

Подробный деплой: [docs/deploy.md](/a:/cratond/docs/deploy.md).

## HTTP API overview

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

### `/health`

`/health` — typed health endpoint демона, а не summary всех degraded сервисов.

- `200 {"status":"ok","reason":"ok"}` если snapshot свежий, overflow нет и shutdown не идёт
- `503 {"status":"unavailable","reason":"stale_snapshot"}` если control loop не публикует свежий snapshot
- `503 {"status":"unavailable","reason":"outbox_overflow"}` если durable outbox переполнен
- `503 {"status":"unavailable","reason":"shutting_down"}` если daemon завершает работу

### `/api/v1/state`

Возвращает полный snapshot-объект, включая:

- `services`
- `backup_phase`
- `disk_usage_percent`
- `backup_history`
- `recovery_history`
- `remediation_history`
- `snapshot_epoch_secs`
- `outbox_overflow`
- `shutting_down`

## Remediation overview

Mutating API требует Bearer token из файла `/var/lib/craton/remediation-token`, если в конфиге не задан другой `ai.token_path`.

`cratonctl` для mutating-команд использует этот токен автоматически, если он доступен.
Read-only команды не требуют токен.

Пример raw API:

```bash
TOKEN="$(cat /var/lib/craton/remediation-token)"
curl -sS -X POST http://127.0.0.1:18800/api/v1/remediate \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"action":"RestartService","target":"ntfy","reason":"ручная диагностика"}'
```

Поддерживаемые action'ы:

- `RestartService`
- `DockerRestart`
- `TriggerBackup`
- `MarkMaintenance`
- `ClearMaintenance`
- `ClearBreaker`
- `ClearFlapping`
- `RunDiskCleanup`
- `ResticUnlock`

Rate limits в текущем коде:

- `RestartService`: 3 в час на сервис
- `DockerRestart`: 1 в час
- `TriggerBackup`: 1 в сутки
- `MarkMaintenance`: 5 в час

## Backup / recovery overview

Recovery:

- unhealthy сервис может быть рестартован
- `BlockedByDep` не рестартуется, пока не восстановится зависимость
- breaker и cooldown ограничивают повторные рестарты
- maintenance подавляет auto-remediation

Backup FSM:

- optional `restic unlock`
- остановка сервисов с `backup_stop = true`
- `restic backup`
- retention
- optional verify
- возврат сервисов в исходное состояние
- crash recovery по `backup-state.json`

## Deploy basics

Практический минимум:

1. собрать `cratond` и `cratonctl`
2. установить `config.toml`
3. установить `deploy/cratond.service`
4. проверить `restic`, `systemctl`, `journalctl`, `ntfy`
5. запустить `systemctl enable --now cratond`
6. проверить `/health`, `cratonctl status`, `cratonctl history backup`

## Файлы на диске

- `/usr/local/bin/cratond`
- `/usr/local/bin/cratonctl`
- `/etc/craton/config.toml`
- `/var/lib/craton/backup-state.json`
- `/var/lib/craton/maintenance.json`
- `/var/lib/craton/alert-outbox.jsonl`
- `/var/lib/craton/remediation-token`
- `/run/craton/llm_context.json`

## Ограничения и нецели

- API рассчитан на localhost и не предоставляет TLS
- `cratonctl` не реализует TUI
- `cratonctl` не имеет watch mode в текущем коде
- `cratonctl` не поддерживает `https://` и работает только через daemon HTTP API
- поля `[ai]` в конфиге существуют, но это не означает готовую ZeroClaw integration
- архитектурный источник истины остаётся в `CONSTITUTION.md`

## Дополнительные документы

- [docs/cratonctl.md](/a:/cratond/docs/cratonctl.md)
- [docs/deploy.md](/a:/cratond/docs/deploy.md)
- [CRATONCTL_DESIGN.md](/a:/cratond/CRATONCTL_DESIGN.md)
- [HANDOFF_FOR_CRATONCTL.md](/a:/cratond/HANDOFF_FOR_CRATONCTL.md)
