#!/bin/sh
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME to an installed Android NDK r28+}"
# cargo-ndk's panic reporter can print its environment: pass no credentials.
exec env -i HOME="$HOME" PATH="$PATH" ANDROID_NDK_HOME="$ANDROID_NDK_HOME" \
    CARGO_PROFILE_RELEASE_LTO=thin CARGO_PROFILE_RELEASE_STRIP=symbols \
    RUSTFLAGS='-C link-arg=-Wl,-z,max-page-size=16384' \
    cargo ndk -t arm64-v8a -P 26 -o "$root/android/app/build/nativeLibs" \
    build --manifest-path "$root/Cargo.toml" --release -p dmsg-core
