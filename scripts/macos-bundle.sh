#!/bin/sh
# Builds Dictum.app (menu bar only) in target/release.
#   scripts/macos-bundle.sh            build the bundle
#   scripts/macos-bundle.sh --zip      also pack it as target/release/dictum-macos-arm64.zip
#   scripts/macos-bundle.sh --install  also copy it to /Applications (replacing an older copy)
#
# macOS remembers the Microphone, Accessibility and Input Monitoring permissions per signing
# identity. The bundle is signed with "Dictum Signing" when that is in a keychain (releases are,
# see scripts/macos-signing-cert.sh), so permissions survive rebuilds and updates. Otherwise it
# is signed ad hoc, which changes with every build: then remove Dictum from those lists in
# System Settings and allow it again after installing a new build.
set -eu

zip=false
install=false
for arg in "$@"; do
    case "$arg" in
        --zip) zip=true ;;
        --install) install=true ;;
        *) echo "unknown option $arg" >&2; exit 2 ;;
    esac
done

cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
identifier=com.github.haygrouve.dictum

cargo build --release -p dictum

app=target/release/Dictum.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp target/release/dictum "$app/Contents/MacOS/dictum"
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>$identifier</string>
    <key>CFBundleName</key>
    <string>Dictum</string>
    <key>CFBundleDisplayName</key>
    <string>Dictum</string>
    <key>CFBundleExecutable</key>
    <string>dictum</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>Dictum turns your speech into text, entirely on this Mac.</string>
</dict>
</plist>
EOF
signing=-
if security find-certificate -c "Dictum Signing" >/dev/null 2>&1; then
    signing="Dictum Signing"
fi
codesign --force --sign "$signing" --identifier "$identifier" "$app"
if [ "$signing" = - ]; then
    echo "built $app (signed ad hoc)"
else
    echo "built $app (signed by $signing)"
fi

if $zip; then
    # Only the bundle itself: no resource forks or extended attributes the updater would drop.
    rm -f target/release/dictum-macos-arm64.zip
    ditto -c -k --norsrc --noextattr --noacl --keepParent "$app" target/release/dictum-macos-arm64.zip
    echo "packed target/release/dictum-macos-arm64.zip"
fi

if $install; then
    pkill -x dictum 2>/dev/null && sleep 1 || true
    rm -rf /Applications/Dictum.app
    cp -R "$app" /Applications/Dictum.app
    echo "installed /Applications/Dictum.app"
fi
