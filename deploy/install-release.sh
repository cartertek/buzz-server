#!/bin/sh
set -eu

mode=install
operation_root=
update_state() {
  state=$1
  error=${2:-}
  python3 - "$operation_root/operation.json" "$state" "$error" <<'PY'
import json, os, sys, tempfile
from datetime import datetime, timezone
from pathlib import Path
p=Path(sys.argv[1]); state,error=sys.argv[2:]; r=json.loads(p.read_text())
if r.get("state") in {"completed","failed"}: raise SystemExit(0)
r["state"]=state; now=datetime.now(timezone.utc).isoformat()
if state == "started": r["started_at"]=now
else: r["completed_at"]=now
if error: r["error"]=error
fd,t=tempfile.mkstemp(prefix="operation.",dir=p.parent)
with os.fdopen(fd,"w") as f: json.dump(r,f,sort_keys=True); f.write("\n"); f.flush(); os.fsync(f.fileno())
os.chmod(t,0o600); os.replace(t,p)
PY
}
case "${1:-}" in
  --stage|--handoff|--run|--status|--recover) mode=${1#--}; shift ;;
esac
if [ "$mode" = run ]; then
  operation_root=${1:-}; [ "$#" -eq 1 ] || { echo "usage: install-release.sh --run OPERATION_DIRECTORY" >&2; exit 64; }
  export BUZZ_DEPLOY_OPERATION_DIR="$operation_root"
  update_state started
  if "$operation_root/buzz-server/deploy/install.sh" --non-interactive; then
    update_state completed
  else
    code=$?; update_state failed "staged installer exited with status $code"; exit "$code"
  fi
  exit 0
fi
if [ "$mode" = status ]; then
  json=false
  [ "${1:-}" = --json ] && { json=true; shift; }
  [ "${2:-}" = --json ] && { json=true; set -- "$1"; }
  [ "$#" -le 1 ] || { echo "usage: install-release.sh --status [OPERATION-ID] [--json]" >&2; exit 64; }
  operations=/var/lib/buzz-server/runtime/deploy/operations
  record=${1:-}
  if [ -n "$record" ]; then record="$operations/$record/operation.json"; else record=$(ls -1t "$operations"/*/operation.json 2>/dev/null | head -1 || true); fi
  [ -f "${record:-}" ] || { echo "deployment operation not found" >&2; exit 66; }
if [ "$json" = true ]; then cat "$record"; else
    python3 - "$record" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
for key in ("operation_id","version","target","state","unit","accepted_at","started_at","completed_at","error"):
    if key in r: print(f"{key}: {r[key]}")
if r.get("state") == "started": print("note: started; unit may still be active or needs recovery")
PY
    state=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("state",""))' "$record")
    unit=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("unit",""))' "$record")
    if [ "$state" = started ]; then
      if systemctl is-active --quiet "$unit" 2>/dev/null; then echo "status: unit active"; else echo "status: stale started; unit inactive or not found"; fi
    fi
  fi
  exit 0
fi
if [ "$mode" = recover ]; then
  operation_id=${1:-}
  operations=/var/lib/buzz-server/runtime/deploy/operations
  record="$operations/$operation_id/operation.json"
  [ -n "$operation_id" ] && [ -f "$record" ] || { echo "deployment operation not found" >&2; exit 66; }
  state=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("state",""))' "$record")
  unit=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("unit",""))' "$record")
  [ "$state" = started ] || { echo "deployment is not recoverable from state: $state" >&2; exit 65; }
  ! systemctl is-active --quiet "$unit" || { echo "deployment unit is already active: $unit" >&2; exit 75; }
  operation_root=/var/lib/buzz-server/runtime/deploy/$operation_id
  [ -x "$operation_root/buzz-server/deploy/install-release.sh" ] || { echo "staged installer is missing; deployment cannot be recovered" >&2; exit 66; }
  systemd-run --unit="$unit" --collect --service-type=oneshot --property=KillMode=control-group --property=TimeoutStartSec=infinity -- "$operation_root/buzz-server/deploy/install-release.sh" --run "$operation_root"
  exit 0
fi
if [ "$mode" = handoff ]; then
  [ "${1:-}" = deploy ] || { echo "usage: install-release.sh --handoff deploy VERSION TARGET [OWNER/REPOSITORY]" >&2; exit 64; }
  shift
fi
stage_only=false
stage_root=
if [ "$mode" = stage ]; then
  stage_only=true
  stage_root=${1:-}
  [ "$#" -ge 1 ] || { echo "usage: install-release.sh --stage OPERATION_DIRECTORY VERSION TARGET [OWNER/REPOSITORY]" >&2; exit 64; }
  shift
fi
if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "usage: install-release.sh VERSION TARGET [OWNER/REPOSITORY]" >&2
  exit 64
fi
version=$1
target=$2
repository=${3:-cartertek/buzz-server}
case "$version" in v[0-9]* ) ;; *) echo "version must be an immutable v* tag" >&2; exit 64;; esac
case "$version" in *[!A-Za-z0-9._-]* ) echo "version contains unsafe characters" >&2; exit 64;; esac
case "$target" in x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu) ;; *) echo "unsupported target" >&2; exit 64;; esac
case "$repository" in *[!A-Za-z0-9._/-]*|*/*/*|'') echo "invalid repository" >&2; exit 64;; esac
if [ "$mode" = install ]; then
  service_group=$(systemctl show buzz-server.service -p ControlGroup --value 2>/dev/null || true)
  if [ -n "$service_group" ] && awk -F: -v group="$service_group" '$3 == group || index($3, group "/") == 1 { found=1 } END { exit found ? 0 : 1 }' /proc/self/cgroup; then
    echo "Refusing to install from inside buzz-server.service; use 'buzz-server deploy' to queue a durable deployment" >&2
    exit 78
  fi
fi
for command in curl tar sha256sum awk mktemp python3 flock; do command -v "$command" >/dev/null 2>&1 || { echo "required command not found: $command" >&2; exit 69; }; done

if [ "$mode" = handoff ]; then
  operations=/var/lib/buzz-server/runtime/deploy/operations
  install -d -o root -g root -m 0700 /var/lib/buzz-server/runtime/deploy "$operations"
  exec 9>"$operations/.lock"
  flock -x 9
  for record in "$operations"/*/operation.json; do
    [ -f "$record" ] || continue
    unit=$(python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print(r.get("unit","") if r.get("state") in {"accepted","started"} else "")' "$record")
    [ -z "$unit" ] || ! systemctl is-active --quiet "$unit" || { echo "another deployment is still active: $unit" >&2; exit 75; }
  done
  operation_id=$(cat /proc/sys/kernel/random/uuid | tr -d - | tr 'A-F' 'a-f')
  operation=/var/lib/buzz-server/runtime/deploy/$operation_id
  operation_root=$operation
  install -d -o root -g root -m 0700 "$operation"
  unit="buzz-server-deploy-${operation_id}.service"
  python3 -c 'import json,os,sys,tempfile; from datetime import datetime,timezone; p=sys.argv[1]; r={"operation_id":sys.argv[2],"version":sys.argv[3],"target":sys.argv[4],"repository":sys.argv[5],"unit":sys.argv[6],"staging_path":os.path.dirname(p)+"/buzz-server","state":"accepted","accepted_at":datetime.now(timezone.utc).isoformat()}; fd,t=tempfile.mkstemp(prefix="operation.",dir=os.path.dirname(p)); f=os.fdopen(fd,"w"); json.dump(r,f,sort_keys=True); f.write("\\n"); f.flush(); os.fsync(f.fileno()); f.close(); os.chmod(t,0o600); os.replace(t,p)' "$operation/operation.json" "$operation_id" "$version" "$target" "$repository" "$unit"
  if ! "$0" --stage "$operation" "$version" "$target" "$repository" >/dev/null; then
    update_state failed "release staging failed"
    echo "release staging failed; operation retained at $operation" >&2
    exit 1
  fi
  if ! systemd-run --unit="$unit" --collect --service-type=oneshot --property=KillMode=control-group --property=TimeoutStartSec=infinity -- "$operation/buzz-server/deploy/install-release.sh" --run "$operation"; then
    update_state failed "systemd-run failed"
    echo "systemd-run failed; operation retained at $operation" >&2
    exit 1
  fi
  echo "accepted deployment $operation_id ($unit)"
  exit 0
fi

asset="buzz-server-${target}.tar.gz"
base="https://github.com/${repository}/releases/download/${version}"
if [ "$stage_only" = true ]; then
  case "$stage_root" in /var/lib/buzz-server/runtime/deploy/*) ;; *) echo "invalid stage root" >&2; exit 64;; esac
  install -d -o root -g root -m 0700 "$stage_root"
  temporary="$stage_root/.download"
  rm -rf "$temporary"
  install -d -o root -g root -m 0700 "$temporary"
else
  temporary=$(mktemp -d)
fi
trap 'rm -rf "$temporary"' EXIT
trap 'exit 130' INT
trap 'exit 143' HUP TERM

echo "==> Downloading Buzz Server $version for $target" >&2
curl --fail --location --connect-timeout 10 --max-time 120 -o "$temporary/$asset" "$base/$asset"
curl --fail --location --connect-timeout 10 --max-time 30 -o "$temporary/$asset.sha256" "$base/$asset.sha256"
(cd "$temporary" && sha256sum -c "$asset.sha256")

package=buzz-server
expected_manifest=$(cat <<EOF
$package/
$package/buzz-server
$package/buzz-server-daemon
$package/buzz-agentctl
$package/buzz-secretsctl
$package/buzz-runtime-probe
$package/buzz-cli
$package/config/
$package/config/buzz-server.dev.example.json
$package/config/buzz-server.schema.json
$package/deploy/
$package/deploy/README.md
$package/deploy/buzz-server.service
$package/deploy/buzz-server-healthcheck.service
$package/deploy/buzz-server-healthcheck.timer
$package/deploy/backup.sh
$package/deploy/healthcheck.sh
$package/deploy/prepare-community-identities.sh
$package/deploy/restore.sh
$package/deploy/buzz-serverctl
$package/deploy/install.sh
$package/deploy/install-package.sh
$package/deploy/migrate-legacy-owner.py
$package/deploy/install-release.sh
$package/deploy/provision-runtimes.sh
EOF
)
actual_manifest=$(tar -tzf "$temporary/$asset" | LC_ALL=C sort)
[ "$actual_manifest" = "$(printf '%s\n' "$expected_manifest" | LC_ALL=C sort)" ] || { echo "archive manifest does not match the release contract" >&2; exit 65; }
tar -tzf "$temporary/$asset" | while IFS= read -r member; do
  case "$member" in "$package"|"$package"/*) ;; *) echo "unsafe archive member: $member" >&2; exit 65;; esac
  case "/$member/" in */../*) echo "unsafe archive traversal" >&2; exit 65;; esac
done
if tar -tvzf "$temporary/$asset" | awk 'substr($1, 1, 1) !~ /^[-d]$/ { found=1 } END { exit found ? 0 : 1 }'; then
  echo "archive must contain only regular files and directories" >&2
  exit 65
fi
if [ "$stage_only" = true ]; then
  tar --no-same-owner --no-same-permissions -C "$stage_root" -xzf "$temporary/$asset"
  chmod 0700 "$stage_root" "$stage_root/$package"
  chown -R root:root "$stage_root/$package"
  printf '%s\n' "$stage_root/$package"
else
  tar --no-same-owner --no-same-permissions -C "$temporary" -xzf "$temporary/$asset"
  "$temporary/$package/deploy/install-package.sh" "$version" "$target" "$temporary/$package"
fi
