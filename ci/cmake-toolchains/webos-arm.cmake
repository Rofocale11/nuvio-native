# webOS 32-bit ARM cross toolchain (NGD/Buildroot 12.2.0)
set(CMAKE_SYSTEM_NAME Linux)
set(CMAKE_SYSTEM_PROCESSOR arm)
set(CMAKE_SYSROOT "$ENV{WEBOS_SDK}/arm-webos-linux-gnueabi/sysroot")
set(CMAKE_FIND_ROOT_PATH "$ENV{WEBOS_SDK}/arm-webos-linux-gnueabi/sysroot")
set(CMAKE_FIND_ROOT_PATH_MODE_PROGRAM NEVER)
set(CMAKE_FIND_ROOT_PATH_MODE_LIBRARY ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_INCLUDE ONLY)
set(CMAKE_FIND_ROOT_PATH_MODE_PACKAGE ONLY)
set(CMAKE_C_COMPILER "$ENV{WEBOS_SDK}/bin/arm-webos-linux-gnueabi-gcc")
set(CMAKE_CXX_COMPILER "$ENV{WEBOS_SDK}/bin/arm-webos-linux-gnueabi-g++")
set(CMAKE_AR "$ENV{WEBOS_SDK}/bin/arm-webos-linux-gnueabi-ar")
set(CMAKE_RANLIB "$ENV{WEBOS_SDK}/bin/arm-webos-linux-gnueabi-ranlib")
# Soft-float ARMv7 (app is ARMv7 soft-float, matches Makefile CFLAGS). The prefix maps exist for
# the packaging gate: `ci/check-package.py` rejects any `/home/<user>/...` string in the payload,
# and libtorrent/Nuvio Engine embed `__FILE__` in assert messages. The app's own build remaps
# these the same way (CFLAGS_REMAP).
set(CMAKE_C_FLAGS_INIT "-march=armv7-a -mfloat-abi=soft -D_FILE_OFFSET_BITS=64 -ffile-prefix-map=$ENV{REPO_ROOT}=/build -ffile-prefix-map=$ENV{HOME}=/build")
set(CMAKE_CXX_FLAGS_INIT "-march=armv7-a -mfloat-abi=soft -D_FILE_OFFSET_BITS=64 -ffile-prefix-map=$ENV{REPO_ROOT}=/build -ffile-prefix-map=$ENV{HOME}=/build")
