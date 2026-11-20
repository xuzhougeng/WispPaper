#!/bin/sh
# Put the release build into a folder and pack it:
#
#     scripts/package-linux.sh <target triple> <version> <label>
#
# The package is a plain tar.gz: unpack it anywhere and run `wisp-study`.
set -eu
target=$1
version=$2
label=$3
release="target/$target/release"
name="WispStudy-v$version-$label"

rm -rf "dist/$name"
mkdir -p "dist/$name"
cp "$release/wisp_study" "dist/$name/wisp-study"
cp "$release/wisp" "dist/$name/wisp"
cp assets/branding/app-icon.png "dist/$name/wisp-study.png"
cp LICENSE THIRD_PARTY.md README.md "dist/$name/"

# For a launcher entry: copy it to ~/.local/share/applications and point Exec and Icon at this folder.
cat > "dist/$name/wisp-study.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Wisp Study
Comment=本地优先的文献库
Exec=wisp-study
Icon=wisp-study
Categories=Office;Education;Science;
StartupWMClass=com.wisp.study
DESKTOP

tar -C dist -czf "dist/$name.tar.gz" "$name"
echo "dist/$name.tar.gz"
