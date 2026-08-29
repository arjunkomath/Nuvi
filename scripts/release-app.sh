#!/bin/sh
set -eu

log() {
    current_step=$1
    printf '\n==> [%s] %s\n' "$(date '+%H:%M:%S')" "$current_step"
}

on_exit() {
    status=$?
    if [ "$status" -ne 0 ]; then
        printf '\n==> Release failed during "%s" (exit %s). Re-run with NUVI_DEBUG=1 for command tracing.\n' "$current_step" "$status" >&2
    fi
}

current_step=setup
trap on_exit 0
[ "${NUVI_DEBUG:-0}" = 1 ] && set -x

cd "$(dirname "$0")/.."

signing_identity=${NUVI_SIGNING_IDENTITY:-Developer ID Application}
notary_profile=${NUVI_NOTARY_PROFILE:-nuvi-notary}
bundle="target/release/Nuvi.app"
version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml)
archive="target/release/Nuvi-${version}-macos-$(uname -m).zip"

log "Building Nuvi ${version}"
printf '    Signing identity: %s\n    Notary profile: %s\n' "$signing_identity" "$notary_profile"
./scripts/build-app.sh

log "Signing $bundle"
codesign --force --sign "$signing_identity" --options runtime --timestamp "$bundle"

log "Verifying code signature"
codesign --verify --deep --strict --verbose=2 "$bundle"

log "Creating notarization archive"
rm -f "$archive" "$archive.sha256"
ditto -c -k --keepParent "$bundle" "$archive"

log "Submitting to Apple for notarization"
xcrun notarytool submit "$archive" --keychain-profile "$notary_profile" --wait

log "Stapling notarization ticket"
xcrun stapler staple "$bundle"

log "Validating notarized app"
xcrun stapler validate "$bundle"
spctl --assess --type execute --verbose=2 "$bundle"

log "Creating final release archive"
rm -f "$archive"
ditto -c -k --keepParent "$bundle" "$archive"
shasum -a 256 "$archive" > "$archive.sha256"

log "Release ready"
printf '    %s\n    %s\n' "$archive" "$archive.sha256"
