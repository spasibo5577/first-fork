#!/usr/bin/env bash
set -euo pipefail

CRATOND_NEW="/tmp/cratond.new"
CRATONCTL_NEW="/tmp/cratonctl.new"
CRATOND_BIN="/usr/local/bin/cratond"
CRATONCTL_BIN="/usr/local/bin/cratonctl"
CRATOND_PREVIOUS="/usr/local/bin/cratond.previous"
CRATONCTL_PREVIOUS="/usr/local/bin/cratonctl.previous"
SERVICE_NAME="cratond"

log() {
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] [install-remote] $*"
}

rollback() {
  log "Smoke-test failed, starting rollback"
  log "Stopping $SERVICE_NAME before restoring previous binaries"
  systemctl stop "$SERVICE_NAME"
  sleep 1

  if [[ -f "$CRATOND_PREVIOUS" ]]; then
    cp "$CRATOND_PREVIOUS" "$CRATOND_BIN"
    chmod 755 "$CRATOND_BIN"
    log "Restored previous cratond binary"
  else
    log "Previous cratond binary not found, rollback is partial"
  fi

  if [[ -f "$CRATONCTL_PREVIOUS" ]]; then
    cp "$CRATONCTL_PREVIOUS" "$CRATONCTL_BIN"
    chmod 755 "$CRATONCTL_BIN"
    log "Restored previous cratonctl binary"
  else
    log "Previous cratonctl binary not found, cratonctl rollback skipped"
  fi

  systemctl start "$SERVICE_NAME"
  log "Rollback complete, previous version restored"
}

if [[ "${EUID}" -ne 0 ]]; then
  echo "[install-remote] This script must be run with sudo/root" >&2
  exit 1
fi

log "Checking uploaded binaries"
[[ -f "$CRATOND_NEW" ]] || { echo "[install-remote] Missing $CRATOND_NEW" >&2; exit 1; }
[[ -f "$CRATONCTL_NEW" ]] || { echo "[install-remote] Missing $CRATONCTL_NEW" >&2; exit 1; }

if [[ -f "$CRATOND_BIN" ]]; then
  log "Backing up current cratond binary"
  cp "$CRATOND_BIN" "$CRATOND_PREVIOUS"
else
  log "Current cratond binary not found, skipping cratond backup"
fi

if [[ -f "$CRATONCTL_BIN" ]]; then
  log "Backing up current cratonctl binary"
  cp "$CRATONCTL_BIN" "$CRATONCTL_PREVIOUS"
else
  log "Current cratonctl binary not found, skipping cratonctl backup"
fi

log "Stopping $SERVICE_NAME"
systemctl stop "$SERVICE_NAME"

log "Installing new cratond binary"
mv "$CRATOND_NEW" "$CRATOND_BIN"
chmod 755 "$CRATOND_BIN"

log "Installing new cratonctl binary"
mv "$CRATONCTL_NEW" "$CRATONCTL_BIN"
chmod 755 "$CRATONCTL_BIN"

log "Starting $SERVICE_NAME"
systemctl start "$SERVICE_NAME"

log "Waiting for daemon to become healthy"
HEALTHY=false
for i in $(seq 1 15); do
  sleep 2
  if curl -sf http://127.0.0.1:18800/health >/dev/null 2>&1; then
    HEALTHY=true
    log "Daemon healthy after $((i * 2)) seconds"
    break
  fi
  log "Attempt ${i}/15: daemon not healthy yet..."
done

if [[ "$HEALTHY" != "true" ]]; then
  log "ERROR: daemon did not become healthy within 30 seconds"
  rollback
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] [install-remote] Smoke-test failed after deploy" >&2
  exit 1
fi

log "Running cratonctl smoke-tests"
if ! {
  timeout 10s "$CRATONCTL_BIN" health >/dev/null
  timeout 10s "$CRATONCTL_BIN" status >/dev/null
  timeout 10s "$CRATONCTL_BIN" services >/dev/null
}; then
  rollback
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] [install-remote] CLI smoke-test failed after deploy" >&2
  exit 1
fi

log "Smoke-tests passed"
echo ""
log "=== Deploy successful ==="
"$CRATONCTL_BIN" health
"$CRATONCTL_BIN" status
echo ""
log "Install completed successfully"
