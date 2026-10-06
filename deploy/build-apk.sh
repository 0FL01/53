#!/bin/sh
# Containerized 53.apk build (see deploy/Dockerfile.apk).
# Usage: sh deploy/build-apk.sh [absolute-public-server-profile-file]
# Output: ./53.apk in the repository root (same artifact as host export53Apk).
#
# Clean allowlist environment: no credentials are passed to docker or the
# build; the only secret-ish input is android/keystore/debug.keystore, COPY'd
# into the image from the local checkout (never printed, never committed).
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
: "${DOCKER_HOST=}"

if [ ! -f "$root/android/keystore/debug.keystore" ]; then
    echo "android/keystore/debug.keystore missing:" >&2
    echo "  cp ~/.android/debug.keystore android/keystore/debug.keystore" >&2
    exit 1
fi

cd "$root"
if [ "$#" -gt 1 ]; then
    echo "usage: sh deploy/build-apk.sh [absolute-public-server-profile-file]" >&2
    exit 2
fi
if [ "$#" -eq 1 ]; then
    case "$1" in /*) ;; *) echo "public profile file must be absolute" >&2; exit 2;; esac
    test -f "$1" || { echo "public profile file missing" >&2; exit 2; }
    # Secret-mount contents alone are not a BuildKit cache key; public digest is.
    digest=$(sha256sum "$1" | cut -d ' ' -f 1)
    set -- --secret "id=server-profile,src=$1" --build-arg "SERVER_PROFILE_SHA256=$digest"
fi
exec env -i HOME="$HOME" PATH="$PATH" DOCKER_HOST="$DOCKER_HOST" \
    docker buildx build \
    -f deploy/Dockerfile.apk \
    --target out \
    -o type=local,dest="$root" \
    "$@" \
    .
