package com.reelget

/**
 * Kotlin JNI wrapper for `libreelget_ffi.so` (compiled for `aarch64-linux-android` / `armv7-linux-androideabi`).
 */
object ReelGet {
    init {
        System.loadLibrary("reelget_ffi")
    }

    interface ProgressListener {
        fun onProgress(eventJson: String)
    }

    external fun nativeVersion(): String

    external fun nativeGetInfo(
        url: String,
        cookiesFile: String? = null,
        rawCookie: String? = null
    ): String

    external fun nativeDownload(
        url: String,
        outputDir: String,
        quality: String = "best",
        cookiesFile: String? = null,
        rawCookie: String? = null,
        listener: ProgressListener? = null
    ): String
}
