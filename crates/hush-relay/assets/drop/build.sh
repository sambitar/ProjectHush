#!/usr/bin/env bash
# Bundle the drop page's JS and inline it into page.built.html.
#
# The built file is committed: `include_str!` in the relay needs no node, and a
# Rust build stays self-contained. Run this after editing main.js or page.html,
# and commit both the source and the output.
#
#   npm install            # once, in crates/hush-relay/ (noble + esbuild)
#   assets/drop/build.sh
set -euo pipefail
cd "$(dirname "$0")"

ESBUILD=../../node_modules/.bin/esbuild

"$ESBUILD" main.js \
  --bundle --minify --format=iife --platform=browser \
  --outfile=page.tmp.js

node -e '
const fs = require("node:fs");
const js = fs.readFileSync("page.tmp.js", "utf8");
if (js.includes("</script")) {
  throw new Error("bundle contains </script>; esbuild should have escaped it");
}
const html = fs.readFileSync("page.html", "utf8");
const marker = "<!--__DROP_JS__-->";
if (!html.includes(marker)) {
  throw new Error("page.html lost its <!--__DROP_JS__--> marker");
}
fs.writeFileSync(
  "page.built.html",
  html.replace(marker, () => "<script>" + js + "</script>"),
);
fs.unlinkSync("page.tmp.js");
'

echo "built $(pwd)/page.built.html"
