# Deploy Guide

Этот документ описывает актуальный деплой `cratond` и `cratonctl` на ARM64 Linux сервер с systemd.

## Что есть в репозитории

- [scripts/deploy.ps1](../scripts/deploy.ps1) — локальный Windows / PowerShell deploy helper
- [scripts/install-remote.sh](../scripts/install-remote.sh) — серверный install script
- [deploy/cratond.service](../deploy/cratond.service) — systemd unit
- [config.example.toml](../config.example.toml) — пример конфига

## Prerequisites

Локально:

- Rust toolchain
- target `aarch64-unknown-linux-gnu`
- `cargo-zigbuild`
- Zig
- `ssh` и `scp`

На сервере:

- ARM64 Linux
- systemd
- `curl`
- `systemctl`
- `timeout`

## First-time setup

После установки бинарников на новый сервер:

```bash
sudo cratonctl init
```

`cratonctl init`:

- создаёт `/etc/craton`
- создаёт `/var/lib/craton`
- создаёт `/run/craton`
- пишет `/etc/craton/config.toml`, если файла ещё нет
- создаёт `/var/lib/craton/remediation-token`, если файла ещё нет
- ставит `/etc/systemd/system/cratond.service`, если файла ещё нет
- делает `systemctl daemon-reload`, если unit был создан

После этого:

1. вручную проверить `config.toml`
2. включить и запустить daemon

```bash
sudo systemctl enable --now cratond
```

## Automated deploy from Windows

Запуск:

```powershell
pwsh -File .\scripts\deploy.ps1
```

### Что делает `scripts/deploy.ps1`

Скрипт:

1. добавляет Zig в `PATH`
2. запускает:

```powershell
cargo zigbuild --release --target aarch64-unknown-linux-gnu --bin cratond --bin cratonctl
```

3. проверяет, что бинарники существуют в:

```text
target\aarch64-unknown-linux-gnu\release\
```

4. копирует:
   - `cratond` -> `/tmp/cratond.new`
   - `cratonctl` -> `/tmp/cratonctl.new`
   - `install-remote.sh` -> `/tmp/install-remote.sh`
5. запускает удалённый install через:

```text
ssh user@host "sudo bash /tmp/install-remote.sh"
```

### Что настраивается в начале `deploy.ps1`

- путь к Zig
- target triple
- SSH user / host
- remote temporary paths

## Remote install on the server

Запускается через `deploy.ps1`, но можно вызвать и вручную:

```bash
sudo bash /tmp/install-remote.sh
```

### Что делает `scripts/install-remote.sh`

Скрипт:

1. проверяет наличие `/tmp/cratond.new` и `/tmp/cratonctl.new`
2. делает backup текущих бинарников в:
   - `/usr/local/bin/cratond.previous`
   - `/usr/local/bin/cratonctl.previous`
3. останавливает `cratond`
4. ставит новые бинарники в `/usr/local/bin`
5. запускает `cratond`
6. делает smoke-test:
   - `curl -sf http://127.0.0.1:18800/health`
   - `cratonctl health`
7. если smoke-test не прошёл — откатывает бинарники назад

Скрипт не трогает:

- `config.toml`
- state files
- remediation token
- systemd unit

## Manual deploy

Если automation не подходит:

1. собрать бинарники
2. скопировать их на сервер
3. остановить daemon
4. заменить бинарники
5. запустить daemon
6. прогнать smoke-test

Пример:

```bash
sudo systemctl stop cratond
sudo install -m 0755 /tmp/cratond /usr/local/bin/cratond
sudo install -m 0755 /tmp/cratonctl /usr/local/bin/cratonctl
sudo systemctl start cratond
curl -sf http://127.0.0.1:18800/health
cratonctl health
cratonctl status
```

## systemd unit

Актуальный unit:

- [deploy/cratond.service](../deploy/cratond.service)

Ключевые свойства:

- `Type=notify`
- `ExecStart=/usr/local/bin/cratond /etc/craton/config.toml`
- `Restart=on-failure`
- `WatchdogSec=30`
- `RuntimeDirectory=craton`
- `StateDirectory=craton`
- `LogsDirectory=craton`

## Smoke test checklist

После первого запуска или обновления:

```bash
curl -sf http://127.0.0.1:18800/health
curl -s http://127.0.0.1:18800/api/v1/state | python3 -m json.tool
cratonctl health
cratonctl status
cratonctl services
cratonctl doctor
cratonctl auth status
```

Если нужен mutating smoke-test:

```bash
cratonctl maintenance set ntfy --reason "тест после деплоя"
cratonctl maintenance clear ntfy
```

## Rollback

Если новый бинарь не проходит smoke-test:

```bash
sudo systemctl stop cratond
sudo cp /usr/local/bin/cratond.previous /usr/local/bin/cratond
sudo chmod 755 /usr/local/bin/cratond
sudo cp /usr/local/bin/cratonctl.previous /usr/local/bin/cratonctl
sudo chmod 755 /usr/local/bin/cratonctl
sudo systemctl start cratond
```

После rollback:

```bash
curl -sf http://127.0.0.1:18800/health
cratonctl health
cratonctl status
```

## Что проверить руками перед production deploy

- корректность `/etc/craton/config.toml`
- реальные unit names в `[[service]]`
- доступность `backup.restic_repo`
- корректность `backup.restic_password_file`
- права на `/var/lib/craton/remediation-token`
- что `systemd` unit действительно соответствует окружению

## Полезные команды

```bash
sudo systemctl status cratond
journalctl -u cratond -n 200 --no-pager
systemctl show cratond -p WatchdogUSec
cratonctl health
cratonctl doctor
cratonctl auth status
```
