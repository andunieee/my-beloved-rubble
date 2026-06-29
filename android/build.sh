#!/bin/sh
# Build the Android app (an APK for arm64 phones), and install it on the
# connected device with `android/build.sh run`.
#
# Needs the Android SDK and NDK (ANDROID_HOME, ANDROID_NDK_ROOT), the
# aarch64-linux-android Rust target, cargo-apk and Go. rclone is built by
# librclone-sys with plain `go build`, which takes its target from the
# GOOS/GOARCH/CC set here.
set -eu
cd "$(dirname "$0")"

: "${ANDROID_HOME:=/opt/android-sdk}"
: "${ANDROID_NDK_ROOT:=$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)}"
export ANDROID_HOME ANDROID_NDK_ROOT
API=26
TOOLCHAIN="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin"

export GOOS=android GOARCH=arm64 CGO_ENABLED=1
export CC="$TOOLCHAIN/aarch64-linux-android$API-clang"
# Go builds rclone as a shared library on Android: go-shim/go turns
# librclone-sys's request for a static one into that, here.
REAL_GO=$(command -v go)
export REAL_GO
export RCLONE_LIB_DIR="$PWD/runtime-libs/arm64-v8a"
export AR_FOR_RCLONE="$TOOLCHAIN/llvm-ar"
export PATH="$PWD/go-shim:$PATH"
# Only Go and the Android target may use the NDK's compiler.
export HOST_CC=cc
# bindgen reads librclone's header for the Android target.
export BINDGEN_EXTRA_CLANG_ARGS="--sysroot=$TOOLCHAIN/../sysroot"

cargo apk "${1:-build}" --release --lib
