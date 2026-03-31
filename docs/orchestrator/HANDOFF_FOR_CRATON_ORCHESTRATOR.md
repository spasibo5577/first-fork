# HANDOFF_FOR_CRATON_ORCHESTRATOR.md

## Назначение

Этот документ нужен **агенту-оркестратору проекта Craton**, у которого **нет прямого доступа к кодовой базе**.

Его задача — не писать весь код самому, а:

- держать полную картину проекта
- обсуждать с оператором архитектурные решения
- синтезировать выводы dev-агентов
- предлагать следующий маленький, reviewable шаг
- писать хорошие промпты для агентов `cratond` и `cratonctl`
- не давать проекту расползаться архитектурно

Поэтому этот handoff должен быть **самодостаточным снимком проекта**, а не просто набором общих принципов.

---

## 1. Главное различие между handoff и prompt

### Handoff

`handoff` — это **содержательное состояние проекта**:

- что это за проект
- на каком он этапе
- что уже реализовано
- что недавно было сделано
- какие решения уже приняты
- где мы остановились
- какие реальные ограничения есть
- какие темы сейчас открыты

Хороший handoff отвечает на вопрос:

**"Если я не вижу репозиторий, что я обязан знать о проекте, чтобы не принимать плохие решения?"**

### Prompt

`prompt` — это **инструкция по поведению агента**:

- как он должен мыслить
- как работать с handoff
- как принимать решения
- как общаться с оператором
- как писать промпты dev-агентам
- чего не делать

Хороший prompt отвечает на вопрос:

**"Как я должен действовать в этой роли?"**

Итого:

- `handoff` = **состояние проекта**
- `prompt` = **операционная инструкция для агента**

Они должны пересекаться, но не быть почти копией друг друга.

---

## 2. Что такое Craton

Craton — это проект управления домашней инфраструктурой вокруг двух бинарников:

- `cratond` — production-oriented Rust daemon для мониторинга и управления инфраструктурой домашнего ARM64 сервера
- `cratonctl` — thin operator CLI поверх localhost HTTP API `cratond`

Практический смысл проекта:

- следить за сервисами на домашнем сервере
- автоматически и предсказуемо выполнять recovery
- делать backup через явный FSM
- следить за диском и делать безопасную cleanup-логику
- давать operator-facing histories и explainable state
- оставаться надёжным, понятным и audit-friendly

Это **не** general-purpose platform, не plugin-host и не self-expanding automation framework.

Это локальный, closed-world, operator-centered daemon для реальной домашней инфраструктуры.

---

## 3. Философия проекта

Проект строится вокруг таких приоритетов:

1. correctness
2. crash safety
3. deterministic behavior
4. explainability
5. operator signal/noise ratio
6. observability
7. UX оператора
8. новые фичи

Важная ментальная модель:

- если всё хорошо — система молчит
- если плохо — система говорит кратко, точно и по делу
- лучше не реализовать сомнительную идею, чем добавить красивую, но опасную механику
- лучше маленький reviewable diff, чем “умная” большая система

---

## 4. Жёсткие архитектурные инварианты

Оркестратор должен всегда защищать эти инварианты:

- single-writer mutable state
- reducer не делает I/O
- effect layer не принимает policy-решения
- monotonic и epoch/wall time не смешиваются
- critical files пишутся атомарно
- no shell
- operator-facing сообщения на русском
- closed-world модель probe/action/policy
- no plugin system
- no user scripting
- no async runtime
- no thread pool

Если предлагается изменение, которое давит на любой из этих пунктов, это должно быть явно отмечено как риск.

---

## 5. Что уже есть в проекте

### `cratond` уже умеет

- health monitoring сервисов
- dependency-aware recovery
- breaker / flapping semantics
- backup FSM через `restic`
- crash recovery для backup FSM
- disk monitoring / cleanup
- localhost HTTP API
- remediation API с Bearer token
- maintenance mode
- durable outbox для уведомлений
- replay недоставленных alerts после рестарта
- histories:
  - recovery
  - backup
  - remediation
- diagnose endpoint
- systemd watchdog integration
- typed `/health`

### `cratonctl` уже умеет

- `health`
- `auth status`
- `status`
- `services`
- `service <id>`
- `history recovery|backup|remediation`
- `diagnose <service>`
- `doctor`
- `trigger <task>`
- `restart <service>`
- `maintenance set|clear`
- `breaker clear`
- `flapping clear`
- `backup run`
- `backup unlock`
- `disk cleanup`
- `--json`
- `--quiet`
- `--no-color`

Важно: `cratonctl` — это **не второй контроллер**, а thin client к daemon API.

---

## 6. Текущий этап проекта

Проект находится в фазе:

**deployed / stable / post-smoke / incremental hardening**

Что это значит practically:

- daemon уже работает на реальном ARM64 сервере
- он уже пережил реальные smoke-tests
- базовая архитектура уже сложилась
- сейчас нельзя относиться к проекту как к черновику
- любые изменения должны быть малыми, reviewable и легко откатываемыми

Это уже не стадия "собрать прототип".
Это стадия "укреплять, шлифовать, расширять осторожно".

---

## 7. Что было сделано на последних этапах

Ниже перечислены важные недавние изменения, которые оркестратор должен знать как часть текущего состояния.

### 7.1 Приведение `cratond` к deploy-ready состоянию

Были закрыты ключевые production gaps:

- исправлен watchdog semantics
- исправлен false recovery (`Unknown -> Healthy` больше не считается recovery)
- устранён hardcoded fallback в backup crash recovery
- добавлены regression/integration tests
- зелёные `cargo test` и `cargo clippy`

### 7.2 Notification / snapshot / health polish

Были сделаны важные улучшения observability:

- `/health` стал typed и перестал опираться на строковые эвристики
- disk monitor теперь принимает решение по свежему sample, а не по stale sample прошлого цикла
- startup semantics стали различать тип запуска
- notifier snapshot начал публиковать typed runtime fields

### 7.3 Startup / reboot awareness

Добавлена логика определения типа запуска через boot session semantics:

- `first_start`
- `daemon_restart`
- `host_boot`
- `unknown`

Это используется для startup alert/log semantics.

### 7.4 Notification channel observability

Добавлены typed runtime fields состояния notification channel:

- `notify_degraded`
- `notify_consecutive_failures`
- `notify_last_success_epoch_secs`
- `notify_last_failure_epoch_secs`

Это сделано как observability signal, а не как отдельная алертная платформа.

### 7.5 Startup kind в snapshot

`startup_kind` также опубликован в snapshot как typed field.

---

## 8. Важные уже принятые решения

Эти решения не стоит пересматривать без серьёзной причины:

### 8.1 Не делаем self-alerting при деградации `ntfy`

Было принято решение **не** делать отдельный operator-facing alert вида
"канал уведомлений деградировал", потому что:

- такой alert идёт в тот же деградировавший канал
- это создаёт риск recursion/noise/outbox pollution
- лучше считать `ntfy` degraded dependency и отражать это в snapshot + journald

### 8.2 Не строим вторую notification platform сейчас

Пока **не** делаем:

- email fallback platform
- telegram fallback platform
- multi-channel routing engine
- сложную policy engine для уведомлений

### 8.3 Recovery / backup / disk — только малыми шагами

Эти подсистемы уже достаточно критичны, чтобы их менять только очень небольшими patch'ами с тестами.

---

## 9. Где мы остановились

На момент этого handoff проект остановился в такой точке:

### Уже завершено

- startup/reboot awareness
- typed notify runtime fields в snapshot
- startup semantics polish
- публикация `startup_kind` в snapshot
- базовая notification observability для operator tooling

### Сознательно НЕ сделано

- нет отдельного self-alert'а про деградацию notification channel
- нет второй notification platform
- нет полноценного coalescing engine для alerts

### Следующие вероятные хорошие шаги

Наиболее вероятные хорошие ближайшие шаги:

1. сделать `cratonctl status` и/или `cratonctl doctor` более явными по новым snapshot-полям:
   - `startup_kind`
   - `notify_degraded`
   - `notify_consecutive_failures`
2. аккуратно продумать и, возможно, реализовать **очень узкий** coalescing для recovery success alerts
3. продолжать observability polish без архитектурного раздувания

Иными словами: мы находимся в фазе, где лучший следующий шаг — это не новая большая подсистема, а **повышение качества operator signal**.

---

## 10. Что оркестратор должен помнить из-за отсутствия доступа к коду

Это критично.

Оркестратор **не видит репозиторий напрямую**.
Значит, он не должен притворяться, что может:

- открыть файл
- проверить `git status`
- прогнать `cargo test`
- сравнить документацию с кодом самостоятельно

Вместо этого оркестратор должен работать через:

1. этот handoff
2. присланные оператором артефакты
3. summaries/dev-reports от coding agents
4. pasted diffs / snippets / test outputs
5. server observations

Правильное поведение оркестратора в таких условиях:

- если факт можно знать только из кода — попросить последний проверенный summary
- если нужно предложить шаг — делать это на основе handoff + свежих отчётов
- если есть риск устаревшего контекста — явно назвать его

---

## 11. Практическая рабочая схема взаимодействия

Обычно у оркестратора будет такой поток информации:

- оператор приносит задачу или вопрос
- dev-агент по `cratond` приносит diff/test/clippy summary
- dev-агент по `cratonctl` приносит CLI/design summary
- оператор приносит наблюдения с сервера

Оркестратор должен:

- свести это в общую картину
- отделить факты от догадок
- выбрать следующий шаг
- при необходимости выдать prompt для следующего dev-агента

---

## 12. Что оркестратор должен считать хорошими направлениями развития

На текущем этапе проекта хорошими направлениями считаются:

- observability polish
- explainable operator semantics
- signal/noise ratio alert'ов
- startup/reboot semantics
- snapshot/API typed fields
- `cratonctl status/doctor/auth status`
- backup safety
- recovery correctness
- documentation accuracy
- deploy/recovery ergonomics

То, что нужно оценивать очень осторожно:

- новые persisted files/fields
- расширение API surface area
- новые probe/action types
- notification coalescing
- изменения в backup/recovery policy

---

## 13. Что оркестратор должен считать плохими идеями по умолчанию

Без очень сильного обоснования не стоит продавливать:

- async runtime
- thread pool
- plugin system
- user scripting / Python scripting
- arbitrary probe extensibility
- Go helpers внутри ядра демона
- hot-reload config
- second notification channel platform
- TUI/web UI как control plane
- multi-node mode
- тяжёлую observability platform

---

## 14. Операционный статус, который стоит считать последним известным

Последний известный локально проверенный статус проекта на момент подготовки этого handoff:

- `cargo test`: зелёный
  - `cratonctl`: 45 tests passed
  - `cratond`: 141 tests passed
- `cargo clippy --all-targets -- -D warnings`: зелёный

Это важно воспринимать как **последний известный подтверждённый статус**, а не как вечную истину.
Если после этого были новые изменения, оркестратору нужен обновлённый summary от dev-агента.

---

## 15. Что обязательно просить у dev-агента перед принятием решения

Если оркестратор не уверен в текущем состоянии, нужно просить у dev-агента:

- `diff --stat`
- краткое описание изменения
- какие файлы затронуты
- `cargo test` итог
- `cargo clippy` итог
- какие тесты добавлены
- какие ограничения/риски остались

Это заменяет прямой repo access.

---

## 16. Что вручную вставлять в новую сессию оркестратора

Рекомендуемые placeholders:

```text
<PASTE_CONSTITUTION_OR_SUMMARY>
<PASTE_LATEST_PROJECT_STATUS_SUMMARY>
<PASTE_RECENT_DEV_AGENT_OUTPUTS>
<PASTE_SERVER_OBSERVATIONS>
<PASTE_CURRENT_PRIORITIES>
<PASTE_OPEN_ARCH_QUESTIONS>
```

Минимально полезный набор:

- краткая выжимка конституции
- этот handoff
- последний summary от dev-агента по `cratond`
- последний summary от dev-агента по `cratonctl`
- текущие приоритеты

---

## 17. Короткий summary в 10 строках

Если оркестратору нужно быстро вспомнить проект:

- `cratond` — deployed Rust daemon для домашней инфраструктуры
- `cratonctl` — thin CLI к localhost API демона
- проект уже в реальной эксплуатации
- главные ценности: correctness, determinism, explainability
- архитектура жёсткая: single-writer, reducer без I/O, effect без policy
- недавно добавлены startup/reboot awareness и typed notify runtime fields
- `startup_kind` уже есть в snapshot
- self-alerting для деградации `ntfy` сознательно не реализован
- лучший следующий тип шагов — small observability/operator UX polish
- оркестратор не видит код и должен работать через handoff + summaries
