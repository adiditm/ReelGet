use async_trait::async_trait;
use reqwest::header::CONTENT_TYPE;
use url::Url;

use crate::error::{ReelGetError, Result};
use crate::extractors::Extractor;
use crate::http::HttpSession;
use crate::models::{MediaFormat, MediaInfo, MediaKind, Platform, StreamProtocol};

pub struct DirectExtractor;

#[async_trait]
impl Extractor for DirectExtractor {
    fn name(&self) -> &'static str {
        "direct"
    }

    fn matches(&self, _url: &Url) -> bool {
        true
    }

    async fn extract(&self, url: &Url, session: &HttpSession) -> Result<MediaInfo> {
        let path_lower = url.path().to_ascii_lowercase();
        let ext_guess = detect_ext_from_path(&path_lower);

        if let Some((ext, protocol)) = ext_guess {
            return Ok(build_direct_info(url, ext, protocol, None));
        }

        // Probe Content-Type via HEAD
        if let Ok(resp) = session.head_or_probe(url, &[]).await {
            if resp.status().is_success() {
                let ct = resp
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let len = resp.content_length();
                if let Some((ext, protocol)) = detect_from_content_type(&ct) {
                    return Ok(build_direct_info(resp.url(), ext, protocol, len));
                }
            }
        }

        Err(ReelGetError::UnsupportedPlatform(
            "Not a direct media URL".to_string(),
        ))
    }
}

fn detect_ext_from_path(path_lower: &str) -> Option<(&'static str, StreamProtocol)> {
    if path_lower.ends_with(".m3u8") {
        Some(("mp4", StreamProtocol::Hls))
    } else if path_lower.ends_with(".mp4") || path_lower.ends_with(".m4v") {
        Some(("mp4", StreamProtocol::Direct))
    } else if path_lower.ends_with(".webm") {
        Some(("webm", StreamProtocol::Direct))
    } else if path_lower.ends_with(".mov") {
        Some(("mov", StreamProtocol::Direct))
    } else if path_lower.ends_with(".mkv") {
        Some(("mkv", StreamProtocol::Direct))
    } else if path_lower.ends_with(".ts") {
        Some(("ts", StreamProtocol::Direct))
    } else if path_lower.ends_with(".m4a") {
        Some(("m4a", StreamProtocol::Direct))
    } else {
        None
    }
}

fn detect_from_content_type(ct: &str) -> Option<(&'static str, StreamProtocol)> {
    if ct.contains("mpegurl") {
        Some(("mp4", StreamProtocol::Hls))
    } else if ct.contains("video/mp4") {
        Some(("mp4", StreamProtocol::Direct))
    } else if ct.contains("video/webm") {
        Some(("webm", StreamProtocol::Direct))
    } else if ct.contains("video/quicktime") {
        Some(("mov", StreamProtocol::Direct))
    } else if ct.starts_with("video/") {
        Some(("mp4", StreamProtocol::Direct))
    } else {
        None
    }
}

fn build_direct_info(
    url: &Url,
    ext: &str,
    protocol: StreamProtocol,
    filesize_bytes: Option<u64>,
) -> MediaInfo {
    let filename = url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .filter(|s| !s.is_empty())
        .unwrap_or("video");
    let title = filename
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(filename)
        .to_string();

    MediaInfo {
        id: title.clone(),
        platform: Platform::Direct,
        webpage_url: url.to_string(),
        title,
        uploader: url.host_str().map(|s| s.to_string()),
        duration_secs: None,
        thumbnail_url: None,
        is_private_or_auth: false,
        formats: vec![MediaFormat {
            format_id: "direct".to_string(),
            url: url.to_string(),
            protocol,
            kind: MediaKind::VideoAndAudio,
            ext: ext.to_string(),
            width: None,
            height: None,
            bitrate_bps: None,
            filesize_bytes,
            codecs: None,
            quality_label: Some("Direct".to_string()),
            headers: Vec::new(),
        }],
    }
}
