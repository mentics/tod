#!/usr/bin/env bash
# tod's `db-query` for a cloud node's sandbox: the workspace's own `db-query`
# signs a prod login with the aws CLI and the user's AWS session, neither of
# which a sandbox has. Here the login is the user's machine's, brokered by
# `tod-cli db` (doc/cloud-sandboxes/database-access.md), so a database the user
# set up under the same name (prod-platform, prod-kb) works with the same
# arguments and the same JSON output. Every other id (local-*, prod-paradedb)
# goes to the workspace's own script, which never needed the aws CLI.
#
#   db-query DB_ID [SQL]      (SQL from stdin when omitted)
#   db-query --list
#
# The session is read-only and capped like the workspace's script.

set -euo pipefail

die() { echo "db-query: $*" >&2; exit 1; }

# The workspace's own db-query, not this file: the next one on PATH, else the
# one under scripts/shell/bin above the working directory.
self="$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")"
find_real() {
  local d c
  local -a dirs
  IFS=: read -ra dirs <<< "$PATH"
  for d in "${dirs[@]}"; do
    c="$d/db-query"
    if [[ -x "$c" && "$(cd "$d" && pwd -P)/db-query" != "$self" ]]; then printf '%s' "$c"; return 0; fi
  done
  d="$PWD"
  while [[ "$d" != "/" ]]; do
    c="$d/scripts/shell/bin/db-query"
    if [[ -x "$c" && "$c" != "$self" ]]; then printf '%s' "$c"; return 0; fi
    d="$(dirname "$d")"
  done
  return 1
}

if [[ $# -lt 1 || "$1" == "--help" || "$1" == "-h" ]]; then
  echo "Usage: db-query DB_ID [SQL]" >&2
  echo "       db-query DB_ID < query.sql" >&2
  echo "       db-query --list" >&2
  echo "Here prod-platform and prod-kb are logged in through the user's machine (tod-cli db)." >&2
  exit 1
fi

BROKER_HINT="the user's machine must be on with tod running and their AWS session signed in; only the user can fix that, so record what you need from the database as a request for them instead of retrying"

brokered_names() { tod-cli db list 2>&1 | sed -n 's/^\([A-Za-z0-9._-]*\) (database .*/\1/p'; }

if [[ "$1" == "--list" ]]; then
  out="$(tod-cli db list 2>&1)" || die "tod-cli db list failed: $out ($BROKER_HINT)"
  brokered_names
  if real="$(find_real)"; then "$real" --list 2>/dev/null | grep -Ev '^prod-(platform|kb)(-postgres)?$' || true; fi
  exit 0
fi

DB_ID="$1"
case "$DB_ID" in
  local-* | prod-paradedb | prod-paradedb-postgres)
    real="$(find_real)" || die "no workspace db-query found for $DB_ID (run it from the workspace checkout)"
    exec "$real" "$@"
    ;;
esac
shift

if [[ $# -ge 1 ]]; then SQL="$1"; else SQL="$(cat)"; fi
[[ -n "$SQL" ]] || die "empty SQL"
SQL=$(printf '%s' "$SQL" | sed 's/[[:space:]]*;[[:space:]]*$//')

command -v jq >/dev/null 2>&1 || die "jq not found"
command -v psql >/dev/null 2>&1 || die "psql not found"

list_out="$(tod-cli db list 2>&1)" || die "$list_out ($BROKER_HINT)"
if ! brokered_names | grep -qx -- "$DB_ID"; then
  have="$(brokered_names | tr '\n' ' ')"
  die "database '$DB_ID' is not set up for this node (set up: ${have:-none}). Do not use the aws CLI or a login of your own: record a request asking the user to add '$DB_ID' under Settings > Databases"
fi

QUERY_FILE=$(mktemp)
PSQL_OUT=$(mktemp)
PSQL_ERR=$(mktemp)
trap 'rm -f "$QUERY_FILE" "$PSQL_OUT" "$PSQL_ERR"' EXIT

FIRST_WORD=$(printf '%s\n' "$SQL" | sed -E '/^[[:space:]]*(--.*)?$/d' | head -n 1 \
  | sed -E 's/^[[:space:]]*//' | cut -d' ' -f1 | tr '[:lower:]' '[:upper:]')
WRAP=0
ROW_CAP=0
if [[ ! "$FIRST_WORD" =~ ^(EXPLAIN|INSERT|UPDATE|DELETE|CREATE|DROP|ALTER|TRUNCATE|GRANT|REVOKE)$ ]]; then
  WRAP=1
  inner="$SQL"
  if [[ ! "$(printf '%s' "$SQL" | tr '[:lower:]' '[:upper:]')" =~ LIMIT ]]; then
    ROW_CAP=500
    inner="SELECT * FROM ($SQL) AS _inner LIMIT $ROW_CAP"
  fi
  printf 'SELECT coalesce(json_agg(row_to_json(t)), '"'"'[]'"'"'::json) FROM (\n%s\n) AS t;\n' "$inner" > "$QUERY_FILE"
else
  printf '%s;\n' "$SQL" > "$QUERY_FILE"
fi

# Session limits in the connection, as the workspace's script sets them.
export PGOPTIONS="-c default_transaction_read_only=on -c statement_timeout=30000 -c lock_timeout=60000"
if ! tod-cli db run "$DB_ID" -- psql -X -v ON_ERROR_STOP=1 -t -A -c '\timing on' -f "$QUERY_FILE" > "$PSQL_OUT" 2> "$PSQL_ERR"; then
  jq -n --arg error "$(cat "$PSQL_ERR") ($BROKER_HINT, unless the error above is the query's own)" --arg db "$DB_ID" '{success: false, error: $error, db: $db}'
  exit 1
fi

EXEC_MS=$(awk '/^Time: /{v=$2; if ($3 == "s") v *= 1000; ms = v} END{if (ms != "") printf "%.2f", ms}' "$PSQL_OUT")

if [[ "$WRAP" -eq 0 ]]; then
  grep -v -E '^(Time:|Timing is on\.)' "$PSQL_OUT" || true
  [[ -n "$EXEC_MS" ]] && echo "Time: ${EXEC_MS} ms"
  exit 0
fi

JQ_FLAGS=(-c)
[[ -t 1 ]] && JQ_FLAGS=()
(grep -m1 '^\[' "$PSQL_OUT" || echo '[]') | jq "${JQ_FLAGS[@]}" \
  --arg db "$DB_ID" --arg ms "$EXEC_MS" --argjson cap "$ROW_CAP" '
  (if type == "array" then . else [.] end) as $rows
  | {success: true,
     data: ({rows: $rows, count: ($rows | length), db: $db,
             execution_time_ms: (if $ms == "" then null else ($ms | tonumber) end)}
           + (if $cap > 0 and ($rows | length) == $cap then {row_cap_applied: $cap} else {} end))}'
