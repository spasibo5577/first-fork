# Deploy Guide

Этот документ описывает практический деплой `cratond` и `cratonctl` на
домашний ARM64 Linux сервер.

## Что должно быть на сервере

Минимум:

- Linux с `systemd`
- `restic`
- `systemctl`
- `journalctl`
- доступ к `ntfy`, если уведомления включены

В зависимости от probe и сервисов также могут быть нужны:

- `tailscale`
- `docker`
- DNS/HTTP сервисы из конфига

## Сборка

### Нативная сборка

Если собираешь прямо на целевой машине:

```bash
cargo build --release --bin cratond --bin cratonctl
```

### Кросс-компиляция для ARM64

Если собираешь с x86_64 хоста:

```bash
rustup target add aarch64-unknown-linux-gnu
cargo install cross
cross build --release --target aarch64-unknown-linux-gnu --bin cratond --bin cratonctl
```

Ожидаемые бинарники:

- `target/aarch64-unknown-linux-gnu/release/cratond`
- `target/aarch64-unknown-linux-gnu/release/cratonctl`

## Копирование на сервер

```bash
scp target/aarch64-unknown-linux-gnu/release/cratond server:/tmp/cratond
scp target/aarch64-unknown-linux-gnu/release/cratonctl server:/tmp/cratonctl
scp config.example.toml server:/tmp/config.toml
scp deploy/cratond.service server:/tmp/cratond.service
```

На сервере:

```bash
sudo install -m 0755 /tmp/cratond /usr/local/bin/cratond
sudo install -m 0755 /tmp/cratonctl /usr/local/bin/cratonctl
sudo install -d -m 0755 /etc/craton
sudo install -d -m 0700 /var/lib/craton
sudo install -d -m 0755 /run/craton
sudo install -m 0600 /tmp/config.toml /etc/craton/config.toml
sudo install -m 0644 /tmp/cratond.service /etc/systemd/system/cratond.service
```

## Настройка `config.toml`

Не копируй пример без правок в production. Перед запуском проверь:

- `daemon.listen`
- `ntfy.url`
- `ntfy.topic`
- `backup.restic_repo`
- `backup.restic_password_file`
- `backup.paths`
- `updates.*`
- `service.unit`
- `service.depends_on`
- `service.probe`

Если используешь AI-секцию:

- текущий код использует поле `picoclaw_url`
- не документируй это как ZeroClaw integration, если у тебя её нет в коде

## Установка systemd unit

Актуальный unit в репозитории:

- [deploy/cratond.service](/a:/cratond/deploy/cratond.service)

Ключевые параметры текущего unit:

- `Type=notify`
- `ExecStart=/usr/local/bin/cratond /etc/craton/config.toml`
- `WatchdogSec=30`
- `RuntimeDirectory=craton`
- `StateDirectory=craton`
- `LogsDirectory=craton`

Проверь, что права и пути соответствуют твоему окружению:

- `/var/lib/craton`
- `/run/craton`
- путь к `restic_password_file`
- путь к возможному AI state

## Запуск

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now cratond
sudo systemctl status cratond
```

Проверить, что бинарь доступен:

```bash
which cratond
which cratonctl
```

## Smoke-test

### Демон

```bash
curl -s http://127.0.0.1:18800/health
curl -s http://127.0.0.1:18800/api/v1/state | python3 -m json.tool
```

Ожидаемо:

- `/health` возвращает `200` и `{"status":"ok","reason":"ok"}` на свежем snapshot
- `/api/v1/state` возвращает JSON object

### CLI

```bash
cratonctl health
cratonctl status
cratonctl services
cratonctl doctor
cratonctl --json history recovery
```

### Mutating smoke-test

После первого старта проверь token path:

```bash
ls -l /var/lib/craton/remediation-token
```

Если нужно проверить доступность mutating path под текущим пользователем:

```bash
cratonctl doctor
```

Затем попробуй безопасную ручную команду:

```bash
cratonctl maintenance set ntfy --reason "тест после деплоя"
cratonctl maintenance clear ntfy
```

## Проверка journald и watchdog

```bash
journalctl -u cratond -n 100 --no-pager
systemctl show cratond -p WatchdogUSec
```

`cratond` шлёт `WATCHDOG=1` независимо от due tasks, поэтому watchdog не должен
зависеть только от очередного recovery tick.

## Проверка backup

Перед первым production backup убедись, что:

- `restic` установлен
- `backup.restic_repo` доступен
- `backup.restic_password_file` читается демоном
- сервисы с `backup_stop = true` реально можно остановить и поднять обратно

Ручной запуск:

```bash
cratonctl backup run
cratonctl --json history backup
```

## Проверка history и diagnose

```bash
cratonctl history recovery
cratonctl history backup
cratonctl history remediation
cratonctl diagnose ntfy
```

## Rollback basics

Если новый бинарь ведёт себя плохо:

1. останови сервис
2. верни предыдущий бинарь `cratond`
3. при необходимости верни предыдущий `config.toml`
4. запусти сервис снова
5. проверь `/health` и `cratonctl status`

Пример:

```bash
sudo systemctl stop cratond
sudo install -m 0755 /usr/local/bin/cratond.previous /usr/local/bin/cratond
sudo systemctl start cratond
cratonctl health
```

Что важно:

- не редактируй вручную `backup-state.json` и `maintenance.json`
- не удаляй remediation token без необходимости
- rollback должен идти через бинарник, unit и config, а не через правку state

## Частые проблемы

### Демон не стартует

Проверь:

- `journalctl -u cratond -n 200 --no-pager`
- валидность `config.toml`
- существование всех unit из `[[service]]`
- доступность `restic`

### `cratonctl` не может выполнять mutating-команды

Проверь:

- есть ли `/var/lib/craton/remediation-token`
- читается ли он нужным пользователем
- не пустой ли token file
- совпадает ли `--url`
- не пытаешься ли использовать `https://`

Начни с:

```bash
cratonctl doctor
```

### `/health` даёт 503

Смотри `reason`:

- `stale_snapshot`
- `outbox_overflow`
- `shutting_down`

Это typed сигнал о состоянии control plane демона, а не обязательно о состоянии всех сервисов.
