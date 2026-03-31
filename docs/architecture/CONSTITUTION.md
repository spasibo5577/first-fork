CRATOND — Agent Development Spec v1

Назначение: спецификация архитектуры, инвариантов и правил реализации для AI-агента, пишущего код проекта cratond.
1. Приоритет документов и режим работы агента
1.1 Назначение этого документа

Этот документ задаёт:

    архитектурные инварианты;
    стабильные контракты поведения;
    ограничения на реализацию;
    правила, которые агент не должен нарушать при генерации кода.

1.2 Что ещё будет источником истины

К этому документу может прилагаться:

    документ текущего состояния проекта;
    текущий код репозитория;
    формулировка конкретной задачи пользователя.

1.3 Приоритет при конфликте

Если источники расходятся, агент действует так:

    Инварианты безопасности и архитектуры из этого документа имеют приоритет.
    Текущее состояние проекта имеет приоритет над этим документом в вопросах:
        конкретных имён функций/модулей;
        уже существующих сигнатур;
        текущего уровня завершённости;
        временных адаптеров и совместимости.
    Задача пользователя может уточнять объём работы, но не должна молча ломать архитектурные инварианты.

1.4 Что делать при расхождении

Если текущий код противоречит этой спецификации:

    НЕ переписывать полпроекта молча;
    локализовать конфликт;
    предложить минимальное изменение, достаточное для задачи;
    если нужен более широкий рефакторинг — явно отметить это в ответе/плане.

1.5 Правило минимального diff

Агент должен:

    сначала читать существующий код модуля;
    менять только необходимое;
    не переименовывать и не перемещать код без причины;
    не вводить новую архитектурную подсистему для локальной задачи.

2. Non-negotiable invariants

Это правила уровня MUST / MUST NOT.
2.1 Единственный владелец состояния

    MUST: всё мутабельное состояние системы (State) изменяется только в control loop.
    MUST NOT: любой другой поток или компонент изменяет State напрямую.
    Следствие: нет shared mutable state между потоками.

2.2 Явные состояния

    MUST: любой процесс с более чем двумя фазами моделируется явным enum/FSM.
    MUST NOT: кодировать фазу через комбинации bool-флагов, nullable-полей или “неявные” состояния.

2.3 Разделение решений и побочных эффектов

    MUST: логика принятия решений живёт в reducer/policy.
    MUST: побочные эффекты выполняются только через typed Command в effect layer.
    MUST NOT: reducer делать I/O, запускать процессы, писать файлы, слать HTTP, менять внешнее состояние.

2.4 Пессимистичная модель внешнего мира

    MUST: любой внешний вызов имеет timeout.
    MUST: любая запись критического файла — атомарная.
    MUST: любой внешний ответ может быть некорректным.
    MUST: любой subprocess может зависнуть.
    MUST NOT: полагаться на “обычно это быстро”.

2.5 Fail-stop при нарушении инварианта

    MUST: нарушение внутреннего инварианта = panic! / abort / перезапуск через systemd.
    MUST NOT: тихо продолжать работу после невозможного состояния.

2.6 No shell, ever

    MUST: все внешние команды задаются argv.
    MUST NOT: использовать shell-интерполяцию, sh -c, конкатенацию командных строк.

2.7 Временная модель

    MUST: расписания используют wall clock (SystemTime / эквивалент).
    MUST: timeout/cooldown/breaker/rate-limit/grace используют monotonic time (Instant).
    MUST NOT: cooldown или breaker завязывать на wall clock.

2.8 Backup compensation invariant

    MUST: при восстановлении после прерванного backup возвращать систему в состояние до backup, а не в “желаемое”.
    MUST: использовать was_running из персистентной backup phase.
    MUST NOT: поднимать “все сервисы подряд” после crash recovery.

2.9 Наблюдаемость

    MUST: система должна уметь объяснить:
        что произошло;
        какое решение было принято;
        почему;
        что будет сделано дальше.

2.10 Тишина оператора

    MUST: если всё работает — не слать уведомления.
    MUST NOT: создавать шум из алертов, которые не требуют внимания.

2.11 Ограничения на реализацию

    MUST NOT: вводить async runtime.
    MUST NOT: вводить thread pool.
    MUST NOT: добавлять новые внешние crates без явного разрешения или без подтверждения в документе текущего состояния.
    MUST NOT: заменять typed commands/events на строковые протоколы.

3. Архитектурная модель
3.1 Общая схема

text

Events ─────> Control Loop / Reducer ─────> Commands ─────> Effect Executor
   ^                                                      |
   |                                                      v
   └──────────── EffectResult / ProbeResults / ACKs ──────┘

HTTP thread: только чтение snapshot; mutating-запросы -> Event в control loop
Notifier thread: durable outbox + delivery

3.2 Компоненты
MUST:

    Control Loop — единственный владелец State.
    Effect Executor — выполняет typed Command, не принимает решений.
    HTTP API — читает опубликованный Snapshot; mutating endpoints только проксируют команду в control loop.
    Notifier — отправка уведомлений и работа с durable outbox.
    Scheduler — генерирует Event::Tick { due_tasks }.
    Watchdog/systemd notify — отражает реальный прогресс control loop.

3.3 Потоки

Система использует 4 долгоживущих потока, не больше:

    Main / Control Loop
    Effect Worker
    HTTP
    Notifier

MUST:

    параллельные healthcheck'и в effect worker выполнять через std::thread::scope и короткоживущие потоки;
    не вводить thread pool.

3.4 Синхронизация
MUST:

    State: ownership только в control loop.
    Snapshot: Mutex<Arc<Snapshot>> или эквивалентная краткоживущая публикация.
    Event channel: MPSC, несколько отправителей, один получатель.
    Alert channel: отдельный канал control loop → notifier или эквивалент.

MUST NOT:

    делить State между потоками через Arc<Mutex<State>>.

4. Проектные принципы
4.1 Config-driven, not user-programmable

    MUST: сервисы и параметры задаются конфигом.
    MUST NOT: вводить исполняемую пользовательскую логику, скрипты или DSL.

4.2 Closed world

    MUST: набор probe/action/policy типов конечен и зафиксирован кодом.
    MUST NOT: строить архитектуру “плагинов” или свободного расширения конфигом.

4.3 Latent complexity

    SHOULD: сложные механизмы (breaker, leases, graph suppression и т.д.) включать там, где они реально нужны.
    MUST NOT: строить избыточную динамическую инфраструктуру.

5. Стабильные типовые контракты

Ниже — семантически стабильные контракты.
Имена могут слегка отличаться в текущем коде, если это уже существует, но смысл и структура поведения должны сохраняться.
5.1 Event

Rust

enum Event {
    Tick { due_tasks: Vec<TaskKind> },
    ProbeResults(Vec<ProbeResult>),
    EffectCompleted { cmd_id: CmdId, result: EffectResult },
    HttpCommand(CommandRequest),
    SignalReceived(Signal),
    StartupRecovery(PersistedState),
}

5.2 Command

Rust

enum Command {
    RunProbes(Vec<ProbeSpec>),

    RestartService { unit: ServiceId, reason: RestartReason },
    StopService { unit: ServiceId, reason: StopReason },
    StartService { unit: ServiceId },

    RestartDockerDaemon { reason: String },

    RunBackupPhase(BackupPhase),

    SendAlert(Alert),

    PersistState(StateSnapshot),
    PersistBackupPhase(BackupState),

    PublishSnapshot(Arc<Snapshot>),

    RunDiskCleanup(CleanupLevel),

    CheckAptUpdates,
    CheckDockerUpdates(Vec<ServiceId>),

    WriteIncident(IncidentReport),
    UpdateLlmContext(LlmContext),

    NotifyWatchdog,

    Shutdown { grace: Duration },
}

5.3 ProbeSpec

Rust

enum ProbeSpec {
    Http {
        url: String,
        timeout: Duration,
        expect_status: u16,
    },
    Dns {
        server: String,
        port: u16,
        query: String,
        timeout: Duration,
    },
    SystemdActive {
        unit: String,
    },
    Exec {
        argv: Vec<String>,
        timeout: Duration,
        expect_stdout: Option<String>,
    },
    TailscaleStatus {
        timeout: Duration,
    },
}

5.4 ProbeResult / ProbeError

Rust

enum ProbeResult {
    Healthy {
        service: ServiceId,
        latency: Duration,
    },
    Unhealthy {
        service: ServiceId,
        error: ProbeError,
    },
    Timeout {
        service: ServiceId,
        deadline: Duration,
    },
}

enum ProbeError {
    ConnectionRefused,
    HttpStatus(u16),
    DnsFailure(String),
    ExecFailed { exit_code: i32, stderr: String },
    UnexpectedOutput(String),
    Timeout,
    DependencyUnavailable(ServiceId),
}

5.5 ServiceState

Rust

enum ServiceState {
    Healthy,
    Degraded { since: Instant, consecutive_failures: u32 },
    Failed { since: Instant, last_error: ProbeError },
    Recovering { attempt: u32, started_at: Instant },
    BlockedByDependency { root: ServiceId },
    Suppressed { breaker: BreakerState },
    InMaintenance { until: SystemTime, reason: String },
    Unknown,
}

    Примечание: until для maintenance может быть wall-clock, если это операторская политика “до времени X”.
    Все внутренние cooldown/grace/breaker окна — monotonic.

5.6 BackupPhase

Rust

enum BackupPhase {
    Idle,
    Locked { run_id: u64 },
    ResticUnlocking { run_id: u64 },
    ServicesStopping { run_id: u64, was_running: Vec<ServiceId> },
    ResticRunning { run_id: u64, was_running: Vec<ServiceId> },
    ServicesStarting { run_id: u64, remaining: Vec<ServiceId> },
    RetentionRunning { run_id: u64 },
    Verifying { run_id: u64 },
}

5.7 BreakerState

Rust

enum BreakerState {
    Closed,
    Open { until: Instant, trip_count: u32 },
    HalfOpen { probe_attempt: u32 },
}

5.8 ExecSpec

Rust

struct ExecSpec {
    argv: Vec<String>,
    timeout: Duration,
    kill_grace: Duration,
}

MUST:

    нельзя создать spec для внешней команды без timeout.

5.9 ServiceSpec

Rust

struct ServiceSpec {
    id: ServiceId,
    display_name: String,
    unit: String,
    kind: ServiceKind,          // Systemd | DockerSystemd | Virtual
    probe: ProbeSpec,

    depends_on: Vec<ServiceId>,
    resources: Vec<ResourceId>,

    startup_grace: Duration,
    restart_cooldown: Duration,

    max_restarts: u32,
    breaker_window: Duration,
    breaker_cooldown: Duration,

    backup_stop: bool,
    severity: Severity,         // Critical | Warning | Info
}

5.10 Snapshot / State

Полная внутренняя структура State может меняться, но семантически должна содержать:

    текущие ServiceState по сервисам;
    состояние backup FSM;
    maintenance entries;
    leases;
    breaker state / restart history;
    scheduler timestamps;
    outbox health / overflow state;
    histories (recovery/backup/remediation);
    опубликованные explainable reason fields для API.

Snapshot — read-only представление для HTTP/API.
6. Control Loop и reducer
6.1 Основной цикл

Control loop принимает события и для каждого:

    вызывает reduce(state, event) -> Vec<Command>;
    отправляет команды в effect layer / notifier / persistence;
    публикует snapshot;
    обновляет heartbeat / watchdog.

6.2 Reducer contract

Rust

fn reduce(state: &mut State, event: Event) -> Vec<Command>

MUST:

    это единственное место мутации State;
    reducer не делает I/O;
    reducer не исполняет subprocess;
    reducer не пишет файлы;
    reducer не стучится в HTTP;
    reducer не отправляет уведомления напрямую, а только выдаёт Command::SendAlert.

6.3 Что решает reducer

Reducer содержит policy decisions:

    recovery policy;
    backup FSM transitions;
    disk policy;
    dependency/root-cause decisions;
    circuit breaker transitions;
    lease arbitration decisions;
    coalescing alerts;
    HTTP mutating command handling;
    AI remediation rate-limits and whitelist enforcement.

7. Правила поведения (policy contracts)
8. Recovery policy
8.1 Базовая последовательность

Если сервис unhealthy, reducer должен рассмотреть в таком порядке:

    maintenance mode?
    есть ли unhealthy dependency/root cause?
    breaker open?
    startup grace ещё не истекла?
    restart cooldown ещё не истек?
    lease на ресурс доступен?
    безопасно ли рестартовать именно этот сервис?

8.2 Результаты решения

В зависимости от условий reducer выбирает:

    restart;
    skip;
    alert-only;
    suppress because dependency;
    defer because lease conflict;
    escalate.

8.3 Принцип хирургической реакции
MUST:

    реакция должна быть минимально достаточной;
    лечить конкретный сломавшийся компонент;
    не рестартовать “всё подряд”.

9. Dependency graph и root cause detection
9.1 Структура

    Граф зависимостей — DAG из depends_on.
    Проверяется при старте.

9.2 Валидация
MUST:

    топологическая сортировка;
    hard fail при циклах;
    hard fail при неизвестных service id в depends_on.

9.3 Правила при healthcheck

Если родитель unhealthy:

    дочерний сервис переходит в BlockedByDependency { root };
    дочерний не рестартуется;
    отдельный alert по дочернему не отправляется.

9.4 Root cause detection

Формальное правило:

    собрать все unhealthy сервисы;
    для каждого проверить unhealthy ancestors;
    если unhealthy ancestor есть — сервис blocked/suppressed by dependency;
    remaining unhealthy services without unhealthy ancestors = root causes.

9.5 Восстановление
MUST:

    сначала лечить root cause;
    после восстановления root cause дать grace period;
    только потом проверять потомков;
    если потомки восстановились сами — ничего не делать.

9.6 Coalesced notification

Если сбой коррелированный:

    одно сводное уведомление;
    указать root cause;
    перечислить затронутые сервисы;
    описать действие.

10. Circuit breaker
10.1 Пер-сервисный breaker

Каждый сервис имеет собственный breaker.
10.2 FSM

text

Closed --[max_restarts within breaker_window]--> Open(until)
Open --[cooldown expired]--> HalfOpen
HalfOpen --[probe healthy]--> Closed
HalfOpen --[probe unhealthy]--> Open(until, trip_count + 1)

10.3 Правила
MUST:

    breaker считается на monotonic time;
    reducer не должен планировать restart, если breaker open;
    half-open должен проверяться аккуратно и явно.

10.4 Escalation

При trip breaker:

    trip 1: suppress + alert;
    trip 2: suppress + alert with higher severity;
    trip 3+: suppress + alert + incident report для AI.

11. Lease system
11.1 Модель

ResourceId — строковый идентификатор, напр.:

    service:<id>
    docker-daemon
    backup-repo
    disk-cleanup

Lease:

    resource
    holder: TaskId
    acquired_at
    TTL

11.2 Правила
MUST:

    lease запрашивается через reducer/command path;
    если ресурс занят — операция откладывается или отклоняется;
    lease освобождается по завершении операции;
    lease имеет safety TTL;
    при старте все leases сбрасываются (они in-memory).

11.3 Примеры

    backup Continuwuity → service:continuwuity + backup-repo
    restart Continuwuity → service:continuwuity
    docker daemon restart → docker-daemon
    docker prune → docker-daemon
    disk cleanup → disk-cleanup

11.4 Поведенческие последствия

    restart Unbound во время backup может быть разрешён;
    restart Continuwuity во время backup должен блокироваться;
    docker prune во время backup должен блокироваться;
    параллельный docker restart из recovery и remediation невозможен.

12. Backup system
12.1 Расписание

    schedule: нечётные дни, 04:00.
    catch-up: если демон был выключен в этот момент, после старта проверить, нужен ли backup за текущий нечётный день.

12.2 Дубликаты
MUST:

    не запускать backup дважды в один день.

OPEN:

В исходном документе есть внутреннее расхождение:

    в одном месте last-backup-day пишется после завершения;
    в другом — сразу после запуска backup для защиты от дублей.

Правило для агента:
если в текущем проекте это уже реализовано — следовать текущему состоянию;
если ещё не реализовано — не выбирать молча, а явно отметить это как место для уточнения.
12.3 FSM

Последовательность фаз:

    Idle
    Locked
    ResticUnlocking
    ServicesStopping { was_running }
    ResticRunning { was_running }
    ServicesStarting { remaining }
    RetentionRunning
    Verifying
    Idle

12.4 Критический инвариант

was_running обязательно персистится в BackupPhase.

Причина:

    после crash recovery должны подниматься только сервисы, работавшие до backup.

12.5 Pattern detection

3 consecutive backup failures:

    escalated alert;
    приложить диагностику:
        exit codes;
        stderr последних 3 запусков;
        disk usage;
        repository status.

12.6 Таймауты

    restic unlock: 120s
    restic backup: 3600s
    restic forget: 600s
    restic check: 1800s
    service stop: 60s
    service start: 120s
    cleanup after interrupted backup: 60s (отдельный контекст)

13. Persistence и crash recovery
13.1 Атомарная запись

Единственный допустимый путь записи критических файлов:

    создать временный файл в том же каталоге;
    записать данные;
    fsync(fd);
    close(fd);
    rename(tmp, target);
    открыть каталог;
    fsync(dir_fd);
    закрыть каталог.

MUST NOT:

    использовать для критических файлов прямой write;
    полагаться на “rename without fsync dir” как достаточную гарантию.

13.2 Какие файлы персистятся

    /var/lib/craton/backup-state.json — BackupPhase + метаданные
    /var/lib/craton/maintenance.json — maintenance entries
    /var/lib/craton/alert-outbox.jsonl — durable outbox
    /var/lib/craton/last-backup-day — дата последнего backup
    /run/craton/llm_context.json — volatile AI context

13.3 Когда писать
MUST:

    backup-state.json — при каждом переходе backup FSM;
    maintenance.json — при каждом изменении;
    alert-outbox.jsonl — при каждом append/delete/mark-delivered;
    llm_context.json — периодически;
    last-backup-day — см. OPEN выше.

13.4 Startup recovery

При старте:

    прочитать backup-state.json;
    если phase != Idle:
        для ServicesStopping / ResticRunning / ServicesStarting:
            поднять was_running сервисы;
            выполнить restic unlock;
            перевести phase в Idle;
        для RetentionRunning / Verifying:
            просто перевести в Idle;
    прочитать maintenance.json, удалить протухшие записи;
    прочитать outbox и повторить недоставленные;
    прочитать last-backup-day и решить, нужен ли catch-up backup.

13.5 Cleanup context
MUST:

    cleanup после прерванного backup выполняется в отдельном контексте/timeout;
    он не должен отменяться только потому, что родительский контекст погашен.

14. Effect Executor
14.1 Общий контракт

Effect executor — “руки” системы:

    получает Command;
    выполняет его;
    возвращает типизированный результат / событие.

MUST NOT:

    принимать policy-решения;
    менять State напрямую.

14.2 Self-exec helper mode

Для опасных внешних команд используется self-exec helper:

text

cratond --exec-helper <timeout_ms> -- <argv...>

Модель:

    главный процесс запускает helper;
    helper создаёт свою process group;
    helper запускает реальную команду;
    helper пишет stdout/stderr/exit_code в pipe;
    главный процесс читает pipe с таймаутом;
    если helper завис — убить helper, учесть orphan/timeout и продолжить.

14.3 Обязательные свойства
MUST:

    любой внешний вызов иметь timeout;
    разделять SIGTERM/SIGKILL через kill_grace;
    корректно собирать stdout/stderr/exit code;
    не блокировать control loop.

15. Конфигурация
15.1 Формат

    MUST: TOML.
    MUST NOT: YAML.

15.2 Структура

Конфиг включает секции:

    [daemon]
    [ntfy]
    [backup]
    [disk]
    [updates]
    [ai]
    [[service]] ... [service.probe]

15.3 Service config contract

Поддерживаются поля ServiceSpec из раздела 5.9.
15.4 Виртуальные узлы

Сервисы типа вроде docker_daemon могут описываться как виртуальные сервисы, чтобы:

    участвовать в dependency graph;
    быть root cause;
    подавлять дочерние рестарты/алерты.

15.5 Валидация конфига
MUST:

    hard fail при неизвестных полях;
    hard fail при отсутствии обязательных полей;
    hard fail при duplicate service id;
    hard fail при циклах в dependency graph;
    проверять допустимость значений.

15.6 Версионирование конфига

    Пока отдельный config version не обязателен.
    При несовместимом изменении формат меняется явно и документируется миграция.

16. Notification system
16.1 Durable outbox

Путь уведомления:

    reducer создаёт Alert;
    alert сериализуется и append'ится в outbox на диск;
    alert попадает в notifier;
    notifier пытается доставить;
    при успехе запись помечается delivered / удаляется по стратегии outbox;
    при неуспехе alert остаётся на диске;
    после рестарта undelivered replay'ятся.

16.2 Retry policy

Базовая политика:

    0s
    5s
    15s

16.3 Overflow policy

Ёмкость outbox: 256 записей максимум.

При переполнении:

    удалить самые старые delivered;
    если этого недостаточно — удалить самые старые undelivered;
    обязательно залогировать факт потери.

16.4 Fallback

Если NTFY недоступен:

    stderr/syslog/journald;
    durable outbox на диске.

16.5 Deduplication

    ключ: SHA-256 от (title, body);
    TTL dedup: 30 минут;
    хранение in-memory;
    после рестарта повторная отправка допустима.

OPEN:

точная реализация SHA-256 зависит от политики зависимостей и текущего состояния проекта.
Агент не должен самовольно добавлять crate без разрешения.
16.6 Coalescing

Если в одном recovery cycle рождается >2 alerts:

    объединять их в сводное сообщение.

16.7 Язык

    MUST: все операторские уведомления на русском языке.

17. Disk monitoring
17.1 Метод

    MUST: использовать statfs.
    MUST NOT: парсить вывод df.

17.2 Пороги

    < 85% — OK
    >= 85% — warning + cleanup level 1
    >= 95% — critical + cleanup level 2

17.3 Cleanup levels

    level 1: apt clean, journal vacuum
    level 2: docker image prune + level 1

17.4 Predictive mode

    опционально;
    по умолчанию выключен;
    только информативные уведомления, без автоматических действий.

Если включён:

    OLS на последних 14 samples;
    если R² >= 0.7, можно слать прогноз заполнения диска.

17.5 Safety checks для docker prune
MUST:

    lease docker-daemon свободен;
    lease backup-repo свободен;
    backup не активен.

18. HTTP API
18.1 Bind

    127.0.0.1:18800
    без TLS
    без общей auth
    remediation имеет отдельную token auth.

18.2 Реализация
OPEN:

вариант HTTP-сервера не зафиксирован окончательно:

    std::net ручная реализация
    минимальный crate вроде tiny-http
    иной уже существующий вариант в проекте

Правило для агента:
не принимать это решение самостоятельно, если оно ещё не принято в текущем проекте.
18.3 Endpoints

Read-only:

    GET /health
    GET /api/v1/state
    GET /api/v1/diagnose/{service}
    GET /api/v1/history/recovery
    GET /api/v1/history/backup
    GET /api/v1/history/remediation

Mutating:

    POST /trigger/{task}
    POST /api/v1/remediate

18.4 Правило mutating endpoint'ов
MUST:

    HTTP thread не меняет State напрямую;
    mutating endpoint отправляет Event::HttpCommand(...) в control loop;
    затем ждёт ACK/ответ.

18.5 Health endpoint

GET /health возвращает 200 OK только если:

    heartbeat control loop моложе 30 секунд;
    последний health cycle завершился менее 10 минут назад;
    нет invariant violations;
    outbox не переполнен.

Иначе:

    503 Service Unavailable
    с кратким объяснением причины.

19. AI integration
19.1 Режимы

    disabled
    audit
    enabled

19.2 Whitelist remediation actions

Допустимы только:

    RESTART_SERVICE
    MARK_MAINTENANCE
    DOCKER_RESTART
    TRIGGER_BACKUP
    CLEAR_BREAKER

Всё остальное:

    MUST: rejected.

19.3 Rate limiting

Per (action, target):

    restart service: 3/hour per service
    docker restart: 1/hour
    trigger backup: 1/day
    mark maintenance: 5/hour

19.4 Где реализуется rate limit

    MUST: в reducer / policy layer
    MUST NOT: в HTTP handler как единственном месте проверки

19.5 Token auth

    случайный токен, генерируется при первом запуске;
    пишется в файл;
    это guard, а не полноценная криптографическая auth-система.

20. systemd integration
20.1 sd_notify

Реализуется вручную через Unix socket из NOTIFY_SOCKET.

Поддерживаемые поля:

    READY=1
    WATCHDOG=1
    STATUS=...

20.2 Watchdog

WATCHDOG=1 отправляется только если:

    control loop heartbeat свежий;
    нет fatal error flag;
    есть реальный прогресс.

Если control loop завис:

    watchdog перестаёт отправляться;
    systemd рестартует процесс.

20.3 Unit

Ожидаемая семантика unit:

    Type=notify
    Restart=on-failure
    watchdog включён
    есть время на graceful shutdown и cleanup

21. Временная модель
21.1 Wall clock

Используется для:

    backup schedule;
    apt/docker update schedule;
    maintenance until wall time;
    catch-up логики.

21.2 Monotonic time

Используется для:

    timeout;
    cooldown;
    breaker windows;
    rate limits;
    startup grace;
    lease TTL.

21.3 Инварианты
MUST:

    NTP-скачок не должен ломать timeout/cooldown;
    расписания могут пропуститься или быть догнаны catch-up механизмом — это допустимо.

22. Error handling
22.1 Классы ошибок

    Transient — retry/log/continue
    Persistent — breaker/alert/stop trying
    Fatal internal — log + exit(1) / restart by systemd
    Invariant violation — panic/abort

22.2 Правила
MUST:

    не использовать unwrap() в обычном коде;
    каждый ? обогащать контекстом;
    для инвариантов использовать expect("...") с ясным пояснением.

MUST NOT:

    глотать ошибки записи state/outbox/config;
    silently ignore parse failures критических файлов.

23. Политика зависимостей
23.1 Разрешённые crates

Базово допустимы:

    serde
    serde_json
    toml
    libc

23.2 Ограничения
MUST NOT:

    добавлять новые crates без необходимости и согласования;
    вводить framework ради 1-2 функций.

23.3 Что предполагается реализовать самостоятельно

    atomic file write
    sd_notify
    DNS probe
    signal handling wrappers
    ring buffer
    OLS regression
    часть HTTP logic / localhost server
    часть exec helper plumbing

23.4 OPEN

Нужно следовать текущему состоянию проекта или спрашивать:

    точная реализация HTTP-сервера;
    реализация SHA-256;
    возможно, конкретная wall-clock scheduling library, если уже используется.

24. Тестирование
24.1 Приоритет тестов
P0 — reducer/policy

Обязательные сценарии:

    сервис unhealthy, зависимость жива → restart;
    сервис unhealthy, зависимость мертва → blocked, не restart;
    сервис упал 3 раза за окно → breaker open;
    breaker open, cooldown истёк → half-open;
    backup phase transitions по всем фазам;
    backup interrupted на phase X → корректная компенсация;
    несколько docker-сервисов down + docker_daemon down → root cause docker;
    disk 87% → cleanup level 1;
    lease занят → операция отложена;
    reducer не рестартует blocked-by-dependency сервис;
    reducer не рестартует сервис при open breaker.

P1 — persistence

    atomic write + read back;
    recovery simulation;
    outbox append/replay/mark delivered.

P2 — probes/executor

    fake exec, проверка argv;
    timeout handling;
    helper termination behavior.

24.2 Integration tests

    backup FSM full cycle with mock exec;
    cascading failure recovery;
    startup crash recovery from persisted partial state.

24.3 Property tests

Nice to have:

    reducer never emits restart with open breaker;
    reducer never emits restart for blocked dependency;
    reducer never emits command requiring lease without lease decision.

25. Сборка и деплой
25.1 Target

    aarch64-unknown-linux-gnu
    glibc acceptable
    musl возможен, но не обязателен

25.2 Release profile

toml

[profile.release]
opt-level = "s"
lto = true
codegen-units = 1
strip = true
panic = "abort"

25.3 Цели

    binary size: < 3 MB
    RSS: < 5 MB
    startup time: < 1 s
    first healthcheck: < 10 s

Это целевые ориентиры; не ломать архитектуру ради микрооптимизаций.
26. Структура проекта

Ожидаемая структура ответственности:

text

cratond/
├── Cargo.toml
├── src/
│   ├── main.rs
│   ├── config.rs
│   ├── model.rs
│   ├── reduce.rs
│   ├── policy/
│   │   ├── mod.rs
│   │   ├── recovery.rs
│   │   ├── backup.rs
│   │   ├── disk.rs
│   │   └── dependency.rs
│   ├── effect/
│   │   ├── mod.rs
│   │   ├── executor.rs
│   │   ├── exec_helper.rs
│   │   ├── probe.rs
│   │   └── systemd.rs
│   ├── persist/
│   │   ├── mod.rs
│   │   ├── atomic.rs
│   │   ├── state.rs
│   │   └── outbox.rs
│   ├── notify.rs
│   ├── http.rs
│   ├── scheduler.rs
│   ├── lease.rs
│   ├── breaker.rs
│   ├── graph.rs
│   ├── history.rs
│   ├── alert.rs
│   └── incident.rs
├── tests/
│   ├── reducer_recovery.rs
│   ├── reducer_backup.rs
│   ├── reducer_disk.rs
│   ├── reducer_dependency.rs
│   ├── persistence.rs
│   └── integration.rs

Правило для агента

    если в текущем коде структура уже слегка отличается — не перестраивать её без необходимости;
    сохранять разделение ответственности.

27. Операционные файлы

Ожидаемые пути:

text

/usr/local/bin/cratond
/etc/systemd/system/cratond.service
/etc/craton/config.toml
/var/lib/craton/backup-state.json
/var/lib/craton/maintenance.json
/var/lib/craton/alert-outbox.jsonl
/var/lib/craton/last-backup-day
/var/lib/craton/remediation-token
/run/craton/llm_context.json
/root/.picoclaw/workspace/incidents/

28. Инструкции агенту по написанию кода
28.1 Перед изменением кода

Агент должен:

    прочитать существующий модуль;
    определить, к какому слою относится задача:
        типы/модель;
        reducer/policy;
        effect execution;
        persistence;
        HTTP;
        tests;
    проверить, не ломает ли решение инварианты.

28.2 Куда класть новую логику

    новые доменные типы → model.rs
    новая decision logic → reduce.rs / policy/*
    новый внешний эффект → effect/*
    новая персистентность → persist/*
    операторские тексты → alert.rs
    incident generation → incident.rs

28.3 Что агенту запрещено делать без явного запроса

    вводить async runtime;
    вводить shell;
    менять thread model;
    добавлять новые crates;
    объединять reducer и executor;
    хранить неявные состояния в bool-комбинациях;
    делать широкие рефакторинги “заодно”.

28.4 Как писать изменения

    минимальный diff;
    exhaustive match;
    timeout на каждый внешний вызов;
    атомарная запись для критических файлов;
    русские operator-facing сообщения;
    тесты на decision logic.

29. Coding checklist

Перед завершением задачи агент должен проверить:

    State мутируется только в control loop?
    reducer не делает I/O?
    все новые внешние вызовы имеют timeout?
    shell нигде не появился?
    критические файлы пишутся атомарно?
    backup/crash recovery не ломают was_running?
    monotonic и wall-clock не перепутаны?
    новые alert messages на русском?
    добавлены/обновлены тесты?
    не добавлены лишние зависимости?
    не создан shared mutable state между потоками?
    если есть конфликт со старым кодом — он явно отмечен?

30. Open decisions / точки, которые нельзя додумывать молча

Агент не должен самостоятельно принимать окончательное решение, если это не зафиксировано текущим состоянием проекта:

    HTTP server implementation
        std ручная реализация
        tiny-http
        иной минимальный вариант

    SHA-256 implementation
        свой код
        отдельный crate
        уже существующая реализация проекта

    Момент записи last-backup-day
        immediately on backup start
        after successful completion

    Некоторые точные форматы persisted state, если они уже существуют в проекте и отличаются по именам полей, но не по смыслу.

31. Краткий итог для агента

Если задача не требует архитектурного изменения, агент должен придерживаться простой схемы:

    решения → reduce.rs / policy/*
    эффекты → effect/*
    состояние и контракты → model.rs
    диск → persist/*
    никакого shell
    никакого async
    никакого shared mutable state
    никаких скрытых FSM
    все timeout’ы явные
    все критические записи атомарные
    все operator-facing сообщения по-русски
