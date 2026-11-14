#!/bin/sh
# Puts the page together: site/ plus the screenshots the documents already use
# and the app icon, so neither is kept twice. Usage: site/build.sh [folder]
# (default dist/site). The folder can then be served as it is:
#   python3 -m http.server --directory dist/site
#
# The download buttons read release.json, which the Pages workflow writes next
# to the page. Without it they lead to the release page.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-"$root/dist/site"}

mkdir -p "$out/img"
cp "$root/site/index.html" "$root/site/style.css" "$root/site/site.js" "$out/"
cp "$root/assets/branding/app-icon.svg" "$root/assets/branding/app-icon.png" "$out/"
cp "$root/docs/images/library.png" "$root"/docs/images/tutorial/*.jpg "$out/img/"
# No Jekyll: the files are served as they are.
: > "$out/.nojekyll"
echo "site: $out"
