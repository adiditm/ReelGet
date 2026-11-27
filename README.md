# reelget

Multiplatform video & reel downloader written in **Rust** (`reelget-core`, `reelget-cli`, `reelget-gui`, `reelget-ffi`).
Supports **public** and **user-authenticated private** videos/reels across **macOS, Windows, Linux, and Android (ARM)**, including **From–To video clipping**.

## Download Prebuilt Binaries
Download ready-to-run binaries for macOS (`.app`), Windows (`.exe`), Linux, and Android (`.apk`) from the **[GitHub Releases](https://github.com/adiditm/ReelGet/releases)** page.

## Supported Platforms & Sources
- **Facebook**: Reels (`/reel/...`), Watch (`/watch/?v=...`), Videos
- **Instagram**: Reels (`/reel/...`), Posts (`/p/...`), TV (`/tv/...`)
- **TikTok**: Videos (`/@user/video/...`), short links (`vm.tiktok.com`)
- **YouTube**: Watch (`/watch?v=...`), Shorts (`/shorts/...`), `youtu.be`
- **X (Twitter)**: Status videos (`x.com/.../status/...`, `twitter.com/...`)
- **Threads**: Video posts (`threads.net` / `threads.com`)
- **Direct & Generic**: Direct `.mp4`/`.webm`/`.mov`, HLS `.m3u8` (including standard `AES-128`), HTML5 `<video>`, OpenGraph `og:video`, JSON-LD `VideoObject`

## Supported Compilation Targets
- **macOS**: `aarch64-apple-darwin` (ARM64), `x86_64-apple-darwin` (Intel)
- **Windows**: `x86_64-pc-windows-msvc` / `x86_64-pc-windows-gnullvm` (Intel), `aarch64-pc-windows-msvc` / `aarch64-pc-windows-gnullvm` (ARM64)
- **Linux**: `x86_64-unknown-linux-gnu` / `x86_64-unknown-linux-musl` (Intel), `aarch64-unknown-linux-gnu` / `aarch64-unknown-linux-musl` (ARM64)
- **Android (ARM only)**: `aarch64-linux-android` (`arm64-v8a`), `armv7-linux-androideabi` (`armeabi-v7a`)

---

## Quick Start (CLI)

### Build & Run GUI (Desktop)
```bash
cargo run --release -p reelget-gui
# Or build macOS .app bundles (ARM64, Intel, and Universal):
./scripts/build-targets.sh
open ./dist/macos-universal/ReelGet.app
```

### Clip Video (From - To) & MP3 Audio Output
```bash
# Clip from 00:00:10 to 00:00:25 (requires ffmpeg for video)
./target/release/reelget download --from 00:00:10 --to 00:00:25 -o ./downloads "https://www.youtube.com/watch?v=K6qj09OHvjw"

# Download as MP3 with bitrate selection (64, 96, 128, 192, 256, 320 kbps)
./target/release/reelget download --mp3 --bitrate 320 -o ./downloads "https://www.youtube.com/watch?v=K6qj09OHvjw"

# Clipped MP3 (works with ffmpeg or pure-Rust symphonia + shine-rs fallback)
./target/release/reelget download --mp3 --bitrate 192 --from 00:00:10 --to 00:00:30 -o ./downloads "https://www.instagram.com/reel/ChxvmGJLfsW/"
```

### Build CLI from source
```bash
cargo build --release -p reelget-cli
./target/release/reelget check-env
```

### Download public videos / reels
```bash
# Single URL
./target/release/reelget download -o ./downloads "https://www.instagram.com/reel/ChxvmGJLfsW/"

# Shorthand without `download` subcommand
./target/release/reelget -o ./downloads "https://x.com/SpaceX/status/1840490493727179198"

# Inspect metadata & formats without downloading
./target/release/reelget info "https://www.tiktok.com/@redbull/video/7460530851797814560"

# Batch download from a file (one URL per line)
./target/release/reelget download -o ./downloads --batch urls.txt
```

### Download private / authenticated videos
Provide your own authorized session cookies via Netscape `cookies.txt` (`--cookies`) or inline `--cookie`:

```bash
# Using Netscape cookies.txt (recommended)
./target/release/reelget download --cookies ./cookies.txt -o ./downloads "https://www.instagram.com/reel/PRIVATE_CODE/"

# Using inline Cookie header
./target/release/reelget download --cookie "c_user=...; xs=..." -o ./downloads "https://www.facebook.com/reel/PRIVATE_ID"

# X (Twitter) protected tweet
./target/release/reelget download --cookie "auth_token=...; ct0=..." "https://x.com/user/status/123456789"
```

> **Note on DRM & Authorization**: `reelget` only accesses private videos using credentials you explicitly supply for accounts you are authorized to view. DRM-protected streams (Widevine, FairPlay, PlayReady, `SAMPLE-AES`) are rejected cleanly.

---

## Multiplatform Compilation

### 1. macOS (ARM64 + Intel)
```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin --workspace
cargo build --release --target x86_64-apple-darwin --workspace
```

### 2. Windows & Linux (via `cargo-zigbuild` or `cross`)
```bash
# Install zig + cargo-zigbuild (or use `cross`)
brew install zig
cargo install --locked cargo-zigbuild
rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu \
                  x86_64-pc-windows-gnullvm aarch64-pc-windows-gnullvm

cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.28 --workspace
cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.28 --workspace
cargo zigbuild --release --target x86_64-pc-windows-gnullvm --workspace
cargo zigbuild --release --target aarch64-pc-windows-gnullvm --workspace
```

### 3. Android (ARM64 & ARMv7 APK + JNI + CLI)
```bash
cargo install --locked cargo-apk cargo-ndk
rustup target add aarch64-linux-android armv7-linux-androideabi

# Build signed Android APK (arm64-v8a & armeabi-v7a)
CARGO_APK_RELEASE_KEYSTORE="$HOME/.android/debug.keystore" \
CARGO_APK_RELEASE_KEYSTORE_PASSWORD="android" \
cargo apk build --release -p reelget-gui --lib

# Build JNI shared library (.so) for Android Kotlin/Java app
cargo ndk -t arm64-v8a -t armeabi-v7a -o ./android-jniLibs build --release -p reelget-ffi

# Or build standalone CLI binary for Android Termux / adb shell
cargo ndk -t arm64-v8a build --release -p reelget-cli
```

See `crates/reelget-ffi/android/ReelGet.kt` for the Kotlin JNI wrapper and `crates/reelget-ffi/include/reelget.h` for the C-ABI header.
Or run `./scripts/build-targets.sh` to build all available targets automatically.
