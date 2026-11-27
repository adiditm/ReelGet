#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

DIST_DIR="$ROOT_DIR/dist"
mkdir -p "$DIST_DIR"

create_macos_app() {
  local app_dir="$1"
  local gui_bin="$2"
  local cli_bin="$3"

  rm -rf "$app_dir"
  mkdir -p "$app_dir/Contents/MacOS" "$app_dir/Contents/Resources"
  cp "$gui_bin" "$app_dir/Contents/MacOS/reelget-gui"
  cp "$cli_bin" "$app_dir/Contents/MacOS/reelget"
  chmod +x "$app_dir/Contents/MacOS/reelget-gui" "$app_dir/Contents/MacOS/reelget"
  cp "crates/reelget-gui/assets/AppIcon.icns" "$app_dir/Contents/Resources/AppIcon.icns"
  printf 'APPL????' > "$app_dir/Contents/PkgInfo"

  cat << 'PLIST' > "$app_dir/Contents/Info.plist"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>ReelGet</string>
  <key>CFBundleDisplayName</key>
  <string>ReelGet</string>
  <key>CFBundleIdentifier</key>
  <string>com.reelget.app</string>
  <key>CFBundleVersion</key>
  <string>0.1.0</string>
  <key>CFBundleShortVersionString</key>
  <string>0.1.0</string>
  <key>CFBundleExecutable</key>
  <string>reelget-gui</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>LSMinimumSystemVersion</key>
  <string>11.0</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

  if command -v codesign >/dev/null 2>&1; then
    codesign --force --deep --sign - "$app_dir" >/dev/null 2>&1 || true
  fi
}

echo "==> Building macOS .app bundles (ARM64 + Intel + Universal)"
if [[ "$(uname -s)" == "Darwin" ]]; then
  built_arm=0
  built_x64=0

  for target in aarch64-apple-darwin x86_64-apple-darwin; do
    if rustup target list --installed | grep -qx "$target"; then
      echo "  -> $target"
      cargo build --release --target "$target" --workspace
      rm -rf "$DIST_DIR/$target"
      mkdir -p "$DIST_DIR/$target"
      create_macos_app \
        "$DIST_DIR/$target/ReelGet.app" \
        "target/$target/release/reelget-gui" \
        "target/$target/release/reelget"
      cp "target/$target/release/reelget" "$DIST_DIR/$target/"
      cp "target/$target/release/libreelget_ffi.dylib" "$DIST_DIR/$target/"
      if [[ "$target" == "aarch64-apple-darwin" ]]; then built_arm=1; fi
      if [[ "$target" == "x86_64-apple-darwin" ]]; then built_x64=1; fi
    else
      echo "  -> skipping $target (run: rustup target add $target)"
    fi
  done

  if [[ "$built_arm" -eq 1 && "$built_x64" -eq 1 ]] && command -v lipo >/dev/null 2>&1; then
    echo "  -> macos-universal (ARM64 + Intel ReelGet.app)"
    UNI_DIR="$DIST_DIR/macos-universal"
    rm -rf "$UNI_DIR"
    mkdir -p "$UNI_DIR"
    lipo -create \
      "target/aarch64-apple-darwin/release/reelget-gui" \
      "target/x86_64-apple-darwin/release/reelget-gui" \
      -output "$UNI_DIR/reelget-gui-universal"
    lipo -create \
      "target/aarch64-apple-darwin/release/reelget" \
      "target/x86_64-apple-darwin/release/reelget" \
      -output "$UNI_DIR/reelget"
    lipo -create \
      "target/aarch64-apple-darwin/release/libreelget_ffi.dylib" \
      "target/x86_64-apple-darwin/release/libreelget_ffi.dylib" \
      -output "$UNI_DIR/libreelget_ffi.dylib"
    create_macos_app \
      "$UNI_DIR/ReelGet.app" \
      "$UNI_DIR/reelget-gui-universal" \
      "$UNI_DIR/reelget"
    rm -f "$UNI_DIR/reelget-gui-universal"
  fi
fi

echo "==> Building Linux & Windows targets (requires cargo-zigbuild + zig)"
if command -v cargo-zigbuild >/dev/null 2>&1 && command -v zig >/dev/null 2>&1; then
  for target in \
    x86_64-unknown-linux-gnu \
    aarch64-unknown-linux-gnu \
    x86_64-pc-windows-gnullvm \
    aarch64-pc-windows-gnullvm; do
    if rustup target list --installed | grep -qx "$target"; then
      echo "  -> $target"
      zig_target="$target"
      if [[ "$target" == *"linux-gnu" ]]; then
        zig_target="${target}.2.28"
      fi
      cargo zigbuild --release --target "$zig_target" --workspace
      rm -rf "$DIST_DIR/$target"
      mkdir -p "$DIST_DIR/$target"
      if [[ "$target" == *"windows"* ]]; then
        cp "target/$target/release/reelget.exe" "$DIST_DIR/$target/"
        cp "target/$target/release/reelget-gui.exe" "$DIST_DIR/$target/"
        cp "target/$target/release/reelget_ffi.dll" "$DIST_DIR/$target/"
      else
        cp "target/$target/release/reelget" "$DIST_DIR/$target/"
        cp "target/$target/release/reelget-gui" "$DIST_DIR/$target/"
        cp "target/$target/release/libreelget_ffi.so" "$DIST_DIR/$target/"
        cp "crates/reelget-gui/assets/icon.png" "$DIST_DIR/$target/com.reelget.app.png"
        cp "crates/reelget-gui/linux/com.reelget.app.desktop" "$DIST_DIR/$target/com.reelget.app.desktop"
        cp "crates/reelget-gui/linux/install-desktop.sh" "$DIST_DIR/$target/install-desktop.sh"
        chmod +x "$DIST_DIR/$target/install-desktop.sh"
      fi
    else
      echo "  -> skipping $target (run: rustup target add $target)"
    fi
  done
else
  echo "  -> cargo-zigbuild or zig not installed; skipping Linux/Windows cross-builds"
fi

echo "==> Building Android ARM targets (APK + JNI + CLI)"
if [[ -z "${JAVA_HOME:-}" && -d "/Applications/Android Studio.app/Contents/jbr/Contents/Home" ]]; then
  export JAVA_HOME="/Applications/Android Studio.app/Contents/jbr/Contents/Home"
  export PATH="$JAVA_HOME/bin:$PATH"
fi
if [[ -z "${ANDROID_HOME:-}" && -d "$HOME/Library/Android/sdk" ]]; then
  export ANDROID_HOME="$HOME/Library/Android/sdk"
fi
if [[ -z "${ANDROID_NDK_HOME:-}" && -n "${ANDROID_HOME:-}" && -d "$ANDROID_HOME/ndk" ]]; then
  latest_ndk="$(ls -1 "$ANDROID_HOME/ndk" | sort -V | tail -n 1 || true)"
  if [[ -n "$latest_ndk" ]]; then
    export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/$latest_ndk"
  fi
fi

if [[ -n "${ANDROID_HOME:-}" && -n "${ANDROID_NDK_HOME:-}" ]]; then
  if command -v cargo-apk >/dev/null 2>&1; then
    echo "  -> Building Android APK (arm64-v8a + armeabi-v7a)"
    if [[ -z "${CARGO_APK_RELEASE_KEYSTORE:-}" && -f "$HOME/.android/debug.keystore" ]]; then
      export CARGO_APK_RELEASE_KEYSTORE="$HOME/.android/debug.keystore"
      export CARGO_APK_RELEASE_KEYSTORE_PASSWORD="android"
    fi
    cargo apk build --release -p reelget-gui --lib
    rm -rf "$DIST_DIR/android"
    mkdir -p "$DIST_DIR/android"
    cp "target/release/apk/ReelGet.apk" "$DIST_DIR/android/ReelGet.apk"
  else
    echo "  -> cargo-apk not installed; skipping APK build"
  fi

  if command -v cargo-ndk >/dev/null 2>&1; then
    echo "  -> Building Android JNI libs & ARM64 CLI"
    rm -rf "$DIST_DIR/android-jniLibs" "$DIST_DIR/aarch64-linux-android"
    mkdir -p "$DIST_DIR/android-jniLibs" "$DIST_DIR/aarch64-linux-android"
    cargo ndk -t arm64-v8a -t armeabi-v7a -o "$DIST_DIR/android-jniLibs" build --release -p reelget-ffi
    cargo ndk -t arm64-v8a build --release -p reelget-cli
    cp "target/aarch64-linux-android/release/reelget" "$DIST_DIR/aarch64-linux-android/"
  else
    echo "  -> cargo-ndk not installed; skipping JNI/CLI Android build"
  fi
else
  echo "  -> ANDROID_HOME or ANDROID_NDK_HOME not found; skipping Android builds"
fi

echo "==> Done. Artifacts in $DIST_DIR"
