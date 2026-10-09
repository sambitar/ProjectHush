#!/usr/bin/env bash
# Local mock for the development Drop.
#
# The "Example SecureDrop" entry in the Drop registry points at
# http://127.0.0.1:8791 so the Drops rail can be exercised end to end without
# Tor and without touching a real organisation. This serves a static page there.
#
# Usage:
#   ./scripts/drops-dev-mock.sh            # serve on 127.0.0.1:8791
#   PORT=9000 ./scripts/drops-dev-mock.sh  # serve elsewhere (also update the
#                                          # registry's DEV_MOCK_URL)
#
# It binds to loopback only. Nothing about it is anonymous or private; it is a
# test fixture and the UI labels it as one.
set -euo pipefail

PORT="${PORT:-8791}"
DIR="$(mktemp -d "${TMPDIR:-/tmp}/hush-drops-mock.XXXXXX")"
trap 'rm -rf "$DIR"' EXIT

cat >"$DIR/index.html" <<'HTML'
<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    <title>Example SecureDrop (development mock)</title>
    <style>
      body { font: 15px/1.5 system-ui, sans-serif; margin: 0; background: #121218; color: #f2f2f7; }
      main { max-width: 640px; margin: 0 auto; padding: 48px 24px; }
      .tag { display: inline-block; padding: 4px 10px; border-radius: 999px; background: #3d3018; color: #f0d48a; font-size: 12px; font-weight: 600; }
      h1 { margin: 16px 0 8px; }
      code { background: #262632; padding: 2px 6px; border-radius: 6px; }
      a { color: #b3a9ff; }
    </style>
  </head>
  <body>
    <main>
      <span class="tag">Development mock</span>
      <h1>Example SecureDrop</h1>
      <p>
        This is a harmless local page, not a real SecureDrop and not an onion
        service. It exists so the Drops rail can be tested without Tor.
      </p>
      <p>
        The Hush shell opened this window through its dedicated Drop webview.
        In a real Drop the same window would be pointed at a verified
        <code>.onion</code> address and routed through Tor.
      </p>
      <p>Nothing you type here is sent anywhere.</p>
    </main>
  </body>
</html>
HTML

echo "Serving the development Drop mock at http://127.0.0.1:${PORT}/"
echo "Enable it in Hush with HUSH_DROPS_DEV=1 and click Open Drop on the test card."
exec python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$DIR"
