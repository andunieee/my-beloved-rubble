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
        cat > "$apps/computer.mybelovedrubble.desktop" <<EOF
    [Desktop Entry]
    Type=Application
    Name=My Beloved Rubble
    Comment=Content-addressed folders with rclone remotes
    Exec=$bin/my-beloved-rubble
    Icon=folder
    Terminal=false
    Categories=Utility;FileTools;
    EOF
        update-desktop-database "$apps" 2>/dev/null || true
    fi
    echo "installed $bin/mbr and $bin/my-beloved-rubble"

# build the arm64 apk and install it on every android device connected through adb
android-install:
    #!/usr/bin/env bash
    set -euo pipefail
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
    devices=$(adb devices | awk 'NR > 1 && $2 == "device" { print $1 }')
    if [ -z "$devices" ]; then
        echo "no android device connected through adb" >&2
        exit 1
    fi
    android/build.sh build
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
