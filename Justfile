version := `sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml`
arch := `uname -m`
bundle := "target/release/Nuvi.app"
archive := "target/release/Nuvi-" + version + "-macos-" + arch + ".zip"

# List available recipes.
default:
    @just --list

# Compile the debug binary.
build:
    cargo build

# Build the release binary and assemble a local macOS app bundle.
app:
    ./scripts/build-app.sh

# List code-signing identities available in the current Keychain.
signing-identities:
    security find-identity -v -p codesigning

# Store Apple notarization credentials in the nuvi-notary Keychain profile.
notary-profile:
    xcrun notarytool store-credentials nuvi-notary

# Build, sign, notarize, staple, verify, and package a release.
release:
    ./scripts/release-app.sh

# Run the release flow with every shell command printed.
release-debug:
    NUVI_DEBUG=1 ./scripts/release-app.sh

# Recheck the release checksum, signature, notarization ticket, and Gatekeeper status.
verify-release:
    shasum -a 256 -c "{{archive}}.sha256"
    codesign --verify --deep --strict --verbose=2 "{{bundle}}"
    xcrun stapler validate "{{bundle}}"
    spctl --assess --type execute --verbose=2 "{{bundle}}"
