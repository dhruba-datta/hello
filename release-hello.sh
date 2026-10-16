#!/bin/sh
# Make a Hello release: a signed Hello.app inside Hello.dmg, its update signature,
# and the appcast.xml that tells installed copies about it. Nothing is uploaded.
# Usage: ./release-hello.sh <output-dir>
#   SPARKLE_KEY    Ed25519 private key (PEM) that signs updates (required)
#   SIGN_IDENTITY  code-signing identity, passed on to package-hello.sh
#   RELEASE_NOTES  optional text file, one change per line, shown in the update window
#   RELEASES_REPO  GitHub repository that hosts the .dmg (default dhruba-datta/hello-releases)
#   OPENSSL        OpenSSL 3 binary (default: Homebrew's openssl@3, then openssl)
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out_dir=${1:?usage: release-hello.sh <output-dir>}
key=${SPARKLE_KEY:?set SPARKLE_KEY to the update signing key}
repo=${RELEASES_REPO:-dhruba-datta/hello-releases}
openssl=${OPENSSL:-}
if [ -z "$openssl" ]; then
  if [ -x /opt/homebrew/opt/openssl@3/bin/openssl ]; then
    openssl=/opt/homebrew/opt/openssl@3/bin/openssl
  else
    openssl=openssl
  fi
fi
"$openssl" version | grep -q '^OpenSSL 3' \
  || { echo "Ed25519 signing needs OpenSSL 3; set OPENSSL" >&2; exit 1; }
[ -r "$key" ] || { echo "cannot read $key" >&2; exit 1; }

plist="$root/app/Info.plist"
version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$plist")
build=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$plist")
minimum=$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$plist")
public_key=$(/usr/libexec/PlistBuddy -c 'Print :SUPublicEDKey' "$plist")
# A key that doesn't match Info.plist would ship updates nobody can install.
[ "$("$openssl" pkey -in "$key" -pubout -outform DER | tail -c 32 | base64)" = "$public_key" ] \
  || { echo "SPARKLE_KEY does not match SUPublicEDKey in Info.plist" >&2; exit 1; }

mkdir -p "$out_dir"
(cd "$root" && cargo build --release)
"$root/package-hello.sh" "$out_dir" >/dev/null

# The disk image opens to Hello and an Applications shortcut to drag it onto.
staging="$out_dir/dmg"
dmg="$out_dir/Hello.dmg"
rm -rf "$staging" "$dmg"
mkdir -p "$staging"
/usr/bin/ditto "$out_dir/Hello.app" "$staging/Hello.app"
ln -s /Applications "$staging/Applications"
hdiutil create -quiet -volname "Hello" -srcfolder "$staging" -fs HFS+ -format ULFO "$dmg"
rm -rf "$staging"
identity=$(codesign -dvv "$out_dir/Hello.app" 2>&1 | sed -n 's/^Authority=//p' | head -1)
[ -n "$identity" ] && codesign --force --sign "$identity" "$dmg"

length=$(stat -f %z "$dmg")
signature=$("$openssl" pkeyutl -sign -rawin -inkey "$key" -in "$dmg" | base64)

notes=""
if [ -n "${RELEASE_NOTES:-}" ]; then
  notes=$(sed -e '/^[[:space:]]*$/d' -e 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g' \
    -e 's|^[-*][[:space:]]*||; s|^|<li>|; s|$|</li>|' "$RELEASE_NOTES")
fi

cat > "$out_dir/appcast.xml" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
  <channel>
    <title>Hello</title>
    <link>https://hello.minutecampus.com/appcast.xml</link>
    <language>en</language>
    <item>
      <title>Hello $version</title>
      <pubDate>$(LC_ALL=C date -u '+%a, %d %b %Y %H:%M:%S +0000')</pubDate>
      <sparkle:version>$build</sparkle:version>
      <sparkle:shortVersionString>$version</sparkle:shortVersionString>
      <sparkle:minimumSystemVersion>$minimum</sparkle:minimumSystemVersion>
      <description><![CDATA[<h2>Hello $version</h2><ul>$notes</ul>]]></description>
      <enclosure url="https://github.com/$repo/releases/download/v$version/Hello.dmg"
        length="$length" type="application/octet-stream"
        sparkle:edSignature="$signature" />
    </item>
  </channel>
</rss>
EOF

cat >&2 <<EOF
Hello $version (build $build) is ready in $out_dir:
  Hello.dmg     $length bytes
  appcast.xml   copy it to the website, then publish the site
Publish the .dmg first, so the appcast never points at a missing file:
  gh release create v$version "$dmg" --repo $repo --title "Hello $version"
EOF
