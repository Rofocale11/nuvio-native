#!/usr/bin/env bash
# Build the pinned OpenSSL 1.1.1 for the webOS ARM target, STATICALLY, for libsbengine.so.
#
# Why this exists: the C torrent engine is linked against OpenSSL for HTTPS trackers and web
# seeds. The NDK sysroot's shared libssl.so.1.1 exists only on webOS 7.4/8.3 (9.0+ ships
# libssl.so.3, older releases 1.0.x), so a dynamically linked engine loads on almost no
# television — including current ones. Building a static copy makes the dependency disappear
# from the engine's DT_NEEDED, so the same .so loads on every webOS this app ships to.
#
# The NDK sysroot's own static libcrypto.a is not usable: it is built with brotli/zstd objects
# this sysroot does not ship. A private build with `no-zlib` has no such inputs.
#
# Version, URL and sha256 are declared once, in ci/dependencies.json (id: openssl).
# Out: vendor/openssl-prefix/{include,lib/libssl.a,lib/libcrypto.a}
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DEPS="$ROOT/ci/deps.py"
VERSION=$(python3 "$DEPS" get openssl version)
ARCHIVE="$(python3 "$DEPS" cache-dir)/$(python3 "$DEPS" get openssl archive)"
SOURCE="$ROOT/vendor/openssl-src"
PREFIX="$ROOT/vendor/openssl-prefix"

WEBOS_SDK=${WEBOS_SDK:-"$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot"}
SYSROOT="$WEBOS_SDK/arm-webos-linux-gnueabi/sysroot"
CROSS="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-"

fail() { echo "build-openssl: $*" >&2; exit 1; }
test -x "${CROSS}gcc" || fail "webOS NDK not found at $WEBOS_SDK (run make setup-env)"
command -v perl >/dev/null 2>&1 || fail "perl is required by the OpenSSL Configure script"

# ci/deps.py downloads (canonical URL, then mirrors) and verifies the pinned sha256; a cached
# archive is re-verified rather than trusted.
python3 "$DEPS" fetch openssl >/dev/null

# A failed or interrupted rebuild must not leave an old OpenSSL looking current.
rm -rf "$SOURCE" "$PREFIX"
mkdir -p "$SOURCE" "$PREFIX"
tar xzf "$ARCHIVE" --strip-components=1 -C "$SOURCE"

export CROSS_COMPILE="$CROSS"
# OpenSSL's Makefile expands `$(CROSS_COMPILE)$(CC)`: keep CC/AR/RANLIB bare names, or the
# prefix is applied twice and the build fails with a doubled path.
export CC=gcc
export AR=ar
export RANLIB=ranlib
# -fPIC is mandatory: these objects end up inside libsbengine.so. Soft-float matches the app
# (ARMv7 soft-float EABI) and the engine's own compile flags.
export CFLAGS="-march=armv7-a -mfloat-abi=soft -fPIC -Os -ffile-prefix-map=$ROOT=/build -ffile-prefix-map=$HOME=/build"
export LDFLAGS=""

cd "$SOURCE"
# `no-async` keeps the build off ucontext on this ancient glibc; `no-zlib` removes the one
# library the sysroot cannot supply for a static crypto; `no-engine` drops dynamic engine
# loading, which the TV has no use for. `no-tests`/`no-docs` keep the build and the artifact
# to what the engine links.
./Configure linux-armv4 \
  no-shared no-tests no-async no-zlib no-engine \
  --prefix="$PREFIX" --openssldir=/etc/ssl \
  -march=armv7-a -mfloat-abi=soft -fPIC -Os
make -j"$(nproc 2>/dev/null || echo 4)" build_libs
make install_sw

for lib in libssl.a libcrypto.a; do
  test -f "$PREFIX/lib/$lib" || fail "$lib was not installed"
done
ls -lh "$PREFIX/lib/libssl.a" "$PREFIX/lib/libcrypto.a"
echo "build-openssl: $VERSION static libs in $PREFIX"
