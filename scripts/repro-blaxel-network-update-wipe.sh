#!/usr/bin/env bash
# Repro for Blaxel support: updating the network (proxy) configuration of a
# sandbox that is a fork of an UPDATED FORK reboots it from its image and
# discards every file written since the image was built.
#
# Needs only the Blaxel CLI (`bl`, logged in to a workspace), python3 and the
# default image `blaxel/base-image:latest`. Creates throwaway sandboxes and
# deletes them on exit.
#
#   CHAIN  A (new sandbox, marker m1)
#            -> bl fork -> B, bl apply a network proxy to B, marker m2 in B
#            -> bl fork B -> C, bl apply a network proxy to C
#          Expected: C keeps m1 and m2, as B kept m1 after its own update.
#          Observed: C's /proc/sys/kernel/random/boot_id changes and BOTH
#          markers are gone.
#   CONTROL same, but B is NOT updated before C is forked from it: C keeps
#          everything after its update.
#
# Every other variant I tried keeps its files: updating a plain sandbox,
# updating a first-generation fork, forking an idle (STANDBY) source,
# updating twice.
set -u
SUFFIX="$(date +%s)-$$"
A="net-wipe-a-$SUFFIX"; B="net-wipe-b-$SUFFIX"; C="net-wipe-c-$SUFFIX"
CA="net-wipe-ca-$SUFFIX"; CB="net-wipe-cb-$SUFFIX"; CC="net-wipe-cc-$SUFFIX"
MARKER_DIRS="/root /tmp /home /usr/local /var/tmp /var/lib /opt"

bl() { command bl --skip-version-warning "$@" 2> >(grep -v -i 'new version' >&2); }

cleanup() {
  for n in "$A" "$B" "$C" "$CA" "$CB" "$CC"; do bl delete sandbox "$n" >/dev/null 2>&1; done
}
trap cleanup EXIT

yaml() { # name, with_proxy(0|1)
  cat <<YAML
apiVersion: blaxel.ai/v1alpha1
kind: Sandbox
metadata:
  name: $1
spec:
  runtime:
    image: blaxel/base-image:latest
    memory: 2048
YAML
  if [ "$2" = 1 ]; then
    cat <<YAML
  network:
    proxy:
      routing:
        - destinations: ["example.com"]
          headers:
            Authorization: "Bearer {{SECRET:token}}"
          secrets:
            token: "dummy-value"
YAML
  fi
}

status() {
  bl get sandbox "$1" -o json 2>/dev/null | python3 -c \
    'import sys,json;d=json.load(sys.stdin);d=d[0] if isinstance(d,list) else d;print(d.get("status",""))'
}

wait_deployed() { # name
  for _ in $(seq 1 90); do
    [ "$(status "$1")" = DEPLOYED ] && return 0
    sleep 3
  done
  echo "timed out waiting for $1 to be DEPLOYED" >&2; return 1
}

# Runs a shell command in the sandbox through its process API; prints stdout.
sh_in() { # name, command
  local body
  body="$(python3 -c 'import json,sys;print(json.dumps({"command":sys.argv[1],"waitForCompletion":True}))' "$2")"
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    if out="$(bl run sandbox "$1" --method POST --path /process --data "$body" 2>/dev/null)"; then
      printf '%s' "$out" | python3 -c 'import sys,json;print(json.load(sys.stdin).get("stdout",""),end="")' && return 0
    fi
    sleep 3
  done
  echo "could not run a command in $1" >&2; return 1
}

write_markers() { # name, file
  sh_in "$1" "for d in $MARKER_DIRS; do mkdir -p \$d; echo marker > \$d/$2; done; echo ok" >/dev/null
}

report() { # label, name
  echo "--- $1 ($2)"
  echo "boot_id: $(sh_in "$2" 'cat /proc/sys/kernel/random/boot_id')"
  for f in m1 m2; do
    line="  $f:"
    for d in $MARKER_DIRS; do
      line="$line $d=$(sh_in "$2" "[ -f $d/$f ] && echo ok || echo MISSING")"
    done
    echo "$line"
  done
}

update_network() { # name
  yaml "$1" 1 | bl apply -f - >/dev/null
  sleep 15 # let any redeploy begin; the status can read DEPLOYED before it does
  wait_deployed "$1"
  sleep 5
}

run_chain() { # label, a, b, c, update_b(0|1)
  echo "=== $1 ==="
  yaml "$2" 0 | bl apply -f - >/dev/null; wait_deployed "$2"; write_markers "$2" m1
  bl fork "sbx/$2" "sbx/$3" >/dev/null; wait_deployed "$3"
  if [ "$5" = 1 ]; then update_network "$3"; fi
  write_markers "$3" m2
  report "B ($3) before forking C" "$3"
  bl fork "sbx/$3" "sbx/$4" >/dev/null; wait_deployed "$4"
  report "C ($4) right after fork" "$4"
  update_network "$4"
  report "C ($4) after network update" "$4"
  echo
}
run_chain "CHAIN: fork, update, fork, update" "$A" "$B" "$C" 1
run_chain "CONTROL: fork, (no update), fork, update" "$CA" "$CB" "$CC" 0
