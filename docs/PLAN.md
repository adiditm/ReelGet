# Rencana Arsitektur & Implementasi: `reelget` (Multiplatform Rust Video & Reel Downloader)

## 1. Ringkasan Tujuan
Membangun aplikasi downloader video dan reel berbasis **Rust** yang:
- Mendukung video **public** maupun **private** (melalui kredensial/cookies resmi milik pengguna yang berhak mengakses konten tersebut, tanpa bypass DRM atau eksploitasi autentikasi).
- Mendukung platform target utama:
  - **Facebook** (Reels, Watch, Videos)
  - **Instagram** (Reels, Posts, TV, Stories)
  - **TikTok** (Videos, short links)
  - **YouTube** (Watch, Shorts, youtu.be)
  - **X / Twitter** (Status videos, protected tweets dengan cookies)
  - **Threads** (`threads.net` / `threads.com` posts)
  - **Direct Media & Generic HTML5** (`.mp4`, `.webm`, `.mov`, HLS `.m3u8`, OpenGraph `og:video`, JSON-LD `VideoObject`, `<video>`)
- Dapat dikompilasi multiplatform:
  - **macOS**: Intel (`x86_64-apple-darwin`) & ARM64 (`aarch64-apple-darwin`)
  - **Windows**: Intel (`x86_64-pc-windows-msvc` / `x86_64-pc-windows-gnullvm`) & ARM64 (`aarch64-pc-windows-msvc` / `aarch64-pc-windows-gnullvm`)
  - **Linux**: Intel (`x86_64-unknown-linux-gnu` / `x86_64-unknown-linux-musl`) & ARM64 (`aarch64-unknown-linux-gnu` / `aarch64-unknown-linux-musl`)
  - **Android (ARM only, tanpa Intel)**: ARM64 (`aarch64-linux-android`) & ARMv7 (`armv7-linux-androideabi`)

---

## 2. Arsitektur Workspace Rust

```
Downloader/
├── Cargo.toml                  # Workspace manifest & shared dependencies
├── Cross.toml                  # Konfigurasi cross-compilation (Linux/Windows/Android)
├── scripts/build-targets.sh    # Script build multiplatform lokal
├── .github/workflows/release.yml # CI/CD matrix build semua OS & arsitektur
└── crates/
    ├── reelget-core/           # Engine async pure-Rust (TLS rustls + webpki-roots)
    ├── reelget-cli/            # Binary CLI `reelget` (progress bar, batch, JSON)
    ├── reelget-gui/            # Aplikasi GUI pure-Rust (`eframe`/`egui`) untuk Desktop & Android
    └── reelget-ffi/            # Shared/static library C-ABI + Android JNI (`libreelget_ffi.so`)
```

### Mengapa Pure-Rust TLS (`rustls` + `webpki-roots`)?
- Menghindari ketergantungan pada `OpenSSL` atau `native-tls` yang sering gagal saat cross-compile ke Windows, Linux `musl`, dan Android.
- `webpki-roots` menyematkan root CA Mozilla langsung di binary sehingga HTTPS bekerja identik di macOS, Windows, Linux, maupun Android tanpa konfigurasi sertifikat OS tambahan.

---

## 3. Desain Dukungan Video Public & Private

### Prinsip Keamanan & Kepatuhan
- **User-Authorized Access Only**: Video private/terbatas hanya dapat diunduh jika pengguna menyertakan sesi autentikasi miliknya sendiri melalui:
  1. `--cookies <cookies.txt>` (format standar Netscape HTTP Cookie File, mendukung `#HttpOnly_`).
  2. `--cookie "key=val; ..."` (inline Cookie header).
  3. `-H "Key: Value"` (custom header seperti `Authorization`, `X-CSRFToken`).
- **Tanpa Bypass DRM**: Stream HLS yang menggunakan DRM (`SAMPLE-AES`, Widevine, FairPlay, PlayReady) dideteksi secara eksplisit dan mengembalikan error `ReelGetError::DrmProtected`.
- **Tanpa Pencurian Kredensial**: Aplikasi tidak membaca database cookie browser secara diam-diam.

### Mekanisme Autentikasi per Platform
| Platform | Public Extraction | Private / Authenticated Extraction |
|---|---|---|
| **Facebook** | SSR HTML + Relay JSON (`id == target_id` -> `browser_native_hd_url` / `sd_url`), fallback `/video.php?v=` & `/reel/` untuk menghindari redirect loop `/watch/` | Cookies `c_user` + `xs` pada `cookies.txt` / `--cookie` |
| **Instagram** | SSR HTML Relay JSON (`code == shortcode` -> `video_versions`) + regex/OG fallback | Cookies `sessionid` + `csrftoken` + fallback API `/api/v1/media/{media_id}/info/` |
| **Threads** | Redirect `threads.net` -> `threads.com`, Relay JSON (`code == post_code` -> `video_versions`) | Cookies `sessionid` (Threads/Instagram) |
| **TikTok** | Mobile Safari UA (menghindari `SlardarWAF`), `<script id="api-data">` / `__UNIVERSAL_DATA_FOR_REHYDRATION__`, session cookie jar + `Referer` | Cookies `sessionid` / `sid_tt` |
| **X / Twitter** | Syndication API (`cdn.syndication.twimg.com/tweet-result`) -> MP4 variants + HLS | Cookies `auth_token` + `ct0` -> X GraphQL `TweetResultByRestId` |
| **YouTube** | InnerTube `ANDROID` / `IOS` client API -> progressive MP4 (`itag 18`) & adaptive streams | Cookies `SAPISID` / `__Secure-3PAPISID` -> header `Authorization: SAPISIDHASH` |

---

## 4. Pipeline Download & Muxing
1. **Direct Progressive MP4/WebM**:
   - Streaming chunked ke file `.part` lalu atomic rename.
   - Mendukung HTTP `Range` resume (`--resume` default aktif).
2. **HLS (`.m3u8`)**:
   - Parser master playlist (`#EXT-X-STREAM-INF`) & media playlist (`#EXTINF`, `#EXT-X-MAP`, `#EXT-X-KEY`).
   - Dekripsi standar `AES-128` CBC pure-Rust (`aes` + `cbc`).
   - Download segmen paralel (`FuturesOrdered` + `Semaphore`) menjaga urutan segmen.
   - Remux `.ts` -> `.mp4` otomatis jika `ffmpeg` tersedia; tetap menghasilkan `.mp4` (fMP4) atau `.ts` valid jika `ffmpeg` tidak ada (mis. di Android).
3. **Adaptive Video + Audio (YouTube)**:
   - Jika `ffmpeg` tersedia, mencoba download adaptive video + audio lalu remux `-c copy`.
   - Jika CDN adaptive mengembalikan `403` (mis. tanpa PO Token), otomatis fallback ke format progressive `VideoAndAudio` (`itag 18`) agar download tetap berhasil di semua platform termasuk Android.

---

## 5. Matriks Target Kompilasi

| OS | Arsitektur | Target Triple | Output |
|---|---|---|---|
| macOS | ARM64 (Apple Silicon) | `aarch64-apple-darwin` | `reelget`, `libreelget_ffi.dylib` |
| macOS | Intel (x86_64) | `x86_64-apple-darwin` | `reelget`, `libreelget_ffi.dylib` |
| Windows | Intel (x86_64) | `x86_64-pc-windows-msvc` / `gnullvm` | `reelget.exe`, `reelget_ffi.dll` |
| Windows | ARM64 | `aarch64-pc-windows-msvc` / `gnullvm` | `reelget.exe`, `reelget_ffi.dll` |
| Linux | Intel (x86_64) | `x86_64-unknown-linux-gnu` / `musl` | `reelget`, `libreelget_ffi.so` |
| Linux | ARM64 | `aarch64-unknown-linux-gnu` / `musl` | `reelget`, `libreelget_ffi.so` |
| Android | ARM64 (`arm64-v8a`) | `aarch64-linux-android` | `reelget` (CLI), `libreelget_ffi.so` (JNI) |
| Android | ARMv7 (`armeabi-v7a`) | `armv7-linux-androideabi` | `reelget` (CLI), `libreelget_ffi.so` (JNI) |

*(Target Intel Android `x86_64-linux-android` / `i686-linux-android` sengaja tidak disertakan sesuai spesifikasi.)*
