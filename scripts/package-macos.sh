#!/usr/bin/env bash
# Build Wisp Paper for macOS and package it: `dist/Wisp Paper.app` and a DMG.
# The command line travels inside the bundle: "Wisp Paper.app/Contents/MacOS/wisp".
#
#   scripts/package-macos.sh [--arch host|universal|arm64|x64]
#
# --arch host       this machine's architecture (the default)
# --arch universal  Apple Silicon + Intel in one binary (needs both rustup targets)
# --arch arm64      Apple Silicon only (aarch64-apple-darwin)
# --arch x64        Intel only (x86_64-apple-darwin)
#
# Environment:
#   CARGO                        the cargo command, e.g. "cargo +stable" (default: cargo)
#   CARGO_TARGET_DIR             where cargo builds (default: target)
#   WISP_MACOS_SIGNING_IDENTITY  a "Developer ID Application" identity; unset signs ad hoc
#   CODESIGN_KEYCHAIN            the keychain holding that identity (optional)
#   WISP_NOTARY_PROFILE          a `notarytool store-credentials` profile; set, the app and
#                                then the DMG are notarized and each gets its ticket stapled
#
# Without an identity the result runs on this Mac but is not fit to hand out:
# Gatekeeper stops an ad hoc signed download.
set -euo pipefail

arch=host
while (($# > 0)); do
  case "$1" in
    --arch) arch="${2:-}"; shift; (($# > 0)) && shift ;;
    --arch=*) arch="${1#--arch=}"; shift ;;
    -h | --help) sed -n '2,21p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
if [[ "${arch}" != host && "${arch}" != universal && "${arch}" != arm64 && "${arch}" != x64 ]]; then
  echo "--arch must be host, universal, arm64 or x64, got '${arch}'" >&2
  exit 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${root}"

# The version is written once, in the root Cargo.toml.
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)"
if [[ -z "${version}" ]]; then
  echo "no version in Cargo.toml" >&2
  exit 1
fi

read -r -a cargo <<<"${CARGO:-cargo}"
target_dir="${CARGO_TARGET_DIR:-target}"
identity="${WISP_MACOS_SIGNING_IDENTITY:-}"
notary_profile="${WISP_NOTARY_PROFILE:-}"

# GPUI compiles its shaders with Xcode's `metal` at build time. Where the
# Metal Toolchain is not installed, it compiles them when the app starts.
features=()
if ! xcrun metal --version >/dev/null 2>&1; then
  echo "metal compiler not available: building with gpui/runtime_shaders"
  features=(--features gpui/runtime_shaders)
fi

build() { "${cargo[@]}" build --release -p wisp_paper -p wisp_cli ${features[@]+"${features[@]}"} "$@"; }

dist="${root}/dist"
app="${dist}/Wisp Paper.app"
macos_dir="${app}/Contents/MacOS"
rm -rf "${app}"
mkdir -p "${macos_dir}" "${app}/Contents/Resources"

if [[ "${arch}" == universal ]]; then
  label=universal
  for triple in aarch64-apple-darwin x86_64-apple-darwin; do build --target "${triple}"; done
  for binary in wisp_paper wisp; do
    lipo -create -output "${macos_dir}/${binary}" "${target_dir}/aarch64-apple-darwin/release/${binary}" "${target_dir}/x86_64-apple-darwin/release/${binary}"
  done
elif [[ "${arch}" == arm64 || "${arch}" == x64 ]]; then
  label="${arch}"
  if [[ "${arch}" == arm64 ]]; then triple=aarch64-apple-darwin; else triple=x86_64-apple-darwin; fi
  build --target "${triple}"
  for binary in wisp_paper wisp; do cp "${target_dir}/${triple}/release/${binary}" "${macos_dir}/${binary}"; done
else
  case "$(uname -m)" in
    arm64) label=arm64 ;;
    x86_64) label=x64 ;;
    *) label="$(uname -m)" ;;
  esac
  build
  for binary in wisp_paper wisp; do cp "${target_dir}/release/${binary}" "${macos_dir}/${binary}"; done
fi
chmod 755 "${macos_dir}/wisp_paper" "${macos_dir}/wisp"

# The icon: every size of an .icns, scaled from the one PNG.
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT
iconset="${work}/app-icon.iconset"
mkdir -p "${iconset}"
for size in 16 32 128 256 512; do
  sips -z "${size}" "${size}" assets/branding/app-icon.png --out "${iconset}/icon_${size}x${size}.png" >/dev/null
  sips -z "$((size * 2))" "$((size * 2))" assets/branding/app-icon.png --out "${iconset}/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "${iconset}" -o "${app}/Contents/Resources/app-icon.icns"
cp LICENSE THIRD_PARTY.md "${app}/Contents/Resources/"

cat >"${app}/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Wisp Paper</string>
  <key>CFBundleDisplayName</key><string>Wisp Paper</string>
  <key>CFBundleIdentifier</key><string>com.wisp.paper</string>
  <key>CFBundleExecutable</key><string>wisp_paper</string>
  <key>CFBundleIconFile</key><string>app-icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>© 2026 xuzhougeng</string>
</dict>
</plist>
EOF
plutil -lint "${app}/Contents/Info.plist" >/dev/null

# Sign from the inside out: the command line beside the app's own binary, then the bundle.
if [[ -n "${identity}" ]]; then
  sign=(codesign --force --options runtime --timestamp --sign "${identity}")
  if [[ -n "${CODESIGN_KEYCHAIN:-}" ]]; then sign+=(--keychain "${CODESIGN_KEYCHAIN}"); fi
  echo "signing with ${identity}"
else
  sign=(codesign --force --sign -)
  echo "no WISP_MACOS_SIGNING_IDENTITY: signing ad hoc"
fi
"${sign[@]}" "${macos_dir}/wisp"
"${sign[@]}" "${macos_dir}/wisp_paper"
"${sign[@]}" "${app}"
codesign --verify --strict "${app}"

# Apple's verdict on one file: submit it, wait, and fail unless it is accepted.
notarize() {
  echo "notarizing ${1##*/}"
  local result="${work}/notary.json"
  xcrun notarytool submit "$1" --keychain-profile "${notary_profile}" --wait --output-format json >"${result}" || true
  cat "${result}"
  echo
  field() { /usr/bin/python3 -c 'import json, sys; print(json.load(open(sys.argv[1])).get(sys.argv[2], ""))' "${result}" "$1" 2>/dev/null || true; }
  local status id
  status="$(field status)"
  if [[ "${status}" != Accepted ]]; then
    id="$(field id)"
    if [[ -n "${id}" ]]; then xcrun notarytool log "${id}" --keychain-profile "${notary_profile}" || true; fi
    echo "notarization of ${1##*/} ended with status '${status}'" >&2
    exit 1
  fi
}

if [[ -n "${notary_profile}" && -z "${identity}" ]]; then
  echo "WISP_NOTARY_PROFILE needs WISP_MACOS_SIGNING_IDENTITY: Apple does not notarize an ad hoc signature" >&2
  exit 1
fi

# The app is notarized on its own first, so the copy inside the DMG carries
# its ticket and opens without a network once it is dragged to /Applications.
if [[ -n "${notary_profile}" ]]; then
  ditto -c -k --keepParent "${app}" "${work}/app.zip"
  notarize "${work}/app.zip"
  xcrun stapler staple "${app}"
fi

# The DMG: the app next to a link to /Applications.
dmg="${dist}/WispPaper-v${version}-macos-${label}.dmg"
stage="${work}/dmg"
mkdir -p "${stage}"
cp -R "${app}" "${stage}/"
ln -s /Applications "${stage}/Applications"
rm -f "${dmg}"
hdiutil create -volname "Wisp Paper" -srcfolder "${stage}" -ov -format UDZO "${dmg}" >/dev/null
if [[ -n "${identity}" ]]; then
  dmg_sign=(codesign --force --timestamp --sign "${identity}")
  if [[ -n "${CODESIGN_KEYCHAIN:-}" ]]; then dmg_sign+=(--keychain "${CODESIGN_KEYCHAIN}"); fi
  "${dmg_sign[@]}" "${dmg}"
fi
if [[ -n "${notary_profile}" ]]; then
  notarize "${dmg}"
  xcrun stapler staple "${dmg}"
fi

echo "app: ${app}"
echo "dmg: ${dmg} ($(du -h "${dmg}" | cut -f1))"
