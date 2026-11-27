use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};

use reelget_core::auth::AuthConfig;
use reelget_core::downloader::ProgressCallback;
use reelget_core::models::{
    ClipRange, DownloadOptions, Mp3Bitrate, OutputFormat, QualityPreference,
};
use reelget_core::ReelGetClient;
use serde::Serialize;
use tokio::runtime::Runtime;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to initialize Tokio runtime")
    })
}

#[derive(Serialize)]
struct FfiResponse<T: Serialize> {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn ok_json<T: Serialize>(data: T) -> String {
    serde_json::to_string(&FfiResponse {
        ok: true,
        data: Some(data),
        error: None,
    })
    .unwrap_or_else(|e| err_json(&e.to_string()))
}

fn err_json(msg: &str) -> String {
    serde_json::to_string(&FfiResponse::<()> {
        ok: false,
        data: None,
        error: Some(msg.to_string()),
    })
    .unwrap_or_else(|_| r#"{"ok":false,"error":"serialization error"}"#.to_string())
}

unsafe fn cstr_to_opt_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    CStr::from_ptr(ptr)
        .to_str()
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn into_raw_c_string(s: String) -> *mut c_char {
    CString::new(s)
        .unwrap_or_else(|_| CString::new(r#"{"ok":false,"error":"interior nul"}"#).unwrap())
        .into_raw()
}

pub type ReelGetProgressFn = extern "C" fn(event_json: *const c_char, user_data: *mut c_void);

#[no_mangle]
pub extern "C" fn reelget_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// Free a string returned by `reelget_info_json` or `reelget_download_json`.
///
/// # Safety
/// `ptr` must be null or a pointer previously returned by this library.
#[no_mangle]
pub unsafe extern "C" fn reelget_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(CString::from_raw(ptr));
    }
}

/// Extract media metadata as JSON.
///
/// # Safety
/// All non-null pointers must be valid NUL-terminated UTF-8 C strings.
#[no_mangle]
pub unsafe extern "C" fn reelget_info_json(
    url: *const c_char,
    cookies_file: *const c_char,
    raw_cookie: *const c_char,
) -> *mut c_char {
    let Some(url_str) = cstr_to_opt_string(url) else {
        return into_raw_c_string(err_json("url is null or empty"));
    };
    let auth = AuthConfig {
        cookies_file: cstr_to_opt_string(cookies_file).map(PathBuf::from),
        raw_cookie: cstr_to_opt_string(raw_cookie),
        headers: Vec::new(),
        user_agent: None,
    };

    let client = ReelGetClient::new();
    let res = runtime().block_on(client.extract_info(&url_str, auth, 3));
    let json = match res {
        Ok(info) => ok_json(info),
        Err(e) => err_json(&e.to_string()),
    };
    into_raw_c_string(json)
}

/// Download a video or reel and return the result as JSON.
///
/// # Safety
/// All non-null string pointers must be valid NUL-terminated UTF-8 C strings.
/// `user_data` must remain valid for the duration of the call if `progress_cb` is provided.
#[no_mangle]
pub unsafe extern "C" fn reelget_download_json(
    url: *const c_char,
    output_dir: *const c_char,
    quality: *const c_char,
    cookies_file: *const c_char,
    raw_cookie: *const c_char,
    progress_cb: Option<ReelGetProgressFn>,
    user_data: *mut c_void,
) -> *mut c_char {
    let Some(url_str) = cstr_to_opt_string(url) else {
        return into_raw_c_string(err_json("url is null or empty"));
    };
    let out_dir = cstr_to_opt_string(output_dir)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let qual = cstr_to_opt_string(quality)
        .and_then(|q| QualityPreference::from_str(&q).ok())
        .unwrap_or_default();

    let auth = AuthConfig {
        cookies_file: cstr_to_opt_string(cookies_file).map(PathBuf::from),
        raw_cookie: cstr_to_opt_string(raw_cookie),
        headers: Vec::new(),
        user_agent: None,
    };

    let opts = DownloadOptions {
        output_dir: out_dir,
        quality: qual,
        auth,
        ..Default::default()
    };

    let user_data_addr = user_data as usize;
    let cb: Option<ProgressCallback> = progress_cb.map(|func| {
        Arc::new(move |ev| {
            if let Ok(json) = serde_json::to_string(&ev) {
                if let Ok(cstr) = CString::new(json) {
                    func(cstr.as_ptr(), user_data_addr as *mut c_void);
                }
            }
        }) as ProgressCallback
    });

    let client = ReelGetClient::new();
    let res = runtime().block_on(client.download(&url_str, opts, cb));
    let json = match res {
        Ok(dl) => ok_json(dl),
        Err(e) => err_json(&e.to_string()),
    };
    into_raw_c_string(json)
}

/// Download and clip a video or reel (`from_time` / `to_time` in `HH:MM:SS`, `MM:SS`, or `SS`).
///
/// # Safety
/// All non-null string pointers must be valid NUL-terminated UTF-8 C strings.
/// `user_data` must remain valid for the duration of the call if `progress_cb` is provided.
#[no_mangle]
pub unsafe extern "C" fn reelget_download_clip_json(
    url: *const c_char,
    output_dir: *const c_char,
    quality: *const c_char,
    from_time: *const c_char,
    to_time: *const c_char,
    cookies_file: *const c_char,
    raw_cookie: *const c_char,
    progress_cb: Option<ReelGetProgressFn>,
    user_data: *mut c_void,
) -> *mut c_char {
    let Some(url_str) = cstr_to_opt_string(url) else {
        return into_raw_c_string(err_json("url is null or empty"));
    };
    let out_dir = cstr_to_opt_string(output_dir)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let qual = cstr_to_opt_string(quality)
        .and_then(|q| QualityPreference::from_str(&q).ok())
        .unwrap_or_default();

    let from_opt = cstr_to_opt_string(from_time);
    let to_opt = cstr_to_opt_string(to_time);
    let clip = match (from_opt, to_opt) {
        (None, None) => None,
        (f, t) => {
            let start_secs = match f.as_deref().map(ClipRange::parse_timestamp).transpose() {
                Ok(v) => v,
                Err(e) => return into_raw_c_string(err_json(&e)),
            };
            let end_secs = match t.as_deref().map(ClipRange::parse_timestamp).transpose() {
                Ok(v) => v,
                Err(e) => return into_raw_c_string(err_json(&e)),
            };
            match ClipRange::new(start_secs, end_secs) {
                Ok(c) => Some(c),
                Err(e) => return into_raw_c_string(err_json(&e)),
            }
        }
    };

    let auth = AuthConfig {
        cookies_file: cstr_to_opt_string(cookies_file).map(PathBuf::from),
        raw_cookie: cstr_to_opt_string(raw_cookie),
        headers: Vec::new(),
        user_agent: None,
    };

    let opts = DownloadOptions {
        output_dir: out_dir,
        quality: qual,
        clip,
        auth,
        ..Default::default()
    };

    let user_data_addr = user_data as usize;
    let cb: Option<ProgressCallback> = progress_cb.map(|func| {
        Arc::new(move |ev| {
            if let Ok(json) = serde_json::to_string(&ev) {
                if let Ok(cstr) = CString::new(json) {
                    func(cstr.as_ptr(), user_data_addr as *mut c_void);
                }
            }
        }) as ProgressCallback
    });

    let client = ReelGetClient::new();
    let res = runtime().block_on(client.download(&url_str, opts, cb));
    let json = match res {
        Ok(dl) => ok_json(dl),
        Err(e) => err_json(&e.to_string()),
    };
    into_raw_c_string(json)
}

/// Download and convert audio to MP3 at the specified bitrate (`64`, `96`, `128`, `192`, `256`, `320`).
///
/// # Safety
/// All non-null string pointers must be valid NUL-terminated UTF-8 C strings.
/// `user_data` must remain valid for the duration of the call if `progress_cb` is provided.
#[no_mangle]
pub unsafe extern "C" fn reelget_download_mp3_json(
    url: *const c_char,
    output_dir: *const c_char,
    bitrate: *const c_char,
    from_time: *const c_char,
    to_time: *const c_char,
    cookies_file: *const c_char,
    raw_cookie: *const c_char,
    progress_cb: Option<ReelGetProgressFn>,
    user_data: *mut c_void,
) -> *mut c_char {
    let Some(url_str) = cstr_to_opt_string(url) else {
        return into_raw_c_string(err_json("url is null or empty"));
    };
    let out_dir = cstr_to_opt_string(output_dir)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let mp3_bitrate = match cstr_to_opt_string(bitrate) {
        Some(b) => match Mp3Bitrate::from_str(&b) {
            Ok(v) => v,
            Err(e) => return into_raw_c_string(err_json(&e)),
        },
        None => Mp3Bitrate::default(),
    };

    let from_opt = cstr_to_opt_string(from_time);
    let to_opt = cstr_to_opt_string(to_time);
    let clip = match (from_opt, to_opt) {
        (None, None) => None,
        (f, t) => {
            let start_secs = match f.as_deref().map(ClipRange::parse_timestamp).transpose() {
                Ok(v) => v,
                Err(e) => return into_raw_c_string(err_json(&e)),
            };
            let end_secs = match t.as_deref().map(ClipRange::parse_timestamp).transpose() {
                Ok(v) => v,
                Err(e) => return into_raw_c_string(err_json(&e)),
            };
            match ClipRange::new(start_secs, end_secs) {
                Ok(c) => Some(c),
                Err(e) => return into_raw_c_string(err_json(&e)),
            }
        }
    };

    let auth = AuthConfig {
        cookies_file: cstr_to_opt_string(cookies_file).map(PathBuf::from),
        raw_cookie: cstr_to_opt_string(raw_cookie),
        headers: Vec::new(),
        user_agent: None,
    };

    let opts = DownloadOptions {
        output_dir: out_dir,
        output_format: OutputFormat::Mp3,
        mp3_bitrate,
        clip,
        auth,
        ..Default::default()
    };

    let user_data_addr = user_data as usize;
    let cb: Option<ProgressCallback> = progress_cb.map(|func| {
        Arc::new(move |ev| {
            if let Ok(json) = serde_json::to_string(&ev) {
                if let Ok(cstr) = CString::new(json) {
                    func(cstr.as_ptr(), user_data_addr as *mut c_void);
                }
            }
        }) as ProgressCallback
    });

    let client = ReelGetClient::new();
    let res = runtime().block_on(client.download(&url_str, opts, cb));
    let json = match res {
        Ok(dl) => ok_json(dl),
        Err(e) => err_json(&e.to_string()),
    };
    into_raw_c_string(json)
}

#[cfg(any(target_os = "android", feature = "jni-bindings"))]
mod android_jni {
    use super::*;
    use jni::objects::{JClass, JObject, JString, JValue};
    use jni::sys::jstring;
    use jni::JNIEnv;

    fn get_opt_jstring(env: &mut JNIEnv, s: JString) -> Option<String> {
        if s.is_null() {
            return None;
        }
        env.get_string(&s)
            .ok()
            .map(|js| js.to_string_lossy().trim().to_string())
            .filter(|v| !v.is_empty())
    }

    #[no_mangle]
    pub extern "system" fn Java_com_reelget_ReelGet_nativeVersion<'local>(
        env: JNIEnv<'local>,
        _class: JClass<'local>,
    ) -> jstring {
        env.new_string(env!("CARGO_PKG_VERSION"))
            .map(|s| s.into_raw())
            .unwrap_or(std::ptr::null_mut())
    }

    #[no_mangle]
    pub extern "system" fn Java_com_reelget_ReelGet_nativeGetInfo<'local>(
        mut env: JNIEnv<'local>,
        _class: JClass<'local>,
        url: JString<'local>,
        cookies_file: JString<'local>,
        raw_cookie: JString<'local>,
    ) -> jstring {
        let Some(url_str) = get_opt_jstring(&mut env, url) else {
            return env
                .new_string(err_json("url is null or empty"))
                .map(|s| s.into_raw())
                .unwrap_or(std::ptr::null_mut());
        };

        let auth = AuthConfig {
            cookies_file: get_opt_jstring(&mut env, cookies_file).map(PathBuf::from),
            raw_cookie: get_opt_jstring(&mut env, raw_cookie),
            headers: Vec::new(),
            user_agent: None,
        };

        let client = ReelGetClient::new();
        let json = match runtime().block_on(client.extract_info(&url_str, auth, 3)) {
            Ok(info) => ok_json(info),
            Err(e) => err_json(&e.to_string()),
        };

        env.new_string(json)
            .map(|s| s.into_raw())
            .unwrap_or(std::ptr::null_mut())
    }

    #[no_mangle]
    pub extern "system" fn Java_com_reelget_ReelGet_nativeDownload<'local>(
        mut env: JNIEnv<'local>,
        _class: JClass<'local>,
        url: JString<'local>,
        output_dir: JString<'local>,
        quality: JString<'local>,
        cookies_file: JString<'local>,
        raw_cookie: JString<'local>,
        callback: JObject<'local>,
    ) -> jstring {
        let Some(url_str) = get_opt_jstring(&mut env, url) else {
            return env
                .new_string(err_json("url is null or empty"))
                .map(|s| s.into_raw())
                .unwrap_or(std::ptr::null_mut());
        };

        let out_dir = get_opt_jstring(&mut env, output_dir)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let qual = get_opt_jstring(&mut env, quality)
            .and_then(|q| QualityPreference::from_str(&q).ok())
            .unwrap_or_default();

        let auth = AuthConfig {
            cookies_file: get_opt_jstring(&mut env, cookies_file).map(PathBuf::from),
            raw_cookie: get_opt_jstring(&mut env, raw_cookie),
            headers: Vec::new(),
            user_agent: None,
        };

        let opts = DownloadOptions {
            output_dir: out_dir,
            quality: qual,
            auth,
            ..Default::default()
        };

        let cb: Option<ProgressCallback> = if !callback.is_null() {
            if let (Ok(jvm), Ok(global_cb)) = (env.get_java_vm(), env.new_global_ref(callback)) {
                Some(Arc::new(move |ev| {
                    if let Ok(json) = serde_json::to_string(&ev) {
                        if let Ok(mut jenv) = jvm.attach_current_thread() {
                            if let Ok(jstr) = jenv.new_string(&json) {
                                let _ = jenv.call_method(
                                    &global_cb,
                                    "onProgress",
                                    "(Ljava/lang/String;)V",
                                    &[JValue::Object(&jstr)],
                                );
                            }
                        }
                    }
                }))
            } else {
                None
            }
        } else {
            None
        };

        let client = ReelGetClient::new();
        let json = match runtime().block_on(client.download(&url_str, opts, cb)) {
            Ok(dl) => ok_json(dl),
            Err(e) => err_json(&e.to_string()),
        };

        env.new_string(json)
            .map(|s| s.into_raw())
            .unwrap_or(std::ptr::null_mut())
    }
}
