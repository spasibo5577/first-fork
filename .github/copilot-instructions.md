# Copilot Instructions — Cratond

Эти инструкции задают правила работы агента в репозитории `cratond`.

## Приоритет источников истины

Если источники расходятся, использовать такой порядок:

1. `CONSTITUTION.md` — архитектурные инварианты и обязательные правила
2. текущий код репозитория
3. `HANDOFF.md` — текущее состояние и известные проблемы
4. `ARCHITECTURE.md` / `README.md` — описательная документация
5. эти инструкции

Не доверяй документации слепо. Всегда сверяй с кодом.

---

## Базовый режим работы

Перед началом любой существенной правки:

```bash
cargo check
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test --quiet --message-format=short
```

Если нужно проверить один тест:

```Bash
cargo test <test_name> -- --nocapture
```

Если среда не позволяет выполнить проверки — скажи это явно и не утверждай,
что проект зелёный.
Сборка

Обычная локальная сборка:

```Bash
cargo build
cargo build --release
```

Для ARM64 Linux (рабочий путь проекта):

```Bash
cargo zigbuild --release --target aarch64-unknown-linux-gnu
```

Если zig не найден — сначала установить/добавить zig в PATH.

Не предполагай, что cross обязательно работает в текущей среде.
Архитектурные инварианты (кратко)
1. Single-writer state

    State мутируется только в control loop
    никаких Arc<Mutex<State>>
    worker threads общаются через события и снимки

2. Reducer

    reduce(state, event, ...) -> Vec<Command>
    reducer мутирует State
    reducer не делает I/O
    reducer не запускает процессы
    reducer не пишет файлы
    reducer не делает сеть

То есть reducer не pure-function в функциональном смысле;
он pure-decision layer без side effects.
3. Effect layer

    все внешние эффекты выполняются только из runtime/effect path
    effect layer не принимает policy-решений
    effect layer не зависит от reduce и не мутирует State напрямую

4. Время

    monotonic / Instant / mono_secs — timeout, breaker, cooldown, lease, rate limit
    wall/epoch — schedule, timestamps, audit, incidents

Никогда не путать mono_secs и epoch_secs.
5. Persistence

    критические файлы — только через persist::atomic_write
    никаких прямых std::fs::write для state/token/outbox/maintenance

6. No shell

    не использовать sh -c, bash -c, shell interpolation
    внешние команды только argv-style

7. Operator-facing тексты

    все уведомления оператору — на русском
    не логировать токены, секреты, пароли

Реальная архитектура проекта
Потоки

Проект использует несколько долгоживущих потоков:

    main / control loop
    HTTP thread
    notifier thread
    signal thread / adapter
    краткоживущие worker threads для отдельных effect operations

Не добавляй async runtime и не вводи thread pool.
Основной цикл

Control loop:

    принимает Event
    вызывает reduce(&mut state, event, ...)
    получает Vec<Command>
    исполняет команды через runtime/effect path
    публикует snapshot / watchdog / persistence

Snapshot

HTTP читает опубликованный snapshot.
Mutating HTTP endpoints не меняют State напрямую — только отправляют Event::HttpCommand(...).
Что важно помнить про текущий код
Reducer

    reduce.rs — это единственное место мутации State
    baseline bootstrap не должен считаться recovery
    Unknown -> Healthy не должен порождать false recovery alert

Runtime

    runtime.rs — orchestration layer
    здесь замыкается loop Command -> Effect -> Event::EffectCompleted
    watchdog должен тикать периодически и независимо от due_tasks/startup grace

HTTP

    GET /api/v1/state — полный snapshot
    GET /api/v1/history/recovery — только recovery history
    GET /api/v1/history/backup — только backup history
    GET /api/v1/history/remediation — только remediation history
    POST /api/v1/remediate — только через auth token / bearer
    mutating endpoints могут быть fire-and-forget, если текущая реализация такова

Notifications

    notifier использует dedup + retry + durable outbox
    не ломай outbox semantics
    если меняешь notify path — проверь replay/overflow behavior

Backup

    backup FSM crash-safe
    pre_backup_state критичен для восстановления
    нельзя “поднимать все сервисы подряд” после crash recovery

Как вносить изменения
Делай

    минимальный безопасный diff
    сначала читай существующий модуль целиком или хотя бы нужную функцию + соседние тесты
    при исправлении бага добавляй regression test
    держи проект зелёным после каждого логического блока

Не делай

    не переписывай файлы целиком без необходимости
    не делай несвязанный cleanup "заодно"
    не меняй публичные интерфейсы без причины
    не добавляй новые crates без явной необходимости
    не “улучшай архитектуру” если это не требуется задачей

Полезные проверки
Найти unwrap в production code

```bash
rg "unwrap\(" src
```

Найти прямые записи файлов

```Bash
rg "std::fs::write|fs::write" src
```

Найти no-op command branches

```Bash
rg "no-op|noop|handled elsewhere" src/runtime.rs src/*.rs
```


Найти возможную путаницу времени

```Bash
rg "epoch_secs|mono_secs|timestamp_epoch_secs" src
```


Формат ответа агента

Отвечай кратко и по делу:

    Что проверил
    Что нашёл
    Какие минимальные изменения сделал
    Итог cargo test
    Итог cargo clippy --all-targets
    Риски / что ещё проверить руками

Не копируй большие файлы целиком без необходимости.
