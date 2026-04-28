# cratonctl

`cratonctl` — тонкий operator CLI для `cratond`.

Он работает только через HTTP API демона и не:

- редактирует state-файлы напрямую
- вызывает `systemctl` в обход демона
- дублирует daemon policy
- становится вторым контроллером системы

## Глобальные флаги

- `--url <url>` — базовый URL демона
- `--token <token>` — Bearer token для mutating-команд
- `--token-file <path>` — читать токен из файла
- `--json` — печатать один JSON document
- `--quiet` — минимизировать human-readable вывод
- `--no-color` — отключить цветной вывод

## Разрешение URL и токена

### URL

Приоритет:

1. `--url`
2. `CRATONCTL_URL`
3. `http://127.0.0.1:18800`

Поддерживается только `http://`.

### Bearer token

Приоритет:

1. `--token`
2. `CRATONCTL_TOKEN`
3. `--token-file`
4. `/var/lib/craton/remediation-token`

Read-only команды работают без токена.  
Mutating-команды без токена завершаются ошибкой.

## Команды

### `cratonctl health`

Проверить health демона.

HTTP:

- `GET /health`

Примеры:

```bash
cratonctl health
cratonctl --json health
```

Exit codes:

- `0` — daemon healthy
- `1` — daemon unavailable
- `2` — локальная ошибка клиента / auth / parse / transport

### `cratonctl auth status`

Проверить URL, token resolution и mutating readiness без побочных эффектов.

Примеры:

```bash
cratonctl auth status
cratonctl auth status --token-file /var/lib/craton/remediation-token
cratonctl --json auth status
```

### `cratonctl status`

Краткий summary:

- health
- число сервисов
- число degraded сервисов
- backup phase
- disk usage
- `startup_kind`
- `outbox_overflow`
- notify degradation

HTTP:

- `GET /health`
- `GET /api/v1/state`

Примеры:

```bash
cratonctl status
cratonctl --quiet status
cratonctl --json status
```

Exit codes:

- `0` — daemon reachable and command succeeded
- `1` — daemon unhealthy
- `2` — локальная ошибка клиента

### `cratonctl services`

Список сервисов в табличном виде.

HTTP:

- `GET /api/v1/state`

Примеры:

```bash
cratonctl services
cratonctl --json services
```

### `cratonctl service <id>`

Подробное состояние одного сервиса.

HTTP:

- `GET /api/v1/state`

Примеры:

```bash
cratonctl service ntfy
cratonctl --json service ntfy
```

### `cratonctl history recovery`
### `cratonctl history backup`
### `cratonctl history remediation`

История daemon-side событий.

HTTP:

- `GET /api/v1/history/recovery`
- `GET /api/v1/history/backup`
- `GET /api/v1/history/remediation`

Примеры:

```bash
cratonctl history recovery
cratonctl history backup
cratonctl history remediation
cratonctl --json history backup
```

### `cratonctl diagnose <service>`

Попросить daemon-side диагностику для сервиса.

HTTP:

- `GET /api/v1/diagnose/{service}`

Важно:

- сервис должен существовать в конфиге демона
- неизвестный сервис -> daemon-side error

Примеры:

```bash
cratonctl diagnose ntfy
cratonctl --json diagnose ntfy
```

### `cratonctl doctor`

Безопасный preflight без mutating requests.

Проверяет:

- reachable ли daemon URL
- отвечает ли `/health`
- отвечает ли `/api/v1/state`
- доступен ли token path
- готовы ли read-only и mutating paths

Примеры:

```bash
cratonctl doctor
cratonctl --json doctor
```

Exit codes:

- `0` — нет fail checks
- `1` — есть fail checks
- `2` — локальная ошибка клиента

### `cratonctl init`

Первичная инициализация сервера.

Что делает:

- требует root
- создаёт config dir, state dir, runtime dir
- создаёт `config.toml`, если его ещё нет
- создаёт remediation token, если его ещё нет
- создаёт systemd unit, если его ещё нет
- запускает `systemctl daemon-reload`, если unit был создан

Что не делает:

- не стартует daemon
- не включает daemon в autostart
- не перезаписывает существующие config/token/unit файлы

Флаги:

- `--non-interactive`
- `--config-dir <path>`
- `--state-dir <path>`

Примеры:

```bash
sudo cratonctl init
sudo cratonctl init --non-interactive
sudo cratonctl init --config-dir /tmp/craton/etc --state-dir /tmp/craton/state
sudo cratonctl --json init --non-interactive
```

### `cratonctl trigger <task>`

Запустить daemon task вне расписания.

HTTP:

- `POST /trigger/{task}`

Auth:

- Bearer token обязателен

Частые task names:

- `recovery`
- `backup`
- `disk-monitor`
- `apt-updates`
- `docker-updates`
- `daily-summary`

Примеры:

```bash
cratonctl trigger recovery
cratonctl trigger backup
cratonctl trigger daily-summary --token-file /var/lib/craton/remediation-token
```

Response semantics:

- `200` — daemon принял запрос
- `409` — policy rejection
- `503` — control loop unavailable
- `504` — control loop did not respond in time

`202` больше не используется.

### `cratonctl restart <service>`

HTTP:

- `POST /api/v1/remediate`

Action:

- `RestartService`

Примеры:

```bash
cratonctl restart ntfy
cratonctl restart ntfy --token-file /var/lib/craton/remediation-token
```

### `cratonctl docker restart <container>`

HTTP:

- `POST /api/v1/remediate`

Action:

- `DockerRestart`

Примеры:

```bash
cratonctl docker restart continuwuity
cratonctl docker restart continuwuity --token-file /var/lib/craton/remediation-token
```

### `cratonctl maintenance set <service> --reason <text>`

HTTP:

- `POST /api/v1/remediate`

Action:

- `MarkMaintenance`

Примеры:

```bash
cratonctl maintenance set ntfy --reason "ручная диагностика"
```

### `cratonctl maintenance clear <service>`

Action:

- `ClearMaintenance`

Примеры:

```bash
cratonctl maintenance clear ntfy
```

### `cratonctl breaker clear <service>`

Action:

- `ClearBreaker`

Примеры:

```bash
cratonctl breaker clear ntfy
```

### `cratonctl flapping clear <service>`

Action:

- `ClearFlapping`

Примеры:

```bash
cratonctl flapping clear ntfy
```

### `cratonctl backup run`

Это alias поверх trigger path.

HTTP:

- `POST /trigger/backup`

Примеры:

```bash
cratonctl backup run
cratonctl --json backup run
```

### `cratonctl backup unlock`

HTTP:

- `POST /api/v1/remediate`

Action:

- `ResticUnlock`

Примеры:

```bash
cratonctl backup unlock
```

### `cratonctl disk cleanup`

HTTP:

- `POST /api/v1/remediate`

Action:

- `RunDiskCleanup`

Примеры:

```bash
cratonctl disk cleanup
```

## Auth requirements

Read-only команды без токена:

- `health`
- `auth status`
- `status`
- `services`
- `service <id>`
- `history *`
- `diagnose <service>`
- `doctor`

Mutating-команды с токеном:

- `trigger <task>`
- `restart <service>`
- `docker restart <container>`
- `maintenance set|clear`
- `breaker clear`
- `flapping clear`
- `backup run`
- `backup unlock`
- `disk cleanup`

## HTTP response semantics

### Trigger

`POST /trigger/{task}`:

- `200` — accepted by daemon
- `401` — unauthorized
- `404` — unknown task
- `409` — rejected by policy
- `503` — control loop unavailable
- `504` — control loop timeout

### Remediation

`POST /api/v1/remediate`:

- `200` — accepted by daemon
- `401` — unauthorized
- `400` — invalid JSON / unknown action / malformed body
- `409` — rejected by policy
- `500` — internal ACK error
- `503` — control loop unavailable
- `504` — control loop timeout

## JSON mode

`--json` печатает один JSON document в stdout.

Если CLI падает локально, формат ошибки будет таким:

```json
{
  "error": {
    "kind": "auth",
    "code": "token_not_provided",
    "message": "token required for mutating command; use --token, CRATONCTL_TOKEN, or --token-file",
    "exit_code": 2
  }
}
```

## Exit codes

- `0` — success
- `1` — daemon-side rejection / unavailable / unhealthy result
- `2` — local usage / config / auth / transport / parse error

Примечание:

- `health` возвращает `1`, если daemon отвечает, но unhealthy
- `status` возвращает `1`, если daemon reachable, но health не `ok`
- `doctor` возвращает `1`, если есть failed checks

## Ограничения

- CLI не поддерживает `https://`
- CLI не редактирует daemon state-файлы напрямую
- CLI не пытается “умно чинить” систему за спиной демона
- CLI не печатает Bearer token
