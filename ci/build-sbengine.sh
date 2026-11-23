#!/bin/sh
# Build libsbengine.so for webOS 32-bit ARM (NDK, no zig).
# Nuvio Engine + libtorrent 2.0.12 + Boost 1.86 + a PRIVATELY BUILT STATIC OpenSSL 1.1.1,
# exposing the single `sbe_get` entrypoint.
#
# Usage: ci/build-sbengine.sh [extras...]
#   WEBOS_SDK override respected. Out: build/sbengine/libsbengine.so
set -eu
REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export REPO_ROOT
WEBOS_SDK="${WEBOS_SDK:-$HOME/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot}"
export WEBOS_SDK
SYSROOT="$WEBOS_SDK/arm-webos-linux-gnueabi/sysroot"
GEN="$WEBOS_SDK/bin/arm-webos-linux-gnueabi-g++"
if [ ! -x "$GEN" ]; then echo "NDK not found at $WEBOS_SDK" >&2; exit 1; fi
mkdir -p "$REPO_ROOT/build/sbengine/build"

# OpenSSL 1.1.1 static: the engine links it in, so the .so has no `libssl.so.1.1` DT_NEEDED and
# loads on every webOS (the firmware ships 1.0.x, 1.1 or 3 depending on release). Built by
# ci/build-openssl.sh; `make` has no target for it yet, so build it on demand here.
OPENSSL_PREFIX="$REPO_ROOT/vendor/openssl-prefix"
if [ ! -f "$OPENSSL_PREFIX/lib/libssl.a" ] || [ ! -f "$OPENSSL_PREFIX/lib/libcrypto.a" ]; then
  echo "=== OpenSSL static prefix missing; building it ==="
  "$REPO_ROOT/ci/build-openssl.sh"
fi


# Staged super-project: nuvio-engine + sbengine.cpp wrapper
WORK="$REPO_ROOT/build/sbengine/src"
rm -rf "$WORK"
mkdir -p "$WORK"
cp -R "$REPO_ROOT/vendor/engine-sources/nuvio-engine" "$WORK/nuvio-engine"
rm -rf "$WORK/nuvio-engine/.git" "$WORK/nuvio-engine/build"

# Wrapper CMake: build nuvio_engine STATIC then wrap into libsbengine.so
cat > "$WORK/CMakeLists.txt" <<'EOF'
cmake_minimum_required(VERSION 3.24)
project(sbengine_wrapper CXX)
add_subdirectory(nuvio-engine)
add_library(sbengine SHARED sbengine.cpp)
target_compile_features(sbengine PRIVATE cxx_std_20)
target_include_directories(sbengine PRIVATE
  "${CMAKE_CURRENT_SOURCE_DIR}/nuvio-engine/include"
  "${CMAKE_CURRENT_SOURCE_DIR}"
)
target_link_libraries(sbengine PRIVATE nuvio_engine)
set_target_properties(sbengine PROPERTIES
  OUTPUT_NAME "sbengine"
  CXX_VISIBILITY_PRESET hidden
  VISIBILITY_INLINES_HIDDEN ON)
# Hide everything; only sbe_get is exported. libstdc++/libgcc and OpenSSL are linked STATICALLY:
# the TV's libstdc++ lacks three symbols the NDK's emits (exception_ptr::_M_release/_M_addref,
# __throw_bad_array_new_length) and its libssl is 1.0.x/1.1/3 depending on the release, so a
# dynamically linked .so loads on almost no television. The static build is what makes one
# artifact portable across every webOS the app ships to.
target_link_options(sbengine PRIVATE "LINKER:--exclude-libs,ALL" -static-libstdc++ -static-libgcc)
EOF
mkdir -p "$WORK"
cp "$REPO_ROOT/vendor/engine-sources/sbengine.cpp" "$WORK/"
mkdir -p "$WORK/core"
cp "$REPO_ROOT/vendor/engine-sources/core/sbengine.h" "$WORK/core/"

echo "=== cmake configure (webOS ARM) ==="
# A stale cache keeps whatever FindOpenSSL decided last time (once: the HOST's pkg-config
# OpenSSL, whose static link flags carry -lbrotlienc/-lzstd this sysroot does not ship), so a
# configure here always starts clean. PKG_CONFIG is disabled for the same reason: on a desktop
# it reads /usr/lib/pkgconfig, not the sysroot.
rm -rf "$REPO_ROOT/build/sbengine/build"
# FetchContent's clones and tarballs go OUTSIDE `build/`: this configure wipes that directory on
# every run, and in CI `build/sbengine/deps` is the one part of the engine build a cache can
# carry between runs (Boost 1.86 is a 140 MB tarball and libtorrent a full clone). Locally it
# simply avoids re-downloading on rebuild.
cmake -S "$WORK" -B "$REPO_ROOT/build/sbengine/build" -G "Unix Makefiles" \
  -DCMAKE_DISABLE_FIND_PACKAGE_PkgConfig=TRUE \
  -DFETCHCONTENT_BASE_DIR="$REPO_ROOT/build/sbengine/deps" \
  -DCMAKE_TOOLCHAIN_FILE="$REPO_ROOT/ci/cmake-toolchains/webos-arm.cmake" \
  -DOPENSSL_USE_STATIC_LIBS=TRUE \
  -DOPENSSL_SSL_LIBRARY="$OPENSSL_PREFIX/lib/libssl.a" \
  -DOPENSSL_CRYPTO_LIBRARY="$OPENSSL_PREFIX/lib/libcrypto.a" \
  -DOPENSSL_INCLUDE_DIR="$OPENSSL_PREFIX/include" \
  -DCMAKE_BUILD_TYPE=Release \
  -DNUVIO_ENGINE_ENABLE_LIBTORRENT=ON \
  -DNUVIO_ENGINE_BUILD_SHARED=OFF \
  -DNUVIO_ENGINE_BUILD_TESTS=OFF \
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON

echo "=== build ==="
cmake --build "$REPO_ROOT/build/sbengine/build" -j"$(nproc)"

SO="$REPO_ROOT/build/sbengine/build/libsbengine.so"
[ -f "$SO" ] || SO="$(find "$REPO_ROOT/build/sbengine/build" -name 'libsbengine.so' | head -1)"
echo "=== result ==="
ls -lh "$SO"
"$WEBOS_SDK/bin/arm-webos-linux-gnueabi-readelf" -d "$SO" | head -n 30
"$WEBOS_SDK/bin/arm-webos-linux-gnueabi-readelf" -h "$SO" | head -n 20
"$WEBOS_SDK/bin/arm-webos-linux-gnueabi-nm" -D "$SO" 2>/dev/null | head -20 || true
# Canonical path: the STRIPPED copy (7.8 MB -> 5.1 MB; the untouched build tree keeps its
# symbols for a crash PC). `make deploy` ships this one.
mkdir -p "$REPO_ROOT/build/sbengine"
"$WEBOS_SDK/bin/arm-webos-linux-gnueabi-strip" --strip-unneeded "$SO" \
  -o "$REPO_ROOT/build/sbengine/libsbengine.so"
ls -lh "$REPO_ROOT/build/sbengine/libsbengine.so"
file "$REPO_ROOT/build/sbengine/libsbengine.so" 2>/dev/null || true
