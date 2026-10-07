#!/bin/sh
# Creates a self-signed code-signing identity, "Dictum Local Signing", in the login keychain.
#
# macOS ties the Microphone, Accessibility and Input Monitoring permissions to the app's signing
# identity. With an ad-hoc signature that identity changes on every build, so the permissions
# stop applying; signed with this certificate they stay granted across rebuilds.
# scripts/macos-bundle.sh uses it automatically when it exists.
#
# Remove it with: security delete-identity -c "Dictum Local Signing"
set -eu

name="Dictum Local Signing"
if security find-certificate -c "$name" >/dev/null 2>&1; then
    echo "\"$name\" already exists"
    exit 0
fi

dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
cat > "$dir/cert.conf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $name
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
EOF
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -config "$dir/cert.conf" \
    -keyout "$dir/key.pem" -out "$dir/cert.pem" 2>/dev/null
openssl pkcs12 -export -inkey "$dir/key.pem" -in "$dir/cert.pem" -name "$name" \
    -out "$dir/identity.p12" -passout pass:dictum
security import "$dir/identity.p12" -k "$HOME/Library/Keychains/login.keychain-db" \
    -P dictum -T /usr/bin/codesign
echo "created \"$name\""
