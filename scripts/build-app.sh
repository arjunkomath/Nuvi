#!/bin/sh
set -eu

cargo build --release

bundle="target/release/Nuvi.app"
resources="$bundle/Contents/Resources"
version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml)
build=$(git rev-list --count HEAD)
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$resources"
cp target/release/Nuvi "$bundle/Contents/MacOS/Nuvi"
cp packaging/Info.plist "$bundle/Contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$version" "$bundle/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$build" "$bundle/Contents/Info.plist"
xcrun actool --compile "$resources" \
    --platform macosx \
    --minimum-deployment-target 11.0 \
    --app-icon Nuvi \
    --output-partial-info-plist target/release/Nuvi-icon-info.plist \
    --output-format human-readable-text \
    packaging/icon/Nuvi.icon

echo "$bundle"
