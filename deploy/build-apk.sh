#!/bin/sh
# Containerized 53.apk build (see deploy/Dockerfile.apk).
# Usage: sh deploy/build-apk.sh [absolute-public-server-profile-file]
#        sh deploy/build-apk.sh --client /absolute/public-profile.txt
# Output: ./53.apk in the repository root (debug by default, client release with --client).
#
# Clean allowlist environment: no credentials are passed to docker or the
# build environment. The signing key is a read-only BuildKit secret, excluded
# from the build context; the optional public profile is also secret-mounted.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
: "${DOCKER_HOST=}"

client_apk=false
if [ "${1-}" = "--client" ]; then
    client_apk=true
    shift
    if [ "$#" -ne 1 ]; then
        echo "usage: sh deploy/build-apk.sh --client /absolute/public-profile.txt" >&2
        exit 2
    fi
elif [ "$#" -gt 1 ]; then
    echo "usage: sh deploy/build-apk.sh [absolute-public-server-profile-file]" >&2
    exit 2
fi

if [ ! -f "$root/android/keystore/debug.keystore" ]; then
    echo "android/keystore/debug.keystore missing:" >&2
    echo "  cp ~/.android/debug.keystore android/keystore/debug.keystore" >&2
    exit 1
fi

cd "$root"
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
    --build-arg "CLIENT_APK=$client_apk" \
    --secret "id=debug-keystore,src=$root/android/keystore/debug.keystore" \
    "$@" \
    .
