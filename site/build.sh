#!/bin/sh
# Puts the site together: site/ (the front page and the tutorials) plus the
# screenshots under docs/images and the app icon, so neither is kept twice.
# Usage: site/build.sh [folder] (default dist/site). The folder can then be
# served as it is:
#   python3 -m http.server --directory dist/site
#
# The download buttons read release.json, which the Pages workflow writes next
# to the page. Without it they lead to the release page.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-"$root/dist/site"}

mkdir -p "$out/img/t" "$out/tutorials"
cp "$root/site/index.html" "$root/site/style.css" "$root/site/tutorial.css" "$root/site/site.js" "$out/"
cp "$root/assets/branding/app-icon.svg" "$root/assets/branding/app-icon.png" "$out/"
cp "$root/docs/images/library.png" "$root"/docs/images/tutorial/*.jpg "$out/img/"
# The tutorials: one page each, and their screenshots under img/t/. A file whose
# name starts with "_" (the skeleton a new tutorial is copied from) stays behind.
for page in "$root"/site/tutorials/*.html; do
  case $(basename "$page") in _*) ;; *) cp "$page" "$out/tutorials/" ;; esac
done
for shot in "$root"/docs/images/tutorials/*.jpg; do
  [ -e "$shot" ] && cp "$shot" "$out/img/t/"
done
# No Jekyll: the files are served as they are.
: > "$out/.nojekyll"
echo "site: $out"
