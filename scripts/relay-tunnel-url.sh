#!/usr/bin/env bash
# Print the HTTPS URL of the lab relay's Cloudflare tunnel, and prove it serves
# the relay.
#
# The relay's HTTPS origin is a quick tunnel on the lab VPS
# (`cloudflared-hush-quick.service`, pointed straight at `127.0.0.1:8790`).
# Cloudflare mints the hostname when the connector starts and reclaims it without
# warning — three hostnames on this box have gone NXDOMAIN while the connector
# still reported `active` — so the URL is read from the running connector each
# time instead of being remembered anywhere. See docs/ops/relay-lab-vps.md.
#
#   ./scripts/relay-tunnel-url.sh              # report the current URL and verify it
#   ./scripts/relay-tunnel-url.sh --refresh    # restart the tunnel, then report
#
# A restart mints a new hostname and kills the old one, and a vault stores the
# relay it was created against, so a vault made against a URL that has since died
# has to be repointed: `hush-cli --dir <vault> relay set --url <new url>`.
#
# Lab-only: this needs SSH to the box. Override with HUSH_LAB_HOST, HUSH_LAB_KEY
# and HUSH_LAB_TUNNEL_UNIT.
set -euo pipefail

HOST="${HUSH_LAB_HOST:-root@137.184.129.205}"
KEY="${HUSH_LAB_KEY:-$HOME/.ssh/alhakim_deploy}"
UNIT="${HUSH_LAB_TUNNEL_UNIT:-cloudflared-hush-quick.service}"

REFRESH=0
case "${1:-}" in
  --refresh) REFRESH=1 ;;
  -h|--help) sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
  "") ;;
  *) echo "Unknown argument: $1 (options: --refresh)" >&2; exit 2 ;;
esac

command -v ssh >/dev/null || { echo "ssh is not installed." >&2; exit 1; }
command -v curl >/dev/null || { echo "curl is not installed." >&2; exit 1; }
[[ -f "$KEY" ]] || { echo "No SSH key at $KEY (set HUSH_LAB_KEY)." >&2; exit 1; }

SSH=(ssh -o BatchMode=yes -o ConnectTimeout=10 -i "$KEY" "$HOST")

# The hostname is read from the connector that is running now, by pid: a name
# from an earlier process in the same journal is exactly the stale value this
# script exists to avoid handing out.
connector_pid() {
  "${SSH[@]}" "systemctl show -p MainPID --value $UNIT" 2>/dev/null | tr -d '\r'
}

hostname_of() { # <pid>
  "${SSH[@]}" "journalctl _PID=$1 -u $UNIT --no-pager -o cat 2>/dev/null \
    | grep -oE 'https://[a-z0-9-]+\.trycloudflare\.com' | tail -1" 2>/dev/null || true
}

if [[ "$REFRESH" -eq 1 ]]; then
  echo "Restarting $UNIT (mints a new hostname; the old one stops serving)…"
  "${SSH[@]}" "systemctl restart $UNIT" || { echo "Could not restart $UNIT on $HOST." >&2; exit 1; }
fi

URL=""
for _ in $(seq 1 20); do
  PID="$(connector_pid)"
  if [[ -n "$PID" && "$PID" != "0" ]]; then
    URL="$(hostname_of "$PID")"
    [[ -n "$URL" ]] && break
  fi
  sleep 2
done
[[ -n "$URL" ]] || { echo "No tunnel hostname in $UNIT's log on $HOST." >&2; exit 1; }

# The hostname in the log is a claim; a request against it is the evidence.
CODE=000
for _ in $(seq 1 12); do
  CODE="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 10 "$URL/health" 2>/dev/null || echo 000)"
  [[ "$CODE" == "200" ]] && break
  sleep 3
done

if [[ "$CODE" != "200" ]]; then
  echo "relay: $URL" >&2
  echo "  $URL/health -> $CODE (the connector is up but this hostname does not serve)" >&2
  echo "  Re-mint it: $0 --refresh" >&2
  exit 1
fi

echo "relay: $URL  (/health 200)"
echo
echo "Hand this to the other side, and use it here:"
echo "  ./scripts/run-one-ui.sh --relay $URL"
echo
echo "A release daemon accepts this URL; the plain-HTTP lab relay it replaces needs a debug build."
