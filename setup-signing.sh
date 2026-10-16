#!/bin/sh
# One-time setup: create a "Hello Local Signing" certificate in your login keychain.
# Signing Hello with it keeps the same app identity across rebuilds, so macOS
# permissions (Accessibility, Input Monitoring, Microphone) stay granted.
# The private key never leaves your keychain; temporary files are deleted.
set -eu

name="Hello Local Signing"
keychain="$HOME/Library/Keychains/login.keychain-db"

if security find-certificate -c "$name" "$keychain" >/dev/null 2>&1; then
  echo "\"$name\" already exists in your login keychain. Nothing to do."
  exit 0
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cat > "$work/cert.cnf" <<'EOF'
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = Hello Local Signing
O = Local
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
subjectKeyIdentifier = hash
EOF

openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -keyout "$work/key.pem" -out "$work/cert.pem" -config "$work/cert.cnf" 2>/dev/null
pass=$(openssl rand -hex 16)
openssl pkcs12 -export -name "$name" -inkey "$work/key.pem" -in "$work/cert.pem" \
  -out "$work/hello.p12" -passout "pass:$pass"
# -T lets codesign use the key without a keychain prompt on every build.
security import "$work/hello.p12" -k "$keychain" -P "$pass" -T /usr/bin/codesign >/dev/null

echo "Created \"$name\" in your login keychain."
security find-certificate -c "$name" -Z "$keychain" | grep SHA-1
