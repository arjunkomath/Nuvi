#!/bin/sh
set -eu

cargo build --release

bundle="target/release/Nuvi.app"
mkdir -p "$bundle/Contents/MacOS"
cp target/release/Nuvi "$bundle/Contents/MacOS/Nuvi"
cp packaging/Info.plist "$bundle/Contents/Info.plist"

echo "$bundle"
