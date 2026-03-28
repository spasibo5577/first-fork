
## 3. Архитектура потоков

```
┌─────────────────────────────────────────────────────────┐
│                    main thread                          │
│                  (control loop)                         │
│                                                         │
│  event_rx ← ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ┐  │
│                                                     │  │
│  loop {                                             │  │
│    check_schedules() → Tick events                  │  │
│    event_rx.recv_timeout(1s) → Event                │  │
│    reduce(state, event) → Vec<Command>              │  │
│    execute_commands(cmds)                           │  │
│  }                                                  │  │
└─────────────────────────────────────────────────────│──┘
                                                      │
         event_tx (mpsc::Sender<Event>)               │
                    ┌─────────────────────────────────┘
                    │
    ┌───────────────┼───────────────┐
    │               │               │
    ▼               ▼               ▼
┌────────┐    ┌──────────┐    ┌──────────────┐
│ signal │    │ http-api │    │ notifier     │
│ thread │    │ thread   │    │ thread       │
│        │    │          │    │              │
│ signal │    │ GET/POST │    │ NTFY deliver │
│ →Event │    │ →Event   │    │ (no events)  │
└────────┘    └──────────┘    └──────────────┘
```

### Потоки и их роли

| Поток             | Создаётся в   | Роль                                                |
|-------------------|---------------|-----------------------------------------------------|
| `main`            | `main.rs`     | Control loop, владеет `State`                       |
| `signal-adapter`  | `main.rs`     | Конвертирует `SignalKind` → `Event::Signal`          |
| `signal`          | `signal.rs`   | Обрабатывает POSIX-сигналы (SIGTERM, SIGHUP)        |
| `http-api`        | `http.rs`     | HTTP-сервер на `tiny_http`, read-only снимки + POST  |
| `notifier`        | `notify.rs`   | Доставка алертов в NTFY с retry и dedup             |
| `picoclaw-trigger`| `aibridge.rs` | Fire-and-forget HTTP POST в AI-мост (не блокирует)  |

---

## 4. Модульная структура

### 4.1 Карта модулей

```
src/
├── main.rs          — Точка входа, фазы инициализации (1-5)
├── model.rs         — Все типы данных: Event, Command, ProbeSpec, ...
├── state.rs         — Mutable runtime state: State, SvcState, ServiceStatus
├── runtime.rs       — Control loop + dispatch команд (execute_commands)
├── reduce.rs        — Чистый reducer: (State, Event) → Vec<Command>
├── config.rs        — TOML-конфиг с валидацией
├── graph.rs         — DAG зависимостей, топосортировка, classify_failures
├── breaker.rs       — Circuit breaker (чистые функции переходов)
├── schedule.rs      — Расписания (Daily, Weekly, OddDays, Interval)
├── history.rs       — Универсальный RingBuf<T>
├── notify.rs        — NTFY-нотификатор (dedup SHA-256, retry, bounded queue)
├── persist.rs       — Atomic write (tmp→rename→fsync)
├── lease.rs         — In-memory lease arbiter (mutex exclusion)
├── http.rs          — HTTP API (tiny_http, SharedSnapshot)
├── signal.rs        — POSIX signal handler
│
├── effect/          — Побочные эффекты (только вызывается из runtime)
│   ├── mod.rs
│   ├── exec.rs      — Безопасный exec с timeout, pgid, SIGTERM→SIGKILL
│   ├── probe.rs     — Health probes: HTTP, DNS, systemd, exec
│   ├── systemd.rs   — sd_notify (READY, WATCHDOG, STOPPING, STATUS)
│   ├── disk.rs      — Мониторинг диска (df, du, cleanup)
│   ├── updates.rs   — APT и Docker обновления
│   ├── aibridge.rs  — Fire-and-forget POST в PicoClaw
│   └── incident.rs  — Запись инцидентов
│
└── policy/          — Чистая логика решений (без I/O)
    ├── mod.rs
    ├── recovery.rs  — RecoveryPlan: Decision per service
    ├── backup.rs    — Backup FSM transitions, crash_compensation
    └── disk.rs      — Оценка состояния диска
```

### 4.2 Зависимости между слоями

```
config, model, graph          ← нет зависимостей от других модулей проекта
         ↓
state, breaker, schedule,     ← зависят от model
history, persist, lease
         ↓
policy/                       ← зависит от state, model, graph, breaker, schedule
         ↓
reduce                        ← зависит от policy, state, model, graph, breaker
         ↓
effect/                       ← зависит от model, config (не от state/reduce!)
         ↓
runtime                       ← зависит от всего; вызывает reduce и effect
         ↓
notify, http, signal          ← отдельные потоки, общаются через каналы
         ↓
main                          ← wire всё вместе
```

**Запрещены обратные зависимости**: `policy` не импортирует `effect`,
`reduce` не импортирует `runtime`.

---

## 5. Типы данных ядра

### 5.1 Event

Все входящие события в control loop:

```
Event::Tick { due_tasks: Vec<TaskKind> }
Event::ProbeResults(Vec<ProbeResult>)
Event::EffectCompleted { cmd_id: u64, result: EffectResult }
Event::HttpCommand(CommandRequest)
Event::Signal(SignalKind)           — Shutdown | Reload
Event::StartupRecovery { persisted_backup: BackupPhase }
```

### 5.2 Command

Все команды, возвращаемые reducer'ом:

| Категория   | Команды                                                              |
|-------------|----------------------------------------------------------------------|
| Probe       | `RunProbes`                                                          |
| Service     | `RestartService`, `StopService`, `StartService`, `RestartDockerDaemon` |
| Backup      | `RunBackupPhase`, `ResticUnlock`, `ResticBackup`, `ResticForget`, `ResticCheck` |
| Persist     | `PersistBackupState`, `PersistMaintenance`                           |
| Notify      | `SendAlert`                                                          |
| Snapshot    | `PublishSnapshot`                                                    |
| Disk        | `CheckDiskUsage`, `RunDiskCleanup`                                   |
| Updates     | `CheckAptUpdates`, `CheckDockerUpdates`                              |
| Incident    | `WriteIncident`                                                      |
| AI          | `UpdateLlmContext`, `TriggerPicoClaw`                                |
| System      | `NotifyWatchdog`, `Shutdown`                                         |
| Lease       | `AcquireLease`, `ReleaseLease`                                       |

### 5.3 ServiceStatus

```
Unknown          — начальное состояние
Healthy          — probe успешен, since_mono
Unhealthy        — probe провален, consecutive count
Recovering       — идёт рестарт, attempt number
Failed           — исчерпаны попытки, error string
BlockedByDep     — зависимость нездорова, root service id
Suppressed       — breaker открыт, until_mono
```

### 5.4 BreakerState

```
Closed    → Open (при restarts_in_window >= threshold)
Open      → HalfOpen (при истечении cooldown)
HalfOpen  → Closed (при успешном probe)
HalfOpen  → Open (при новом сбое, trip_count++)
```

Явный сброс через оператор: `RemediationAction::ClearBreaker`.

---

## 6. Backup FSM

Состояния и переходы (линейная цепочка):

```
Idle
  ↓ should_start() == true
Locked { run_id }
  ↓
ResticUnlocking { run_id }
  ↓
ServicesStopping { run_id, pre_backup_state }
  ↓
ResticRunning { run_id, pre_backup_state }
  ↓ EffectCompleted(success)
ServicesStarting { run_id, remaining }
  ↓
ServicesVerifying { run_id, started }
  ↓
RetentionRunning { run_id }
  ↓
Verifying { run_id }   ← только если backup.verify == true
  ↓
Idle
```

**Crash recovery**: при старте читается `/var/lib/craton/backup-state.json`.
Если фаза не Idle — запускается `backup::crash_compensation()`:
- `needs_restic_unlock()` → `ResticUnlock`
- `needs_service_recovery()` → `StartService` для остановленных сервисов
- всегда → `ResetToIdle`

**Старый путь миграции**: при отсутствии нового файла проверяется
`/var/lib/granit/backup-state.json` (Go-монолит).

---

## 7. Recovery Policy

Алгоритм `policy::recovery::evaluate()`:

1. Собрать множество нездоровых сервисов из probe results
2. `graph.classify_failures()` → root causes + blocked dependents
3. Если ≥2 Docker-сервиса упали без зависимостей → `docker_restart_needed`
4. Для каждого сервиса вызвать `evaluate_single()`:

```
probe.is_healthy()          → Decision::Healthy
svc_state.in_maintenance()  → Decision::InMaintenance
blocked.contains(sid)       → Decision::BlockedByDependency { root }
docker_root_cause           → Decision::DockerRootCause
!breaker.allows_recovery()  → Decision::BreakerOpen
cooldown не истёк           → Decision::BreakerOpen
attempt > severity.max()    → Decision::Failed
иначе                       → Decision::Restart { unit, attempt, severity }
```

**Максимум попыток по severity**:
- `Info`: 1 попытка
- `Warning`: 3 попытки
- `Critical`: 5 попыток

**Lease-блокировка**: если на ресурс (`ResourceId::Service`) взят lease
(например, backup остановил сервис), restart не выполняется.

---

## 8. Расписание задач

| Задача          | Тип расписания  | Время         |
|-----------------|-----------------|---------------|
| Recovery        | Interval        | каждые 5 мин  |
| Disk monitor    | Interval        | каждые 6 ч    |
| Backup          | OddDays         | 04:00         |
| APT updates     | Daily           | 09:00         |
| Docker updates  | Weekly (сб)     | 10:00         |
| Daily summary   | Daily           | 09:05         |

Startup delay: первые 120 секунд задачи не запускаются (grace period).

`schedule::is_due()` использует 5-минутное окно для срабатывания дневных задач.
Повторный запуск в тот же день блокируется через `last_*_day: Option<u32>`.

---

## 9. Probe-типы

| Тип             | Механизм                                    | Параметры                              |
|-----------------|---------------------------------------------|----------------------------------------|
| `Http`          | Raw TCP, GET, парсинг HTTP-статуса          | url, timeout_secs, expect_status       |
| `Dns`           | UDP A-query, проверка QR-бит и RCODE        | server, port, query, timeout_secs      |
| `SystemdActive` | `systemctl is-active --quiet <unit>`        | unit (пустая строка = unit сервиса)    |
| `Exec`          | Прямой exec, exit code + stdout-check       | argv, timeout_secs, expect_stdout      |

`StdoutCheck` для Exec-пробы:
- `Contains { pattern }`
- `NotContains { pattern }`
- `JsonField { pointer, expected }` — JSON Pointer (RFC 6901)

Все пробы запускаются параллельно через `std::thread::scope`.

---

## 10. HTTP API

Адрес по умолчанию: `127.0.0.1:18800`

| Метод | Путь                        | Описание                                         |
|-------|-----------------------------|--------------------------------------------------|
| GET   | `/health`                   | `{"status":"ok"}` или `{"status":"degraded"}`   |
| GET   | `/api/v1/state`             | JSON-снимок всего состояния демона               |
| GET   | `/api/v1/history/{name}`    | История (из снимка, по имени ресурса)            |
| GET   | `/api/v1/diagnose/{svc}`    | `systemctl status` + `journalctl -n 50`          |
| POST  | `/trigger/{task}`           | Запустить задачу вне расписания                  |

Допустимые значения `{task}` для POST:
`recovery`, `backup`, `disk-monitor`, `apt-updates`, `docker-updates`, `daily-summary`

Мутирующие запросы отправляют `Event::HttpCommand` в control loop и
возвращают `202 Accepted`. HTTP-поток никогда не мутирует `State` напрямую.

---

## 11. Нотификации (NTFY)

- Реализация: raw TCP, самодельный HTTP POST (без http-клиента в зависимостях)
- Deduplication: SHA-256 от `title + "\0" + body`, TTL 30 минут
- Retry: настраивается через `ntfy.retries` (по умолчанию `[0, 5, 15, 60]` секунд)
- Queue: bounded sync_channel (64 сообщения по умолчанию)
- Переполнение очереди: сообщение дропается и логируется в stderr
- Приоритеты: `min | low | default | high | urgent` (NTFY protocol)

---

## 12. Конфигурация (TOML)

Путь по умолчанию: `/etc/craton/config.toml` (первый аргумент CLI).

Структура:

```toml
[daemon]          # listen, watchdog, log_level
[ntfy]            # url, topic, retries
[backup]          # restic_repo, restic_password_file, paths, retention, verify
[disk]            # warn_percent, critical_percent, predictive
[updates]         # apt_schedule, docker_schedule
[ai]              # mode, picoclaw_url, context_path, token_path

[[service]]       # id, name, unit, kind, probe, depends_on, severity, ...
[service.probe]   # type = "http|dns|systemd_active|exec"
```

Валидация при старте:
- Нет сервисов → ошибка
- Дублирующиеся `id` → ошибка
- Неизвестный `depends_on` → ошибка
- `warn_percent >= critical_percent` → ошибка
- Пустые `restic_repo`, `restic_password_file`, `ntfy.url` → ошибка
- Цикл в зависимостях → ошибка (Kahn's algorithm)

---

## 13. Файловая система

| Путь                                      | Назначение                            |
|-------------------------------------------|---------------------------------------|
| `/etc/craton/config.toml`                 | Конфигурация (read-only при старте)   |
| `/var/lib/craton/backup-state.json`       | Persisted backup FSM (atomic write)   |
| `/var/lib/craton/remediation-token`       | Токен AI-ремедиации                   |
| `/run/craton/llm_context.json`            | LLM-контекст (volatile, tmpfs)        |
| `/var/lib/granit/backup-state.json`       | Legacy (миграция с Go-монолита)       |

---

## 14. Зависимости (crates)

| Crate          | Версия | Назначение                               |
|----------------|--------|------------------------------------------|
| `serde`        | 1      | Сериализация/десериализация              |
| `serde_json`   | 1      | JSON (снимки, persisted state, API)      |
| `toml`         | 0.8    | Парсинг конфигурации                     |
| `libc`         | 0.2    | syscalls: clock_gettime, setpgid, fsync  |
| `signal-hook`  | 0.3    | POSIX signal handling                    |
| `tiny_http`    | 0.12   | Встроенный HTTP-сервер                   |
| `sha2`         | 0.10   | SHA-256 для dedup алертов                |

Новые crates добавляются только с явным обоснованием в commit message.

---

## 15. Release-профиль

```toml
[profile.release]
opt-level = "s"       # минимальный размер бинаря
lto = true            # link-time optimization
codegen-units = 1     # лучшая оптимизация
strip = true          # убрать символы отладки
panic = "abort"       # нет раскрутки стека (меньше бинарь)
```

---

## 16. Статус реализации (фазы)

| Подсистема                     | Статус         | Заметки                                      |
|--------------------------------|----------------|----------------------------------------------|
| Config parsing & validation    | ✅ Готово       |                                              |
| Dependency graph               | ✅ Готово       | Cycle detection, classify_failures           |
| Circuit breaker                | ✅ Готово       | Closed→Open→HalfOpen→Closed                  |
| Schedule                       | ✅ Готово       | Daily, Weekly, OddDays, Interval             |
| Reducer skeleton               | ✅ Готово       | Все ветки обработаны                         |
| Health probes                  | ✅ Готово       | HTTP, DNS, systemd, exec + stdout check      |
| Recovery policy                | ✅ Готово       | Docker correlation, lease check              |
| HTTP API                       | ✅ Готово       | /health, /state, /diagnose, /trigger         |
| NTFY notifier                  | ✅ Готово       | Dedup, retry, bounded queue                  |
| Atomic persist                 | ✅ Готово       | tmp→rename→fsync                             |
| Crash recovery                 | ✅ Готово       | Backup phase compensation                    |
| sd_notify (systemd watchdog)   | ✅ Готово       | READY, WATCHDOG, STOPPING                    |
| Signal handling                | ✅ Готово       | SIGTERM→Shutdown, SIGHUP→Reload              |
| Lease arbiter                  | ✅ Готово       | Acquire/Release/TTL eviction                 |
| Backup FSM execution           | ⚠️ Skeleton    | Phase 4: команды не исполняются (no-op)      |
| Disk cleanup execution         | ⚠️ Skeleton    | Phase 4: CheckDiskUsage, RunDiskCleanup      |
| APT/Docker update checks       | ⚠️ Skeleton    | Phase 4: команды не исполняются              |
| Incident writing               | ⚠️ Skeleton    | Phase 4: только eprintln                     |
| PicoClaw AI bridge             | ⚠️ Skeleton    | Phase 4: aibridge.rs готов, не проводится    |
| Maintenance persistence        | ⚠️ Skeleton    | Phase 4: PersistMaintenance — no-op          |

> Команды "Phase 4" обрабатываются в `execute_commands` как `_ => {}` (no-op).
> Логика и типы данных для них готовы; остаётся wire в runtime.
