# list recipes
default:
    @just --list

# run the GUI
gui *args:
    cargo run --bin my-beloved-rubble -- {{args}}

# run the CLI
cli *args:
    cargo run --bin mbr -- {{args}}

# install on this computer and on every android device connected through adb
install: desktop-install android-install

# install mbr and my-beloved-rubble for this user, into ~/.cargo/bin (and the application menu on linux)
desktop-install:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo install --path . --locked --bin mbr --bin my-beloved-rubble
    bin="${CARGO_HOME:-$HOME/.cargo}/bin"
    if [ "$(uname -s)" = Linux ]; then
        apps="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
        mkdir -p "$apps"
        icons="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"
        mkdir -p "$icons"
        cp assets/icon.svg "$icons/computer.mybelovedrubble.svg"
        cat > "$apps/computer.mybelovedrubble.desktop" <<EOF
    [Desktop Entry]
    Type=Application
    Name=My Beloved Rubble
    Comment=Content-addressed folders with rclone remotes
    Exec=$bin/my-beloved-rubble
    Icon=computer.mybelovedrubble
    Terminal=false
    Categories=Utility;FileTools;
    EOF
        update-desktop-database "$apps" 2>/dev/null || true
    fi
    echo "installed $bin/mbr and $bin/my-beloved-rubble"

# build the arm64 apk, or run it on the connected device with `just android-build run`
android-build action="build":
    #!/usr/bin/env bash
    set -euo pipefail
    cd android

    : "${ANDROID_HOME:=/opt/android-sdk}"
    : "${ANDROID_NDK_ROOT:=$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)}"
    export ANDROID_HOME ANDROID_NDK_ROOT
    api=26
    toolchain="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin"

    export GOOS=android GOARCH=arm64 CGO_ENABLED=1
    export CC="$toolchain/aarch64-linux-android$api-clang"
    # Go builds rclone as a shared library on Android: go-shim/go turns
    # librclone-sys's request for a static one into that, here.
    REAL_GO=$(command -v go)
    export REAL_GO
    export RCLONE_LIB_DIR="$PWD/runtime-libs/arm64-v8a"
    export AR_FOR_RCLONE="$toolchain/llvm-ar"
    export PATH="$PWD/go-shim:$PATH"
    # Only Go and the Android target may use the NDK's compiler.
    export HOST_CC=cc
    # bindgen reads librclone's header for the Android target.
    export BINDGEN_EXTRA_CLANG_ARGS="--sysroot=$toolchain/../sysroot"

    # signed with CARGO_APK_RELEASE_KEYSTORE if set, otherwise with ~/.android/debug.keystore,
    # which can't upgrade an install of the github release apk (uninstall that first)
    if [ -z "${CARGO_APK_RELEASE_KEYSTORE:-}" ]; then
        export CARGO_APK_RELEASE_KEYSTORE="$HOME/.android/debug.keystore"
        export CARGO_APK_RELEASE_KEYSTORE_PASSWORD=android
        if [ ! -f "$CARGO_APK_RELEASE_KEYSTORE" ]; then
            mkdir -p "$HOME/.android"
            keytool -genkeypair -keystore "$CARGO_APK_RELEASE_KEYSTORE" \
                -storepass android -alias androiddebugkey -keypass android \
                -keyalg RSA -keysize 2048 -validity 10000 -dname "CN=Android Debug,O=Android,C=US"
        fi
    fi

    cargo apk build --release --lib

    # cargo-apk can neither ship Java nor declare a content provider: add
    # java/'s provider (which lends files to other apps for "Open with"
    # and "Share"), its Inbox activity (the target for files shared with
    # the app, and the file picker) and their manifest entries, then
    # repackage and re-sign.
    build_tools=$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -1)
    jar="$ANDROID_HOME/platforms/android-34/android.jar"
    apk_dir="$(cargo metadata --no-deps --format-version 1 | jq -r .target_directory)/release/apk"
    classes=$(mktemp -d)
    trap 'rm -r "$classes"' EXIT
    javac -d "$classes" -classpath "$jar" --release 11 java/computer/mybelovedrubble/*.java
    "$build_tools/d8" --release --min-api 26 --lib "$jar" --output "$apk_dir" $(find "$classes" -name '*.class')
    entries='<provider android:name="computer.mybelovedrubble.Files" android:authorities="computer.mybelovedrubble.files" android:exported="false" android:grantUriPermissions="true"/>'
    share='<category android:name="android.intent.category.DEFAULT"/><data android:mimeType="*/*"/>'
    entries+='<activity android:name="computer.mybelovedrubble.Inbox" android:exported="true" android:theme="@android:style/Theme.Translucent.NoTitleBar" android:configChanges="orientation|screenSize|screenLayout|keyboardHidden">'
    entries+="<intent-filter><action android:name=\"android.intent.action.SEND\"/>$share</intent-filter>"
    entries+="<intent-filter><action android:name=\"android.intent.action.SEND_MULTIPLE\"/>$share</intent-filter>"
    entries+='</activity>'
    sed -i "s#</application>#$entries</application>#" "$apk_dir/AndroidManifest.xml"
    res="$PWD/res"
    (
        cd "$apk_dir"
        libs=$("$build_tools/aapt" list my-beloved-rubble-unaligned.apk | grep '^lib/')
        "$build_tools/aapt" package -f -F my-beloved-rubble-unaligned.apk -M AndroidManifest.xml -I "$jar" -S "$res"
        "$build_tools/aapt" add my-beloved-rubble-unaligned.apk classes.dex $libs
        "$build_tools/zipalign" -f 4 my-beloved-rubble-unaligned.apk my-beloved-rubble.apk
        "$build_tools/apksigner" sign --ks "$CARGO_APK_RELEASE_KEYSTORE" \
            --ks-pass "pass:$CARGO_APK_RELEASE_KEYSTORE_PASSWORD" my-beloved-rubble.apk
    )

    if [ "{{action}}" = run ]; then
        adb install -r "$apk_dir/my-beloved-rubble.apk"
        adb shell am start -n computer.mybelovedrubble/android.app.NativeActivity
        adb logcat -s mbr
    fi

# build the arm64 apk and install it on every android device connected through adb
android-install: android-build
    #!/usr/bin/env bash
    set -euo pipefail
    devices=$(adb devices | awk 'NR > 1 && $2 == "device" { print $1 }')
    if [ -z "$devices" ]; then
        echo "no android device connected through adb" >&2
        exit 1
    fi
    target=$(cd android && cargo metadata --no-deps --format-version 1 | jq -r .target_directory)
    apk="$target/release/apk/my-beloved-rubble.apk"
    for device in $devices; do
        echo "installing on $device"
        adb -s "$device" install -r "$apk"
    done

# render the screenshots for the readme and the user guide into docs/static/screenshots
screenshots:
    cargo run --release --features screenshots --bin screenshots

# serve the user guide on http://127.0.0.1:1111, rebuilding on changes
docs:
    zola --root docs serve

# tag the version in Cargo.toml and push the tag, which publishes a github release
release:
    #!/usr/bin/env bash
    set -euo pipefail
    tag="v$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n1)"
    git tag "$tag"
    git push origin "$tag"
    echo "pushed $tag"

# render assets/icon.svg into the png and ico files every platform takes
icons:
    #!/usr/bin/env bash
    set -euo pipefail
    resvg -w 512 assets/icon.svg assets/icon.png
    tmp=$(mktemp -d)
    for size in 16 24 32 48 64 128 256; do
        resvg -w "$size" assets/icon.svg "$tmp/$size.png"
    done
    magick "$tmp"/{16,24,32,48,64,128,256}.png assets/icon.ico
    rm -r "$tmp"
    for density in mdpi:48 hdpi:72 xhdpi:96 xxhdpi:144 xxxhdpi:192; do
        dir="android/res/mipmap-${density%:*}"
        mkdir -p "$dir"
        resvg -w "${density#*:}" assets/icon.svg "$dir/ic_launcher.png"
    done
    cp assets/icon.svg docs/static/icon.svg
