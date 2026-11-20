#!/usr/bin/env bash
# Installs the LifeUI agent's systemd units on the Ubuntu home server and enables its timers.
#
#   sudo tools/server/install_server.sh            # install or update, then enable the timers
#   sudo tools/server/install_server.sh --check    # only check the setup, change nothing
#
# Expects (see README, "Home server"): the repo at /opt/lifeui owned by lifeui, a venv at /opt/lifeui/.venv,
# and /opt/lifeui/.env.agent owned by lifeui with mode 600. Re-run it after pulling changes to the unit files.
set -euo pipefail

APP_DIR=${APP_DIR:-/opt/lifeui}
APP_USER=${APP_USER:-lifeui}
UNIT_DIR=/etc/systemd/system
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# Agent runs (morning, midday, evening, notes) and the daily news run (08:30, before the morning agent run).
RETIRED_TIMERS=()
TIMERS=(lifeui-agent-cloud-morning.timer lifeui-agent-cloud-midday.timer lifeui-agent-cloud-evening.timer lifeui-agent-pc.timer lifeui-news.timer)
CHECK_ONLY=0
[[ "${1:-}" == "--check" ]] && CHECK_ONLY=1

problems=0
fail() { echo "FAIL $*" >&2; problems=$((problems + 1)); }
ok() { echo "ok   $*"; }

[[ $EUID -eq 0 ]] || { echo "run as root (sudo $0)" >&2; exit 1; }

id "$APP_USER" >/dev/null 2>&1 && ok "user $APP_USER exists" || fail "user $APP_USER is missing (useradd --system --home-dir $APP_DIR --shell /usr/sbin/nologin $APP_USER)"
[[ -f "$APP_DIR/tools/agent_runner.py" ]] && ok "repo at $APP_DIR" || fail "no repo at $APP_DIR"
[[ -x "$APP_DIR/.venv/bin/python" ]] && ok "venv: $("$APP_DIR/.venv/bin/python" --version 2>&1)" || fail "no venv at $APP_DIR/.venv"

env_file="$APP_DIR/.env.agent"
if [[ ! -f "$env_file" ]]; then
  fail "$env_file is missing (copy tools/agent.env.example and fill it in)"
else
  mode=$(stat -c %a "$env_file"); owner=$(stat -c %U "$env_file")
  [[ "$mode" == 600 ]] && ok ".env.agent mode 600" || fail ".env.agent has mode $mode, needs 600 (chmod 600 $env_file)"
  [[ "$owner" == "$APP_USER" ]] && ok ".env.agent owned by $APP_USER" || fail ".env.agent is owned by $owner, needs $APP_USER (chown $APP_USER: $env_file)"
  for key in LIFEUI_URL LIFEUI_RUNNER_TOKEN AGENT_PROVIDER; do
    grep -Eq "^${key}=.+" "$env_file" && ok ".env.agent sets $key" || fail ".env.agent does not set $key"
  done
  if grep -Eq '^AGENT_PROVIDER=ollama' "$env_file"; then
    systemctl is-active --quiet ollama && ok "ollama service is running" || fail "AGENT_PROVIDER=ollama but the ollama service is not running"
  fi
  vaults=$(sed -n 's/^OBSIDIAN_VAULT=//p' "$env_file" | tail -1)
  IFS=: read -ra vault_list <<< "$vaults"
  for vault in "${vault_list[@]}"; do
    [[ -n "$vault" ]] || continue
    sudo -u "$APP_USER" test -r "$vault" && ok "$APP_USER can read the vault $vault" || fail "$APP_USER cannot read the vault $vault"
    grep -Eq "^BindReadOnlyPaths=-?${vault}\$" "$HERE/lifeui-agent@.service" && ok "the sandbox shows $vault" \
      || fail "lifeui-agent@.service has no BindReadOnlyPaths=$vault, so the run would not see it"
  done
fi

if [[ $problems -gt 0 ]]; then
  echo "$problems problem(s): nothing installed." >&2
  exit 1
fi
[[ $CHECK_ONLY -eq 1 ]] && { echo "Setup looks right (--check: nothing changed)."; exit 0; }

for unit in lifeui-agent@.service lifeui-news.service "${TIMERS[@]}"; do
  install -m 644 -o root -g root "$HERE/$unit" "$UNIT_DIR/$unit"
done
for unit in "${RETIRED_TIMERS[@]}"; do
  if [[ -f "$UNIT_DIR/$unit" ]]; then
    systemctl disable --now "$unit" 2>/dev/null || true
    rm -f "$UNIT_DIR/$unit"
  fi
done
systemctl daemon-reload
systemctl enable --now "${TIMERS[@]}"
echo
systemctl list-timers 'lifeui-*' --all --no-pager
echo
echo "Installed. One run by hand:  sudo systemctl start lifeui-agent@cloud-morning && journalctl -u lifeui-agent@cloud-morning -n 30"
