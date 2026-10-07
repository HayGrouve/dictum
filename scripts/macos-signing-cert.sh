#!/bin/sh
# Creates the self-signed code-signing identity "Dictum Signing" and imports it into the login
# keychain, so scripts/macos-bundle.sh signs local builds with it.
#   scripts/macos-signing-cert.sh            create it on this Mac
#   scripts/macos-signing-cert.sh --github   also store it as the repository's Actions secrets
#                                            MACOS_SIGNING_P12 and MACOS_SIGNING_PASSWORD (needs gh)
#
# macOS ties the Microphone, Accessibility and Input Monitoring permissions to the signing
# identity. Releases and local builds signed with the same identity keep those permissions across
# updates and rebuilds. Run it once: a new identity means granting the permissions again.
#
# Remove it from this Mac with: security delete-identity -c "Dictum Signing"
set -eu

cd "$(dirname "$0")/.."
name="Dictum Signing"
github=false
[ "${1:-}" = "--github" ] && github=true

if security find-certificate -c "$name" >/dev/null 2>&1; then
    echo "\"$name\" already exists in a keychain; not creating another" >&2
    exit 1
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
password=$(openssl rand -base64 24)
openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -config "$dir/cert.conf" \
    -keyout "$dir/key.pem" -out "$dir/cert.pem" 2>/dev/null
openssl pkcs12 -export -inkey "$dir/key.pem" -in "$dir/cert.pem" -name "$name" \
    -out "$dir/identity.p12" -passout "pass:$password"

if $github; then
    base64 < "$dir/identity.p12" | gh secret set MACOS_SIGNING_P12
    printf '%s' "$password" | gh secret set MACOS_SIGNING_PASSWORD
    echo "stored the identity as Actions secrets MACOS_SIGNING_P12 and MACOS_SIGNING_PASSWORD"
fi

security import "$dir/identity.p12" -k "$HOME/Library/Keychains/login.keychain-db" \
    -P "$password" -T /usr/bin/codesign
echo "created \"$name\""
