#!/bin/sh
# Package Hello.app: the compiled program, Hello's Info.plist, icon, license and
# custom-commands kit, plus the runtime frameworks (ONNX Runtime, Moonshine and
# Sparkle) taken from an existing Hello.app.
# Usage: ./package-hello.sh <output-dir>
#   SOURCE_APP     app to take the frameworks from (default /Applications/Hello.app)
#   SIGN_IDENTITY  code-signing identity (default "Hello Local Signing", else ad-hoc)
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
SOURCE_APP=${SOURCE_APP:-/Applications/Hello.app}
out_dir=${1:?usage: package-hello.sh <output-dir>}
bundle="$out_dir/Hello.app"
# Honours CARGO_TARGET_DIR and any .cargo/config.toml target-dir.
target_dir=$(cd "$root" && cargo metadata --format-version 1 --no-deps \
  | /usr/bin/python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')
binary="$target_dir/release/voice-control"

[ -x "$binary" ] || { echo "missing $binary; run cargo build --release first" >&2; exit 1; }
frameworks_source="$SOURCE_APP/Contents/Frameworks"
[ -f "$frameworks_source/libonnxruntime.1.23.2.dylib" ] \
  || { echo "$SOURCE_APP does not carry the runtime frameworks; set SOURCE_APP" >&2; exit 1; }
command -v node >/dev/null || { echo "Node.js is required to build the commands kit" >&2; exit 1; }

rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
/usr/bin/ditto "$frameworks_source" "$bundle/Contents/Frameworks"
cp "$binary" "$bundle/Contents/MacOS/Hello"
cp "$root/app/Info.plist" "$bundle/Contents/Info.plist"
printf 'APPL????' > "$bundle/Contents/PkgInfo"
cp "$root/branding/AppIcon.icns" "$bundle/Contents/Resources/AppIcon.icns"
cp "$root/LICENSE" "$root/THIRD_PARTY_NOTICES.md" "$bundle/Contents/Resources/"

kit="$bundle/Contents/Resources/commands-sdk"
mkdir -p "$kit"
cp "$root/sdk/commands/package.json" "$kit/"
/usr/bin/ditto "$root/sdk/commands/src" "$kit/src"
/usr/bin/ditto "$root/sdk/commands/workspace-template" "$kit/workspace-template"
node --no-warnings "$root/sdk/commands/build.mjs" "$kit/dist"

# Sign inside-out with a stable identity so macOS permissions survive rebuilds.
# setup-signing.sh creates "Hello Local Signing"; ad-hoc signing works too, but
# then permissions must be granted again after every rebuild.
identity=${SIGN_IDENTITY:-}
if [ -z "$identity" ] && security find-certificate -c "Hello Local Signing" >/dev/null 2>&1; then
  identity="Hello Local Signing"
fi
identity=${identity:--}
echo "signing with: $identity" >&2
frameworks="$bundle/Contents/Frameworks"
sparkle="$frameworks/Sparkle.framework/Versions/B"
for item in \
  "$sparkle/XPCServices/Installer.xpc" \
  "$sparkle/XPCServices/Downloader.xpc" \
  "$sparkle/Autoupdate" \
  "$sparkle/Updater.app" \
  "$frameworks/Sparkle.framework" \
  "$frameworks/libonnxruntime.1.23.2.dylib" \
  "$frameworks/libmoonshine.dylib"; do
  [ -e "$item" ] && codesign --force --sign "$identity" --preserve-metadata=entitlements "$item"
done
codesign --force --sign "$identity" --entitlements "$root/app/VoiceControl.entitlements" "$bundle"
codesign --verify --deep --strict "$bundle"
echo "$bundle"
