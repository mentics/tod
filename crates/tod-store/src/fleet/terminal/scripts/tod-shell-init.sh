#!/usr/bin/env bash
# tod shell session bootstrap — sourced by agent shell terminals
_tod_shell_id="${1:?shell id required}"
_tod_state_dir="${2:?state dir required}"
_tod_cwd="${3:-}"
_tod_backend="${TOD_TERMINAL_BACKEND:-posix}"

_tod_state_path="$_tod_state_dir/$_tod_shell_id.json"
if [ ! -f "$_tod_state_path" ] && [ "$_tod_backend" != "git_bash" ]; then
  _tod_pid=$$
  if [ "$_tod_backend" = "git_bash" ]; then
    _tod_ppid=$(ps -o ppid= -p $$ 2>/dev/null | tr -d ' \r\n' || true)
    if [ -z "$_tod_ppid" ]; then
      _tod_ppid=$(awk '/^PPid:/{print $2; exit}' /proc/$$/status 2>/dev/null || true)
    fi
    if [ -n "$_tod_ppid" ]; then
      _tod_pid="$_tod_ppid"
    fi
  fi
  _tod_tty=$(tty 2>/dev/null | tr -d '\r\n' || true)
  _tod_json=$(printf '{"pid":%s,"tty":"%s","backend":"%s"}' "$_tod_pid" "$_tod_tty" "$_tod_backend")

  _tmp="$_tod_state_dir/$_tod_shell_id.json.tmp"
  _out="$_tod_state_dir/$_tod_shell_id.json"
  printf '%s\n' "$_tod_json" > "$_tmp" && mv "$_tmp" "$_out"
fi

if [ -n "$_tod_cwd" ] && [ -d "$_tod_cwd" ]; then
  cd "$_tod_cwd" || true
fi

_env_hook="$_tod_state_dir/env.sh"
if [ -f "$_env_hook" ]; then
  # shellcheck disable=SC1090
  source "$_env_hook"
fi

unset _tod_shell_id _tod_state_dir _tod_cwd _tod_backend _tod_pid _tod_ppid _tod_tty _tod_json _tmp _out _env_hook
