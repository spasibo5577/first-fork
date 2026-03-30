---
created: 2026-03-29
updated: 2026-03-29
---
# Cratond — автономный демон управления инфраструктурой

Один бинарник. Без зависимостей. Без контейнеров.
Поставил, написал конфиг — работает.

Cratond непрерывно следит за сервисами на сервере, сам чинит
то что сломалось, делает бэкапы, следит за диском и присылает
уведомления в NTFY. Если всё хорошо — молчит.

---

## Быстрый старт

```bash
# 1. Скомпилировать (кросс-компиляция для ARM64)
cross build --release --target aarch64-unknown-linux-gnu

# 2. Скопировать на сервер
scp target/aarch64-unknown-linux-gnu/release/cratond server:/usr/local/bin/

# 3. Создать директории
ssh server 'mkdir -p /etc/craton /var/lib/craton /run/craton'

# 4. Скопировать конфиг
scp config.example.toml server:/etc/craton/config.toml

# 5. Отредактировать конфиг под свои сервисы
ssh server 'nano /etc/craton/config.toml'

# 6. Скопировать systemd unit
scp deploy/cratond.service server:/etc/systemd/system/

# 7. Запустить
ssh server 'systemctl daemon-reload && systemctl enable --now cratond'

# 8. Проверить
ssh server 'systemctl status cratond'
ssh server 'curl -s localhost:18800/health | python3 -m json.tool'
````

Первая проверка сервисов произойдёт через 2 минуты после старта  
(grace period для стабилизации).

---

## Мониторинг сервисов

Каждые 5 минут cratond проверяет все сервисы из конфига.  
Проверки запускаются параллельно, каждая с таймаутом.

### Типы проверок (probes)

#### HTTP

Отправляет GET-запрос, проверяет код ответа.

toml

```
[service.probe]
type          = "http"
url           = "http://127.0.0.1:8080/v1/health"
timeout_secs  = 5
expect_status = 200
```

#### DNS

Отправляет UDP A-запрос на указанный резолвер.

toml

```
[service.probe]
type    = "dns"
server  = "127.0.0.1"
port    = 5335
query   = "google.com"
timeout_secs = 5
```

#### systemd

Проверяет `systemctl is-active <unit>`.

toml

```
[service.probe]
type = "systemd_active"
# unit берётся из поля service.unit
```

#### Exec

Запускает произвольную команду, проверяет exit code.  
Опционально проверяет stdout.

toml

```
[service.probe]
type         = "exec"
argv         = ["tailscale", "status", "--json"]
timeout_secs = 10

# Проверка содержимого stdout (опционально):

# Вариант 1: строка содержит подстроку
[service.probe.expect_stdout]
kind    = "contains"
pattern = "Running"

# Вариант 2: строка НЕ содержит подстроку
[service.probe.expect_stdout]
kind    = "not_contains"
pattern = "Error"

# Вариант 3: JSON-поле по JSON Pointer (RFC 6901)
[service.probe.expect_stdout]
kind     = "json_field"
pointer  = "/BackendState"
expected = "Running"
```

---

## Автоматическое восстановление

Когда probe показывает что сервис упал, cratond проходит  
чек-лист перед рестартом:

1. **Maintenance** — сервис в обслуживании? → пропустить
2. **Зависимости** — упала ли зависимость? → чинить её, не этот сервис
3. **Circuit breaker** — не было ли слишком много рестартов? → приостановить
4. **Lease** — не заблокирован ли ресурс (например, backup остановил сервис)? → ждать
5. **Cooldown** — прошло ли достаточно времени с последнего рестарта? → ждать

Если все проверки пройдены — `systemctl restart <unit>`.

### Количество попыток по важности

|Severity|Макс. попыток|Описание|
|---|---|---|
|`critical`|5|Ключевые сервисы (DNS, proxy)|
|`warning`|3|Важные, но не критичные|
|`info`|1|Вспомогательные|

toml

```
[[service]]
id       = "unbound"
severity = "critical"   # 5 попыток
```

---

## Граф зависимостей

Сервисы могут зависеть друг от друга через `depends_on`.  
Проверяется при старте: циклы и неизвестные ID — fatal error.

toml

```
[[service]]
id         = "adguard"
depends_on = ["unbound"]   # AdGuard зависит от Unbound
```

### Что это даёт

- Если Unbound упал → AdGuard помечается как `BlockedByDependency`
- AdGuard НЕ рестартуется (бесполезно, пока DNS не работает)
- Чинится только Unbound
- После восстановления Unbound — grace period, затем проверка AdGuard
- Если AdGuard встал сам — никаких действий

### Одно уведомление вместо десяти

Вместо отдельного алерта на каждый зависимый сервис — одно  
сводное: «Упал Unbound, затронуты: AdGuard, Caddy».

### Docker correlation

Если несколько Docker-сервисов упали одновременно без общей  
зависимости — cratond рестартует Docker daemon, а не каждый  
контейнер отдельно.

Для этого нужен виртуальный узел:

toml

```
[[service]]
id   = "docker_daemon"
kind = "virtual"
severity = "critical"
[service.probe]
type = "systemd_active"

[[service]]
id         = "continuwuity"
kind       = "docker_systemd"
depends_on = ["docker_daemon"]
```

---

## Circuit breaker

Если сервис рестартуется и снова падает — это flapping.  
Cratond остановит рестарты после достижения лимита.

### Как работает

text

```
Closed (нормальная работа)
  → рестарты >= max_restarts за breaker_window_secs
Open (рестарты заблокированы)
  → ждём breaker_cooldown_secs
HalfOpen (пробуем одну проверку)
  → probe healthy → Closed
  → probe unhealthy → Open (trip_count++)
```

### Конфигурация

toml

```
[[service]]
max_restarts         = 3      # сколько рестартов до срабатывания
breaker_window_secs  = 3600   # за какой период считать
breaker_cooldown_secs = 3600  # сколько ждать в Open
```

### Уведомления

- Trip 1: «Breaker сработал для {service}»
- Trip 2+: повторный алерт с повышенным приоритетом

Один алерт вместо спама каждые 5 минут.

### Ручной сброс

Bash

```
curl -X POST http://localhost:18800/api/v1/remediate \
  -H "Authorization: Bearer $(cat /var/lib/craton/remediation-token)" \
  -H "Content-Type: application/json" \
  -d '{"action":"CLEAR_BREAKER","target":"ntfy"}'
```

---

## Резервное копирование

Полный автоматический цикл через restic.

### Последовательность

1. Проверка расписания
2. Захват lease на `backup-repo`
3. `restic unlock` (на случай зависшего лока)
4. Остановка сервисов с `backup_stop = true`
5. Захват lease на каждый остановленный сервис
6. `restic backup`
7. Запуск сервисов обратно, освобождение lease
8. `restic forget --prune` (ротация)
9. `restic check` (если `verify = true`)
10. Освобождение lease на repo
11. Уведомление о результате

### Расписание

toml

```
[backup]
schedule = "odd_days:04:00"   # нечётные дни в 04:00
```

Если демон был выключен в момент расписания — при следующем  
старте проверит и запустит пропущенный бэкап (catch-up).

### Crash recovery

Если демон или сервер упал посреди бэкапа:

1. При старте читается `/var/lib/craton/backup-state.json`
2. Если фаза не `Idle`:
    - Запускаются обратно сервисы которые были остановлены
    - Выполняется `restic unlock`
    - Фаза сбрасывается в `Idle`
3. Уведомление: «Crash recovery выполнен»

Ключевой инвариант: поднимаются **только те сервисы, которые  
были запущены ДО бэкапа** (сохраняется в `pre_backup_state`).

### Escalation при повторных сбоях

3 неудачных бэкапа подряд → расширенный алерт:

- Exit codes и stderr последних попыток
- Состояние диска
- Incident report (markdown файл)

### Ротация

toml

```
[backup.retention]
daily   = 7
weekly  = 4
monthly = 6
```

### Какие сервисы останавливать

toml

```
[[service]]
id          = "continuwuity"
backup_stop = true    # остановить перед backup
```

---

## Мониторинг диска

Проверка через `statfs` каждые 6 часов.

|Использование|Реакция|
|---|---|
|< 85%|Ничего|
|≥ 85%|⚠️ Предупреждение + `apt clean` + `journal vacuum`|
|≥ 95%|🔴 Критический алерт + `docker image prune -a`|

Пороги настраиваются:

toml

```
[disk]
warn_percent     = 85
critical_percent = 95
```

Перед `docker image prune` проверяется:

- Бэкап не запущен
- Docker daemon не заблокирован lease

---

## Уведомления

Все уведомления идут через NTFY. Все тексты на русском.

### Настройка

toml

```
[ntfy]
url    = "http://127.0.0.1:8080"
topic  = "craton-alerts"
retries = [0, 5, 15, 60]   # задержки между попытками
```

### Механизм доставки

- Raw TCP POST (без HTTP-клиента в зависимостях)
- Retry с настраиваемыми интервалами
- Дедупликация: SHA-256 от `title + body`, TTL 30 минут
- Durable outbox: алерты сохраняются на диск в  
    `/var/lib/craton/alert-outbox.jsonl`
- После рестарта недоставленные отправляются повторно
- Overflow: максимум 256 записей, старые удаляются
- Fallback: если NTFY недоступен — stderr (journald)

### Тишина

Если всё работает — cratond молчит. Никаких «всё ок»  
каждые 5 минут.

### Ежедневная сводка

Каждый день в 09:05 — сводка:

- Статус всех сервисов (✅/❌)
- Состояние диска
- Результат последнего бэкапа

---

## HTTP API

Слушает на `127.0.0.1:18800` (настраивается). Без TLS,  
только localhost.

### Read-only

|Endpoint|Описание|
|---|---|
|`GET /health`|Статус демона. `200` если control loop свежий, `503` если завис|
|`GET /api/v1/state`|Полный JSON-снимок: сервисы, backup, диск|
|`GET /api/v1/history/recovery`|История recovery-циклов|
|`GET /api/v1/history/backup`|История бэкапов|
|`GET /api/v1/history/remediation`|История remediation-команд|
|`GET /api/v1/diagnose/{service}`|Диагностика конкретного сервиса|

### Mutating

|Endpoint|Описание|
|---|---|
|`POST /trigger/{task}`|Запустить задачу вне расписания|
|`POST /api/v1/remediate`|Выполнить действие (требует токен)|

### Примеры

Bash

```
# Статус демона
curl -s localhost:18800/health

# Полное состояние
curl -s localhost:18800/api/v1/state | python3 -m json.tool

# Диагностика сервиса
curl -s localhost:18800/api/v1/diagnose/unbound

# Запустить бэкап вне расписания
curl -X POST localhost:18800/trigger/backup

# Запустить проверку здоровья
curl -X POST localhost:18800/trigger/recovery

# Проверить диск
curl -X POST localhost:18800/trigger/disk-monitor

# Отправить ежедневную сводку
curl -X POST localhost:18800/trigger/daily-summary
```

Допустимые значения для `/trigger/{task}`:  
`recovery`, `backup`, `disk-monitor`, `apt-updates`,  
`docker-updates`, `daily-summary`

---

## Remediation API

`POST /api/v1/remediate` — выполнение действий через API.  
Требует авторизации.

### Авторизация

Токен генерируется автоматически при первом запуске и  
сохраняется в файл (по умолчанию  
`/var/lib/craton/remediation-token`).

Bash

```
TOKEN=$(cat /var/lib/craton/remediation-token)
curl -X POST http://localhost:18800/api/v1/remediate \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"action":"RESTART_SERVICE","target":"ntfy","reason":"ручной рестарт"}'
```

### Доступные действия

|Action|Target|Описание|
|---|---|---|
|`RESTART_SERVICE`|service id|Рестартовать сервис|
|`DOCKER_RESTART`|—|Рестартовать Docker daemon|
|`MARK_MAINTENANCE`|service id|Maintenance на 1 час (без алертов и рестартов)|
|`CLEAR_MAINTENANCE`|service id|Снять maintenance|
|`CLEAR_BREAKER`|service id|Сбросить circuit breaker|
|`CLEAR_FLAPPING`|service id|Сбросить breaker + историю рестартов|
|`TRIGGER_BACKUP`|—|Запустить бэкап|
|`RESTIC_UNLOCK`|—|Разблокировать restic repo|
|`RUN_DISK_CLEANUP`|—|Стандартная очистка диска|

### Rate limiting

Ограничения применяются в reducer (не в HTTP), поэтому  
действуют на все источники команд одинаково:

|Действие|Лимит|
|---|---|
|`RESTART_SERVICE`|3 в час на каждый сервис|
|`DOCKER_RESTART`|1 в час|
|`TRIGGER_BACKUP`|1 в сутки|
|`MARK_MAINTENANCE`|5 в час|

При превышении лимита — ответ `202 Accepted`, но действие  
записывается в лог как `rejected: rate limited`.

### Формат запроса

JSON

```
{
  "action": "RESTART_SERVICE",
  "target": "ntfy",
  "reason": "ручной рестарт после обновления"
}
```

`target` обязателен для: `RESTART_SERVICE`, `MARK_MAINTENANCE`,  
`CLEAR_MAINTENANCE`, `CLEAR_BREAKER`, `CLEAR_FLAPPING`.

`reason` — опционально, записывается в audit trail.

---

## Конфигурация

Формат: TOML. Путь по умолчанию: `/etc/craton/config.toml`.  
Можно указать другой как первый аргумент командной строки:

Bash

```
cratond /path/to/config.toml
```

Неизвестные поля вызывают ошибку при парсинге.

### Полный пример

Полный пример конфигурации со всеми параметрами и комментариями  
находится в файле `config.example.toml` в корне репозитория.

### Минимальный конфиг

Минимально необходимый конфиг:

toml

```
[ntfy]
url   = "http://127.0.0.1:8080"
topic = "craton-alerts"

[backup]
restic_repo          = "/opt/restic-repo"
restic_password_file = "/root/.config/restic/passphrase"

[[service]]
id   = "ntfy"
name = "NTFY"
unit = "ntfy.service"
kind = "systemd"
severity = "critical"

[service.probe]
type          = "http"
url           = "http://127.0.0.1:8080/v1/health"
```

Все остальные параметры имеют значения по умолчанию.

### Секции конфига

#### `[daemon]`

|Параметр|По умолчанию|Описание|
|---|---|---|
|`listen`|`127.0.0.1:18800`|Адрес HTTP API|
|`watchdog`|`true`|Отправлять WATCHDOG=1 в systemd|
|`log_level`|`info`|Уровень логирования|

#### `[ntfy]`

|Параметр|Обязательно|Описание|
|---|---|---|
|`url`|✅|URL NTFY-сервера|
|`topic`|✅|Топик для алертов|
|`retries`|Нет|Задержки между попытками `[0, 5, 15, 60]`|

#### `[backup]`

|Параметр|Обязательно|Описание|
|---|---|---|
|`restic_repo`|✅|Путь к репозиторию|
|`restic_password_file`|✅|Путь к файлу с паролем|
|`schedule`|Нет|`"odd_days:04:00"`|
|`restic_binary`|Нет|`"/usr/bin/restic"`|
|`paths`|Нет|Директории для бэкапа|
|`verify`|Нет|`true` — проверять после forget|
|`verify_subset_percent`|Нет|`5` — процент данных для проверки|

#### `[backup.retention]`

|Параметр|По умолчанию|Описание|
|---|---|---|
|`daily`|`7`|Ежедневных снимков|
|`weekly`|`4`|Еженедельных|
|`monthly`|`6`|Ежемесячных|

#### `[disk]`

|Параметр|По умолчанию|Описание|
|---|---|---|
|`warn_percent`|`85`|Порог предупреждения|
|`critical_percent`|`95`|Критический порог|
|`predictive`|`false`|Предиктивный анализ|

#### `[ai]`

|Параметр|По умолчанию|Описание|
|---|---|---|
|`mode`|`"disabled"`|`disabled` / `audit` / `enabled`|
|`picoclaw_url`|`""`|URL AI-агента|
|`context_path`|`/run/craton/llm_context.json`|Куда писать контекст|
|`token_path`|`/var/lib/craton/remediation-token`|Файл с токеном|

#### `[[service]]`

|Параметр|Обязательно|По умолчанию|Описание|
|---|---|---|---|
|`id`|✅|—|Уникальный идентификатор|
|`name`|✅|—|Отображаемое имя|
|`unit`|✅|—|systemd unit|
|`kind`|✅|—|`systemd` / `docker_systemd` / `virtual`|
|`severity`|Нет|`warning`|`info` / `warning` / `critical`|
|`depends_on`|Нет|`[]`|Зависимости (массив id)|
|`backup_stop`|Нет|`false`|Останавливать для бэкапа|
|`startup_grace_secs`|Нет|`15`|Grace period после старта|
|`restart_cooldown_secs`|Нет|`60`|Мин. интервал между рестартами|
|`max_restarts`|Нет|`3`|Рестартов до breaker|
|`breaker_window_secs`|Нет|`3600`|Окно подсчёта рестартов|
|`breaker_cooldown_secs`|Нет|`3600`|Пауза breaker в Open|

---

## Установка

### Сборка (кросс-компиляция для ARM64)

Bash

```
# На машине разработчика (Windows/Linux x86_64)
rustup target add aarch64-unknown-linux-gnu
cargo install cross

cross build --release --target aarch64-unknown-linux-gnu

# Бинарник: target/aarch64-unknown-linux-gnu/release/cratond
```

### Деплой на сервер

Bash

```
# Копируем бинарник
scp target/aarch64-unknown-linux-gnu/release/cratond \
  server:/usr/local/bin/

# Создаём директории
ssh server 'mkdir -p /etc/craton /var/lib/craton'

# Копируем конфиг
scp config.example.toml server:/etc/craton/config.toml

# Копируем systemd unit
scp deploy/cratond.service server:/etc/systemd/system/

# Права на конфиг (содержит пути к секретам)
ssh server 'chmod 600 /etc/craton/config.toml'

# Запуск
ssh server 'systemctl daemon-reload && systemctl enable --now cratond'
```

---

## systemd

### Unit файл

ini

```
[Unit]
Description=Craton Infrastructure Daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=notify
ExecStart=/usr/local/bin/cratond /etc/craton/config.toml
Restart=on-failure
RestartSec=5
WatchdogSec=30

# Graceful shutdown
TimeoutStopSec=90

# Безопасность
NoNewPrivileges=yes
ProtectSystem=strict
ReadWritePaths=/var/lib/craton /run/craton
ProtectHome=yes
PrivateTmp=yes

[Install]
WantedBy=multi-user.target
```

### Watchdog

Cratond отправляет `WATCHDOG=1` в systemd при каждом цикле  
control loop. Если control loop зависнет — watchdog перестанет  
тикать и systemd перезапустит демон через 30 секунд.

### Управление

Bash

```
# Статус
systemctl status cratond

# Логи
journalctl -u cratond -f

# Перезапуск
systemctl restart cratond

# Остановка
systemctl stop cratond
```

---

## Файлы на диске

|Путь|Описание|
|---|---|
|`/usr/local/bin/cratond`|Бинарник|
|`/etc/craton/config.toml`|Конфигурация|
|`/var/lib/craton/backup-state.json`|Фаза бэкапа (для crash recovery)|
|`/var/lib/craton/maintenance.json`|Maintenance state|
|`/var/lib/craton/alert-outbox.jsonl`|Durable outbox (недоставленные алерты)|
|`/var/lib/craton/remediation-token`|Bearer-токен для API|
|`/run/craton/llm_context.json`|JSON-контекст для AI (volatile, tmpfs)|

### Права

Bash

```
chmod 600 /etc/craton/config.toml
chmod 700 /var/lib/craton
chmod 600 /var/lib/craton/remediation-token
```

---

## AI-интеграция

Cratond может работать с внешним AI-агентом (PicoClaw, ZeroClaw  
или другим).

### Режимы

|Режим|Описание|
|---|---|
|`disabled`|AI отключён (по умолчанию)|
|`audit`|AI получает контекст, предложения логируются, не выполняются|
|`enabled`|AI может выполнять действия через remediation API|

### Контекст для AI

Cratond публикует JSON-файл с текущим состоянием системы:

toml

```
[ai]
context_path = "/run/craton/llm_context.json"
```

Содержимое обновляется после каждого recovery-цикла. AI может  
читать этот файл и принимать решения на основе текущего  
состояния.

### Команды от AI

AI отправляет команды через `POST /api/v1/remediate` с  
Bearer-токеном. Whitelist действий и rate limiting применяются  
одинаково для всех источников.

### Триггеры к AI

При инцидентах cratond отправляет fire-and-forget HTTP POST  
на `picoclaw_url` с типом события и деталями. AI может  
реагировать на эти триггеры.

---

## Troubleshooting

### Демон не запускается

Bash

```
# Проверить конфиг
/usr/local/bin/cratond /etc/craton/config.toml
# Ошибки валидации будут в stderr

# Частые причины:
# - Неизвестные поля в конфиге
# - Дублирующиеся service id
# - Циклы в depends_on
# - warn_percent >= critical_percent
```

### Нет уведомлений

Bash

```
# Проверить что NTFY доступен
curl -s http://127.0.0.1:8080/v1/health

# Проверить логи cratond
journalctl -u cratond | grep -i ntfy

# Проверить outbox
cat /var/lib/craton/alert-outbox.jsonl
```

### Сервис flapping (постоянно рестартуется)

Bash

```
# Поставить в maintenance
curl -X POST http://localhost:18800/api/v1/remediate \
  -H "Authorization: Bearer $(cat /var/lib/craton/remediation-token)" \
  -H "Content-Type: application/json" \
  -d '{"action":"MARK_MAINTENANCE","target":"SERVICE_ID","reason":"диагностика"}'

# Посмотреть историю
curl -s localhost:18800/api/v1/history/recovery | python3 -m json.tool

# Сбросить breaker после починки
curl -X POST http://localhost:18800/api/v1/remediate \
  -H "Authorization: Bearer $(cat /var/lib/craton/remediation-token)" \
  -H "Content-Type: application/json" \
  -d '{"action":"CLEAR_FLAPPING","target":"SERVICE_ID"}'
```

### Бэкап не запускается

Bash

```
# Проверить фазу бэкапа
curl -s localhost:18800/api/v1/state | python3 -c "
import json,sys
d=json.load(sys.stdin)
print('phase:', d.get('backup_phase'))
print('last_day:', d.get('last_backup_day'))
"

# Запустить вручную
curl -X POST localhost:18800/trigger/backup

# Проверить restic
restic -r /opt/restic-repo snapshots
```

### Демон завис

systemd watchdog перезапустит автоматически. Если нет:

Bash

```
# Проверить health
curl -s localhost:18800/health
# 503 = control loop завис

# Принудительный рестарт
systemctl restart cratond
```

---

## Характеристики

|Параметр|Значение|
|---|---|
|Размер бинарника|~2-3 MB|
|Потребление RAM|~3-5 MB|
|Время запуска|< 1 секунды|
|Первая проверка|через 2 минуты|
|Потоки|4 (loop, http, notifier, signal)|
|Внешние зависимости|нет (кроме restic для бэкапов)|
|Тесты|126|
