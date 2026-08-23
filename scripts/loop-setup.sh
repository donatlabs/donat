#!/usr/bin/env bash
# Install (or remove) the nightly loop timer on this machine.
#
#   scripts/loop-setup.sh install   # check prerequisites, write the units, enable
#   scripts/loop-setup.sh status    # next run, last runs
#   scripts/loop-setup.sh remove    # disable and delete the units
#
# A systemd *user* timer rather than cron, for one property cron lacks:
# Persistent=true runs a night that was missed because the machine was off,
# the next time it is on. A desktop is off at night more often than not, and
# a loop that only runs on nights the machine happens to be up is not a
# nightly loop. The timer needs the user's session to exist while nobody is
# logged in, which is what `loginctl enable-linger` grants.
#
# Everything runs as you, with your `claude` login (the subscription, not an
# API key) and your `gh` login. Nothing here needs root.
set -euo pipefail

cmd="${1:-install}"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
units="$HOME/.config/systemd/user"
service="donat-loop-nightly.service"
timer="donat-loop-nightly.timer"
state="${DONAT_LOOP_STATE:-$HOME/.local/state/donat-loops}"
hour="${DONAT_LOOP_AT:-03:30}"

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing: $1 — $2" >&2
    exit 1
  fi
}

case "$cmd" in
  install)
    need claude "install Claude Code and log in (claude login)"
    need gh "install the GitHub CLI and log in (gh auth login)"
    need cargo "install the Rust toolchain (rustup)"
    need cargo-audit "cargo install cargo-audit"
    need systemctl "this installer is for systemd; on another init, run scripts/loop-nightly.sh from cron"
    gh auth status >/dev/null 2>&1 || { echo "gh is not logged in: gh auth login" >&2; exit 1; }
    git -C "$repo" remote get-url origin >/dev/null 2>&1 || { echo "no origin remote in $repo" >&2; exit 1; }

    mkdir -p "$units" "$state"
    # PATH is spelled out because a timer does not read your shell profile:
    # claude lives in ~/.local/bin, cargo and cargo-audit in ~/.cargo/bin.
    cat >"$units/$service" <<EOF
[Unit]
Description=donat nightly loops (scripts/loop-nightly.sh in $repo)

[Service]
Type=oneshot
WorkingDirectory=$repo
ExecStart=$repo/scripts/loop-nightly.sh
Environment=HOME=$HOME
Environment=PATH=$HOME/.local/bin:$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin
Environment=DONAT_LOOP_STATE=$state
EOF
    cat >"$units/$timer" <<EOF
[Unit]
Description=donat nightly loops at $hour

[Timer]
OnCalendar=*-*-* $hour:00
Persistent=true
RandomizedDelaySec=10m

[Install]
WantedBy=timers.target
EOF
    systemctl --user daemon-reload
    systemctl --user enable --now "$timer" >/dev/null
    if [ "$(loginctl show-user "$USER" -p Linger --value 2>/dev/null)" != "yes" ]; then
      if loginctl enable-linger "$USER" 2>/dev/null; then
        echo "linger enabled: the timer fires even when you are not logged in"
      else
        echo "could not enable linger; the timer fires only while you are logged in (sudo loginctl enable-linger $USER to fix)" >&2
      fi
    fi
    echo "installed: $units/$timer → $repo/scripts/loop-nightly.sh"
    echo "state:     $state (runs.jsonl, logs/, worktrees/, target/)"
    systemctl --user list-timers "$timer" --no-pager | sed -n '1,2p'
    ;;
  status)
    systemctl --user list-timers "$timer" --no-pager 2>/dev/null | sed -n '1,2p' || echo "timer not installed"
    if [ -f "$state/runs.jsonl" ]; then
      echo "last runs:"
      tail -n 10 "$state/runs.jsonl" | python3 -c '
import json, sys
for line in sys.stdin:
    e = json.loads(line)
    cost = f" ${e[\"cost_usd\"]:.2f}" if e.get("cost_usd") is not None else ""
    print(f"  {e[\"ts\"]}  {e[\"job\"]:<16} {e[\"outcome\"]:<14} {e[\"seconds\"]:>5}s{cost}  {e.get(\"pr\", \"\")}")'
    else
      echo "no runs yet ($state/runs.jsonl)"
    fi
    ;;
  remove)
    systemctl --user disable --now "$timer" >/dev/null 2>&1 || true
    rm -f "$units/$timer" "$units/$service"
    systemctl --user daemon-reload
    echo "removed $timer; state in $state is kept"
    ;;
  *)
    echo "usage: scripts/loop-setup.sh install|status|remove" >&2
    exit 2
    ;;
esac
