# cratonctl

`cratonctl` — операторский CLI для `cratond`. Это тонкий клиент к локальному
HTTP API демона. Он не редактирует state-файлы напрямую, не вызывает
`systemctl` как обходной путь и не дублирует daemon policy.

## Назначение

`cratonctl` нужен для трёх основных задач:

- быстро посмотреть состояние демона и сервисов
- быстро понять, готов ли mutating path по auth/token
- вручную инициировать safe actions через уже существующий policy слой `cratond`
- получать machine-readable JSON для скриптов и автоматизации

Anti-goals:

- не становиться вторым контроллером системы
- не редактировать `maintenance.json`, `backup-state.json` и другие state-файлы
- не предоставлять TUI, watch mode, shell completions, YAML/XML output
- не поддерживать `https://` в текущем CLI

## Как он работает

`cratonctl` использует только текущие HTTP endpoints:

- `GET /health`
- `GET /api/v1/state`
- `GET /api/v1/history/recovery`
- `GET /api/v1/history/backup`
- `GET /api/v1/history/remediation`
- `GET /api/v1/diagnose/{service}`
- `POST /trigger/{task}`
- `POST /api/v1/remediate`

## Глобальные флаги

- `--url <url>`: базовый URL демона
- `--token <token>`: Bearer token для mutating-команд
- `--token-file <path>`: путь к файлу токена
- `--json`: JSON output вместо human-readable текста
- `--quiet`: короткий human output
- `--no-color`: полностью отключить цветной human-readable output

## Разрешение URL и токена

### URL

Приоритет:

1. `--url`
2. `CRATONCTL_URL`
3. `http://127.0.0.1:18800`

В текущем MVP поддерживается только `http://`.

### Bearer token

Приоритет:

1. `--token`
2. `CRATONCTL_TOKEN`
3. `--token-file <path>`
4. autodiscovery: `/var/lib/craton/remediation-token`

Read-only команды могут работать без токена. Mutating-команды без токена
завершаются ошибкой auth/config.

CLI различает:

- token not provided
- explicit token file missing
- token file unreadable
- token file invalid or empty

Для прозрачной диагностики auth path:

```bash
cratonctl auth status
```

Эта команда показывает:

- daemon URL
- token resolution order
- autodiscovery token path
- token file exists / missing / unreadable / invalid
- mutating commands available: yes/no
- human-readable explanation why not

## Read-only команды

### `cratonctl health`

Показывает typed health самого демона.

HTTP:

- `GET /health`

Примеры:

```bash
cratonctl health
cratonctl --json health
```

Ожидаемый human output:

```text
ok
```

или

```text
unavailable: stale_snapshot
```

Exit codes:

- `0`: daemon healthy
- `1`: daemon unavailable
- `2`: локальная ошибка клиента, транспорта или парсинга

### `cratonctl status`

Краткий summary:

- health
- startup kind
- количество сервисов
- число degraded сервисов
- backup phase
- disk usage
- notifications summary
- `shutting_down`
- `outbox_overflow`

HTTP:

- `GET /health`
- `GET /api/v1/state`

Примеры:

```bash
cratonctl status
cratonctl --quiet status
cratonctl --json status
```

Human-readable status дополнительно:

- показывает `startup_kind` как короткую operator-friendly строку вроде `Host reboot` или `Daemon restart`
- показывает состояние notifier только если канал деградировал, например `Notifications: DEGRADED (5 consecutive failures)`
- не пытается превращать notify timestamps в отдельный экран мониторинга

### `cratonctl services`

Список сервисов в табличном виде.

HTTP:

- `GET /api/v1/state`

Колонки:

- `SERVICE`
- `STATUS`
- `SUMMARY`

### `cratonctl service <id>`

Подробное состояние одного сервиса.

HTTP:

- `GET /api/v1/state`

Показывает:

- `service`
- `status`
- `summary`
- дополнительные поля для `suppressed`, `blocked_by_dep`, `unhealthy`

В human-readable output raw monotonic поля не показываются.
В `--json` они остаются как есть, если пришли из daemon API.

### `cratonctl history recovery`

HTTP:

- `GET /api/v1/history/recovery`

### `cratonctl history backup`

HTTP:

- `GET /api/v1/history/backup`

### `cratonctl history remediation`

HTTP:

- `GET /api/v1/history/remediation`

### `cratonctl diagnose <service>`

Daemon-side диагностика по `systemctl` и `journalctl`.

HTTP:

- `GET /api/v1/diagnose/{service}`

Показывает:

- имя сервиса
- unit
- active yes/no
- `systemctl status`
- последние 50 строк journal

### `cratonctl doctor`

Безопасный preflight для оператора.

Проверяет только:

- reachable ли daemon URL
- отвечает ли `GET /health`
- отвечает и парсится ли `GET /api/v1/state`
- доступен ли token path для mutating commands
- готовы ли базовые preconditions для read-only и mutating paths

`doctor` не делает mutating requests и ничего не исправляет автоматически.
Он дополняет `auth status`, а не заменяет его:

- `auth status` отвечает на вопрос "почему mutating path доступен или нет"
- `doctor` отвечает на вопрос "готовы ли daemon/API/auth preconditions в целом"

В human-readable output `doctor` также даёт краткий actionable advice, например:

- проверить `--url`
- использовать `--token`, `CRATONCTL_TOKEN` или `--token-file`
- исправить права на token file
- запустить `cratonctl auth status` для детального auth breakdown

## Mutating команды

Все mutating-команды требуют Bearer token.

### `cratonctl trigger <task>`

Запускает scheduled task вне расписания.

HTTP:

- `POST /trigger/{task}`

Примеры:

```bash
cratonctl trigger recovery
cratonctl trigger backup
cratonctl trigger recovery --token-file /var/lib/craton/remediation-token
```

Поддерживаемые task names определяются daemon-side scheduler/API.
CLI не валидирует и не ограничивает их отдельным локальным списком.

### `cratonctl restart <service>`

HTTP:

- `POST /api/v1/remediate`

JSON body:

```json
{
  "action": "RestartService",
  "target": "ntfy"
}
```

### `cratonctl maintenance set <service> --reason "..."`

HTTP:

- `POST /api/v1/remediate`

JSON body:

```json
{
  "action": "MarkMaintenance",
  "target": "ntfy",
  "reason": "ручная диагностика"
}
```

Важно: в текущей реализации CLI нет `--for`. Duration maintenance
определяется политикой демона, а не флагом CLI.

### `cratonctl maintenance clear <service>`

Action:

- `ClearMaintenance`

### `cratonctl breaker clear <service>`

Action:

- `ClearBreaker`

### `cratonctl flapping clear <service>`

Action:

- `ClearFlapping`

### `cratonctl backup run`

Эквивалент `trigger backup`.

HTTP:

- `POST /trigger/backup`

### `cratonctl backup unlock`

HTTP:

- `POST /api/v1/remediate`

Action:

- `ResticUnlock`

### `cratonctl disk cleanup`

HTTP:

- `POST /api/v1/remediate`

Action:

- `RunDiskCleanup`

## JSON mode

Используй `--json`, если:

- команду читает скрипт
- нужен стабильный объект/массив для `jq`
- важно отделить stdout от stderr

Поведение:

- stdout содержит один JSON document
- ошибки тоже выводятся JSON-объектом в stdout, если передан `--json`
- stderr в JSON mode не используется для обычных ошибок команды

Пример ошибки:

```json
{
  "error": {
    "kind": "auth",
    "code": "token_file_unreadable",
    "message": "authentication error: token file is not readable: /var/lib/craton/remediation-token (permission denied)",
    "exit_code": 2
  }
}
```

## Exit codes

- `0`: успех
- `1`: daemon-side ошибка или отказ
- `2`: invalid arguments, config error, transport error, parse error

Практически это значит:

- `health` возвращает `1`, если daemon unhealthy
- `service <id>` вернёт `1`, если сервис не найден
- проблемы URL, токена, TCP connection или JSON parsing вернут `2`

## Примеры

### Проверить демон

```bash
cratonctl health
cratonctl auth status
cratonctl status
cratonctl doctor
```

### Получить JSON для автоматизации

```bash
cratonctl --json services
cratonctl --json history backup
cratonctl --json service ntfy
```

### Ручная диагностика

```bash
cratonctl diagnose continuwuity
cratonctl maintenance set continuwuity --reason "ручная проверка после обновления"
cratonctl restart continuwuity
cratonctl maintenance clear continuwuity
```

### Работа с backup

```bash
cratonctl backup run
cratonctl --json history backup
cratonctl backup unlock
```

## Ограничения

- нет TUI
- нет watch mode
- нет autocomplete
- нет YAML output
- нет HTTPS client support в MVP
- `doctor` — это safe preflight, а не full integration test
- CLI не вычисляет wall-clock age из raw monotonic daemon fields
- branding/banner появляются только в onboarding/help paths и не должны мешать script-friendly usage

## Что planned / future

Ниже перечислено как возможное развитие, но не как текущая функциональность:

- watch mode
- shell completions
- более богатое форматирование вывода
- дополнительные безопасные helper-команды для bulk inspection
