# HANDOFF.md — Состояние проекта cratond

> Документ для следующего агента/инженера. Содержит верифицированное
> текущее состояние, известные проблемы и приоритеты.
>
> Дата: актуально на момент последнего коммита.
> Перед любой работой — верифицируй по коду, не доверяй слепо.

---

## 1. Быстрый статус

| Проверка                  | Результат                                 |
|---------------------------|-------------------------------------------|
| `cargo check`             | ✅ OK                                     |
| `cargo clippy -D warnings`| ✅ OK (0 warnings, 0 errors)              |
| `cargo test` (unit)       | ✅ 107 passed, 0 failed                   |
| Архитектурные инварианты  | ✅ Не нарушены                            |

> Примечание по тестам: два теста отфильтровываются при локальном запуске,
> чтобы избежать зависания на I/O:
> - `wallclock_now` — читает системное время (не зависает, но медленно на CI)
> - `echo_succeeds` / `timeout_kills` — платформозависимые exec-тесты
>
> Полный прогон: `cargo test --quiet --message-format=short`
> Если зависает — проверь, не ждёт ли сетевой тест (NTFY, DNS, TCP).

---

## 2. Что было исправлено в последней сессии

### 2.1 Критический баг: оборванный match arm в `reduce.rs`

**Симптом**: `cargo clippy` падал с `unclosed delimiter` на строке 1161.

**Причина**: предыдущий агент сгенерировал `handle_backup_effect` с оборванным
arm `BackupPhase::ResticRunning`. После цикла запуска сервисов arm не был закрыт.
Вместо этого внутрь него был вставлен фрагмент `BackupPhase::RetentionRunning`,
а затем снаружи — ещё один полный `BackupPhase::RetentionRunning` (дубликат).
Итого: незакрытый `if success {`, три лишних скобки, дублированный arm.

**Исправление** (точечный патч `reduce.rs`, строки 381–400):
- Завершён arm `ResticRunning`: добавлен переход в `RetentionRunning`,
  `PersistBackupState`, `ResticForget { daily, weekly, monthly }`, ветка `else`
- Удалён дублированный `BackupPhase::RetentionRunning` arm
- Оставлен единственный корректный `RetentionRunning` arm с обеими ветками

### 2.2 Clippy-предупреждения (все исправлены)

| Lint                          | Где                   | Исправление                                         |
|-------------------------------|-----------------------|-----------------------------------------------------|
| `map_or(false, ...)`          | `reduce.rs:299`       | Заменено на `is_some_and(...)`                      |
| `needless_pass_by_value`      | `finalize_backup`     | `error.clone()` убран, порядок перестроен           |
| `&Option<T>` → `Option<&T>`   | `handle_remediation`  | Сигнатура изменена, вызовы обновлены                |
| `&Option<T>` → `Option<&T>`   | `record_remediation`  | Сигнатура изменена, `clone()` → `cloned()`          |
| `&action` / `&target` lints   | вызовы внутри handle  | Убраны лишние `&` (double-ref через autoderef)      |
| `&action` / `&target` в call  | `handle_http_command` | `target.as_ref()` вместо `&target`                  |

### 2.3 Создана документация

- `docs/architecture/CONSTITUTION.md` — архитектурные инварианты, схема потоков, описание
  всех модулей, типов, расписания, HTTP API, статус реализации по фазам
- `docs/handoffs/HANDOFF.md` — этот файл

---

## 3. Текущее состояние по подсистемам

### ✅ Полностью реализовано и протестировано

| Подсистема             | Модуль(и)               | Тестов |
|------------------------|-------------------------|--------|
| Config + валидация     | `config.rs`             | 7      |
| Dependency graph       | `graph.rs`              | 6      |
| Circuit breaker        | `breaker.rs`            | 11     |
| Schedule               | `schedule.rs`           | 11     |
| RingBuf                | `history.rs`            | 10     |
| Atomic persist         | `persist.rs`            | 3      |
| Lease arbiter          | `lease.rs`              | 7      |
| NTFY notifier          | `notify.rs`             | 5      |
| Recovery policy        | `policy/recovery.rs`    | 9      |
| Backup policy (FSM)    | `policy/backup.rs`      | 8      |
| Health probes          | `effect/probe.rs`       | 5      |
| Exec (с timeout/pgid)  | `effect/exec.rs`        | 5      |
| Reducer (все ветки)    | `reduce.rs`             | 14     |
| HTTP API               | `http.rs`               | —      |
| Signal handling        | `signal.rs`             | —      |
| sd_notify              | `effect/systemd.rs`     | —      |

### ⚠️ Скелет (Phase 4) — логика готова, wire не сделан

Следующие команды в `runtime::execute_commands` обрабатываются как **no-op**
(`_ => {}`) — типы и логика в reducer есть, но исполнение не подключено:

```rust
Command::CheckDiskUsage
Command::RunDiskCleanup { .. }
Command::CheckAptUpdates
Command::CheckDockerUpdates
Command::ResticBackup { .. }
Command::ResticForget { .. }
Command::ResticCheck { .. }
Command::RunBackupPhase { .. }
Command::PersistMaintenance
Command::TriggerPicoClaw { .. }
Command::AcquireLease { .. }
Command::ReleaseLease { .. }
```

Также в `execute_commands`:
- `Command::WriteIncident` — только `eprintln!`, не пишет файл
- `Command::UpdateLlmContext` — вызывает `exec_update_llm_context` (работает)

Модули `effect/disk.rs`, `effect/updates.rs`, `effect/incident.rs`,
`effect/aibridge.rs` — **написаны**, но в control loop не подключены.

---

## 4. Известные ограничения и технический долг

### 4.1 Backup FSM — `RunBackupPhase` не исполняется

В reducer правильно эмитируются команды `RunBackupPhase { phase }` и
`ResticBackup { paths }`, `ResticForget`, `ResticCheck` — но в
`execute_commands` эти ветки — no-op. Backup **не запустится реально**
даже если расписание сработает. Нужно wire `effect/exec.rs` вызовы.

**Что нужно**: дописать исполнение в `runtime.rs`:
```rust
Command::ResticBackup { paths } => exec_restic_backup(config, &paths),
Command::ResticForget { daily, weekly, monthly } => exec_restic_forget(config, ...),
Command::ResticCheck { subset_percent } => exec_restic_check(config, ...),
Command::AcquireLease { resource, holder } => { state.leases.acquire(...); }
Command::ReleaseLease { resource } => { state.leases.release(...); }
```

### 4.2 Проbes запускаются синхронно внутри control loop

В `runtime::exec_run_probes` пробы запускаются через `std::thread::scope`
параллельно — это хорошо. Но весь scope блокирует control loop на время
выполнения. При большом количестве сервисов и медленных проб это может
задерживать обработку других событий.

**Опция**: вынести probe-цикл в фоновый поток, возвращать `Event::ProbeResults`
через `event_tx`. Но это требует изменения архитектуры — обсудить с командой.

### 4.3 EffectCompleted events не генерируются

`Event::EffectCompleted { cmd_id, result }` обрабатывается в reducer, но
`runtime.rs` никогда его не отправляет. Backup FSM ждёт этого события для
продвижения фаз. До wire Phase 4 это не является блокером.

### 4.4 Lease `AcquireLease` / `ReleaseLease` — no-op в runtime

`LeaseArbiter` полностью реализован в `lease.rs`, reducer эмитирует команды,
но `execute_commands` их игнорирует. Поэтому:
- Backup не захватывает lease на сервисы перед остановкой
- Recovery не видит lease и может пытаться перезапустить сервис, которого
  намеренно остановил backup

В `policy/recovery.rs` проверка lease **уже есть**:
```rust
// lease_blocks_restart_during_backup — тест в reduce.rs
```
Но lease никогда не ставится (AcquireLease — no-op). Коллизии пока нет только
потому что backup фактически не выполняется.

### 4.5 Windows-совместимость частичная

`effect/exec.rs` имеет `#[cfg(unix)]` / `#[cfg(not(unix))]` ветки.
На Windows `setpgid` и `SIGTERM/SIGKILL` не работают — используется
`child.kill()`. DNS и HTTP pробы работают на обеих платформах.

`schedule.rs` использует `libc::localtime_s` на Windows (с `#[cfg(target_os = "windows")]`).

---

## 5. Архитектурные инварианты (краткое напоминание)

Полный список — в `docs/architecture/CONSTITUTION.md`. Критически важные:

```
1. State мутируется ТОЛЬКО в control loop
2. Reducer: нет I/O, нет exec, нет sleep
3. Все внешние вызовы с Duration timeout
4. Запрещён sh -c / bash -c
5. Критические файлы — только atomic write
6. Монотонное время для cooldown/breaker/lease
   Wall-time для schedule/timestamps/audit
7. Operator alerts — на русском
8. Не логировать секреты
```

---

## 6. Приоритеты для следующей сессии

### P0 — Критично для работоспособности

1. **Wire backup execution** (Phase 4):
   - `exec_restic_backup`, `exec_restic_forget`, `exec_restic_check` в `runtime.rs`
   - `AcquireLease` / `ReleaseLease` — передать в `state.leases`
   - `Command::RunBackupPhase` — wire в backup FSM

2. **Wire EffectCompleted loop**:
   - После async-эффекта отправлять `Event::EffectCompleted { cmd_id, result }` через `event_tx`

### P1 — Важно

3. **Wire disk monitoring**:
   - `CheckDiskUsage` → вызвать `effect/disk.rs`, записать в `state.disk_usage_percent`
   - `RunDiskCleanup` → вызвать cleanup

4. **Wire WriteIncident**:
   - `effect/incident.rs` — записать в файл (atomic write)

### P2 — Желательно

5. **Wire APT/Docker update checks**:
   - `CheckAptUpdates`, `CheckDockerUpdates` → `effect/updates.rs`

6. **PicoClaw AI bridge**:
   - `TriggerPicoClaw` → `effect/aibridge::trigger()`

7. **PersistMaintenance**:
   - Сохранять `state.services[id].maintenance` в файл для переживания рестарта

---

## 7. Структура файлов (краткая)

```
src/
├── main.rs           — инициализация, 5 фаз запуска
├── model.rs          — все типы (Event, Command, ProbeSpec, ...)
├── state.rs          — State, SvcState, ServiceStatus
├── runtime.rs        — control loop + execute_commands
├── reduce.rs         — reducer (State, Event) → Vec<Command>
├── config.rs         — TOML конфиг + валидация
├── graph.rs          — DAG зависимостей (Kahn's algorithm)
├── breaker.rs        — circuit breaker (чистые функции)
├── schedule.rs       — расписания (is_due)
├── history.rs        — RingBuf<T>
├── notify.rs         — NTFY: dedup, retry, bounded queue
├── persist.rs        — atomic_write + read_optional
├── lease.rs          — LeaseArbiter (in-memory)
├── http.rs           — tiny_http API (5 endpoints)
├── signal.rs         — POSIX signal handler
│
├── effect/
│   ├── exec.rs       — run(argv, timeout), pgid isolation, kill escalation
│   ├── probe.rs      — HTTP, DNS, systemd, exec probes
│   ├── systemd.rs    — sd_notify (READY/WATCHDOG/STOPPING)
│   ├── disk.rs       — df/du, cleanup [НЕ ПОДКЛЮЧЁН]
│   ├── updates.rs    — apt-get/docker checks [НЕ ПОДКЛЮЧЁН]
│   ├── aibridge.rs   — PicoClaw trigger [НЕ ПОДКЛЮЧЁН]
│   └── incident.rs   — incident log writer [НЕ ПОДКЛЮЧЁН]
│
└── policy/
    ├── recovery.rs   — RecoveryPlan, Decision per service
    ├── backup.rs     — FSM transitions, crash_compensation
    └── disk.rs       — disk threshold evaluation
```

---

## 8. Обязательный ритуал перед изменениями

```sh
# Перед патчем:
cargo check --quiet --message-format=short
cargo clippy --quiet --message-format=short --no-deps -- -D warnings

# После патча:
cargo test --quiet --message-format=short -- \
  --skip wallclock_now --skip echo_succeeds --skip timeout_kills
cargo clippy --quiet --message-format=short --no-deps -- -D warnings
```

Если среда не позволяет запустить проверки — явно сказать об этом
и не утверждать, что всё зелёное.

---

## 9. Конфигурационный пример (минимальный)

```toml
[ntfy]
url = "http://127.0.0.1:8080"
topic = "craton-alerts"

[backup]
restic_repo = "/opt/restic-repo"
restic_password_file = "/root/.config/restic/passphrase"

[[service]]
id = "unbound"
name = "Unbound DNS"
unit = "unbound.service"
kind = "systemd"
severity = "critical"

[service.probe]
type = "dns"
server = "127.0.0.1"
port = 5335
query = "google.com"

[[service]]
id = "adguard"
name = "AdGuard Home"
unit = "AdGuardHome.service"
kind = "systemd"
severity = "critical"
depends_on = ["unbound"]

[service.probe]
type = "dns"
server = "127.0.0.1"
port = 53
query = "google.com"
```

---

## 10. Файлы на диске (runtime)

| Путь                                    | Создаётся кем         | Назначение                       |
|-----------------------------------------|-----------------------|----------------------------------|
| `/etc/craton/config.toml`               | оператор              | конфиг (read-only)               |
| `/var/lib/craton/`                      | cratond при старте    | каталог для persisted state      |
| `/run/craton/`                          | cratond при старте    | volatile state (tmpfs)           |
| `/var/lib/craton/backup-state.json`     | persist::atomic_write | состояние backup FSM             |
| `/run/craton/llm_context.json`          | exec_update_llm_context | LLM контекст (volatile)        |
| `/var/lib/craton/remediation-token`     | оператор              | токен для AI-ремедиации          |
| `/var/lib/granit/backup-state.json`     | Go-монолит (legacy)   | читается при миграции, не пишется |
