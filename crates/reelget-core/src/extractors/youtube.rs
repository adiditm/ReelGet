use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use url::Url;

use crate::auth::compute_sapisidhash;
use crate::error::{ReelGetError, Result};
use crate::extractors::Extractor;
use crate::http::HttpSession;
use crate::models::{MediaFormat, MediaInfo, MediaKind, Platform, StreamProtocol};

const ANDROID_UA: &str = "com.google.android.youtube/20.10.38 (Linux; U; Android 14) gzip";
const IOS_UA: &str =
    "com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)";

pub struct YouTubeExtractor;

#[async_trait]
impl Extractor for YouTubeExtractor {
    fn name(&self) -> &'static str {
        "youtube"
    }

    fn matches(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        let h = host.to_ascii_lowercase();
        h == "youtube.com"
            || h.ends_with(".youtube.com")
            || h == "youtu.be"
            || h == "youtube-nocookie.com"
            || h.ends_with(".youtube-nocookie.com")
    }

    async fn extract(&self, url: &Url, session: &HttpSession) -> Result<MediaInfo> {
        let video_id = extract_youtube_id(url).ok_or_else(|| {
            ReelGetError::InvalidUrl(format!("Could not extract YouTube video ID from {url}"))
        })?;

        let watch_url = Url::parse(&format!("https://www.youtube.com/watch?v={video_id}"))
            .map_err(|e| ReelGetError::InvalidUrl(e.to_string()))?;

        let (_, html) = session.get_text_with_final_url(&watch_url, &[]).await?;

        let api_key = extract_regex_group(&html, r#""INNERTUBE_API_KEY"\s*:\s*"([^"]+)""#)
            .unwrap_or_else(|| "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8".to_string());
        let visitor_data = extract_regex_group(&html, r#""visitorData"\s*:\s*"([^"]+)""#);

        let sapisid = session
            .get_cookie_value("youtube.com", "SAPISID")
            .or_else(|| session.get_cookie_value("youtube.com", "__Secure-3PAPISID"))
            .or_else(|| session.get_cookie_value("google.com", "SAPISID"));
        let has_auth = sapisid.is_some();

        // Try ANDROID InnerTube first (provides progressive muxed MP4 + adaptive 1080p/audio with direct URLs)
        let mut player_json = call_innertube_player(
            &video_id,
            &api_key,
            visitor_data.as_deref(),
            sapisid.as_deref(),
            InnerTubeClient::Android,
            session,
        )
        .await;

        // Fallback to IOS InnerTube if ANDROID failed or returned no playable formats
        let android_ok = player_json
            .as_ref()
            .ok()
            .and_then(|j| j.get("streamingData"))
            .is_some();
        let mut used_ua = ANDROID_UA;

        if !android_ok {
            if let Ok(ios_json) = call_innertube_player(
                &video_id,
                &api_key,
                visitor_data.as_deref(),
                sapisid.as_deref(),
                InnerTubeClient::Ios,
                session,
            )
            .await
            {
                player_json = Ok(ios_json);
                used_ua = IOS_UA;
            }
        }

        let player = player_json?;
        let status = player
            .get("playabilityStatus")
            .and_then(|p| p.get("status"))
            .and_then(|s| s.as_str())
            .unwrap_or("");
        let reason = player
            .get("playabilityStatus")
            .and_then(|p| p.get("reason"))
            .and_then(|s| s.as_str())
            .unwrap_or("");

        if status == "LOGIN_REQUIRED" || status == "ERROR" || status == "UNPLAYABLE" {
            if !has_auth || reason.to_ascii_lowercase().contains("private") || reason.to_ascii_lowercase().contains("sign in") {
                return Err(ReelGetError::AuthenticationRequired {
                    platform: "YouTube".to_string(),
                    hint: format!(
                        "YouTube returned '{status}: {reason}'. For private/members-only/age-restricted videos, supply YouTube cookies (including SAPISID) via --cookies."
                    ),
                });
            }
            return Err(ReelGetError::ExtractorFailed {
                platform: "YouTube".to_string(),
                message: format!("{status}: {reason}"),
            });
        }

        let vd = player.get("videoDetails");
        let title = vd
            .and_then(|v| v.get("title"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("YouTube {video_id}"));
        let uploader = vd
            .and_then(|v| v.get("author"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let duration_secs = vd
            .and_then(|v| v.get("lengthSeconds"))
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok());
        let thumbnail_url = vd
            .and_then(|v| v.get("thumbnail"))
            .and_then(|t| t.get("thumbnails"))
            .and_then(|a| a.as_array())
            .and_then(|a| a.last())
            .and_then(|t| t.get("url"))
            .and_then(|u| u.as_str())
            .map(|s| s.to_string());

        let sd = player
            .get("streamingData")
            .ok_or_else(|| ReelGetError::MediaNotFound("Missing streamingData in YouTube response".to_string()))?;

        let mut formats = Vec::new();
        let fmt_headers = vec![("User-Agent".to_string(), used_ua.to_string())];

        if let Some(muxed) = sd.get("formats").and_then(|v| v.as_array()) {
            for f in muxed {
                if let Some(mf) = parse_yt_format(f, MediaKind::VideoAndAudio, &fmt_headers) {
                    formats.push(mf);
                }
            }
        }

        if let Some(adaptive) = sd.get("adaptiveFormats").and_then(|v| v.as_array()) {
            for f in adaptive {
                let mime = f.get("mimeType").and_then(|v| v.as_str()).unwrap_or("");
                let kind = if mime.starts_with("audio/") {
                    MediaKind::AudioOnly
                } else {
                    MediaKind::VideoOnly
                };
                if let Some(mf) = parse_yt_format(f, kind, &fmt_headers) {
                    formats.push(mf);
                }
            }
        }

        if let Some(hls_url) = sd.get("hlsManifestUrl").and_then(|v| v.as_str()) {
            formats.push(MediaFormat {
                format_id: "hls".to_string(),
                url: hls_url.to_string(),
                protocol: StreamProtocol::Hls,
                kind: MediaKind::VideoAndAudio,
                ext: "mp4".to_string(),
                width: None,
                height: None,
                bitrate_bps: None,
                filesize_bytes: None,
                codecs: None,
                quality_label: Some("HLS".to_string()),
                headers: fmt_headers,
            });
        }

        if formats.is_empty() {
            return Err(ReelGetError::MediaNotFound(format!(
                "No direct YouTube stream URLs found for {video_id}"
            )));
        }

        Ok(MediaInfo {
            id: video_id,
            platform: Platform::YouTube,
            webpage_url: url.to_string(),
            title,
            uploader,
            duration_secs,
            thumbnail_url,
            is_private_or_auth: has_auth,
            formats,
        })
    }
}

enum InnerTubeClient {
    Android,
    Ios,
}

async fn call_innertube_player(
    video_id: &str,
    api_key: &str,
    visitor_data: Option<&str>,
    sapisid: Option<&str>,
    client: InnerTubeClient,
    session: &HttpSession,
) -> Result<Value> {
    let endpoint = Url::parse(&format!(
        "https://www.youtube.com/youtubei/v1/player?key={api_key}&prettyPrint=false"
    ))
    .map_err(|e| ReelGetError::InvalidUrl(e.to_string()))?;

    let (client_name, client_id, client_ver, ua, client_obj) = match client {
        InnerTubeClient::Android => (
            "ANDROID",
            "3",
            "20.10.38",
            ANDROID_UA,
            json!({
                "clientName": "ANDROID",
                "clientVersion": "20.10.38",
                "androidSdkVersion": 34,
                "hl": "en",
                "gl": "US",
                "visitorData": visitor_data.unwrap_or("")
            }),
        ),
        InnerTubeClient::Ios => (
            "IOS",
            "5",
            "20.10.4",
            IOS_UA,
            json!({
                "clientName": "IOS",
                "clientVersion": "20.10.4",
                "deviceMake": "Apple",
                "deviceModel": "iPhone16,2",
                "osName": "iPhone",
                "osVersion": "18.3.2.22D82",
                "hl": "en",
                "gl": "US",
                "visitorData": visitor_data.unwrap_or("")
            }),
        ),
    };

    let body = json!({
        "videoId": video_id,
        "context": {
            "client": client_obj
        },
        "contentCheckOk": true,
        "racyCheckOk": true
    });

    let mut extra = vec![
        ("User-Agent".to_string(), ua.to_string()),
        ("X-YouTube-Client-Name".to_string(), client_id.to_string()),
        ("X-YouTube-Client-Version".to_string(), client_ver.to_string()),
        ("Origin".to_string(), "https://www.youtube.com".to_string()),
    ];

    if let Some(sid) = sapisid {
        let auth_hdr = compute_sapisidhash(sid, "https://www.youtube.com");
        extra.push(("Authorization".to_string(), auth_hdr));
        extra.push(("X-Origin".to_string(), "https://www.youtube.com".to_string()));
    }

    tracing::debug!(client = client_name, video_id, "Calling YouTube InnerTube player");
    session.post_json(&endpoint, &body, &extra).await
}

fn parse_yt_format(
    f: &Value,
    kind: MediaKind,
    headers: &[(String, String)],
) -> Option<MediaFormat> {
    let url = f.get("url").and_then(|v| v.as_str())?.to_string();
    let itag = f.get("itag").and_then(|v| v.as_u64()).unwrap_or(0);
    let mime = f.get("mimeType").and_then(|v| v.as_str()).unwrap_or("");

    let ext = if mime.contains("webm") {
        "webm"
    } else if mime.contains("audio/mp4") {
        "m4a"
    } else {
        "mp4"
    }
    .to_string();

    let codecs = mime
        .split("codecs=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .map(|s| s.to_string());

    let width = f.get("width").and_then(|v| v.as_u64()).map(|n| n as u32);
    let height = f.get("height").and_then(|v| v.as_u64()).map(|n| n as u32);
    let bitrate_bps = f.get("bitrate").and_then(|v| v.as_u64());
    let filesize_bytes = f
        .get("contentLength")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u64>().ok());
    let quality_label = f
        .get("qualityLabel")
        .or_else(|| f.get("audioQuality"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Some(MediaFormat {
        format_id: itag.to_string(),
        url,
        protocol: StreamProtocol::Direct,
        kind,
        ext,
        width,
        height,
        bitrate_bps,
        filesize_bytes,
        codecs,
        quality_label,
        headers: headers.to_vec(),
    })
}

pub fn extract_youtube_id(url: &Url) -> Option<String> {
    let host = url.host_str()?.to_ascii_lowercase();
    if host == "youtu.be" {
        let seg = url.path_segments()?.next()?.trim_matches('/');
        if !seg.is_empty() {
            return Some(seg.to_string());
        }
    }
    for (k, v) in url.query_pairs() {
        if k == "v" && !v.is_empty() {
            return Some(v.to_string());
        }
    }
    let segs: Vec<&str> = url.path_segments()?.collect();
    for (i, seg) in segs.iter().enumerate() {
        if matches!(*seg, "shorts" | "live" | "embed" | "v") && i + 1 < segs.len() {
            let id = segs[i + 1].trim_matches('/');
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn extract_regex_group(text: &str, pattern: &str) -> Option<String> {
    let re = Regex::new(pattern).ok()?;
    re.captures(text).map(|c| c[1].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_youtube_id() {
        assert_eq!(
            extract_youtube_id(&Url::parse("https://www.youtube.com/watch?v=K6qj09OHvjw").unwrap())
                .as_deref(),
            Some("K6qj09OHvjw")
        );
        assert_eq!(
            extract_youtube_id(&Url::parse("https://www.youtube.com/shorts/k5Z7sFwFF7Q").unwrap())
                .as_deref(),
            Some("k5Z7sFwFF7Q")
        );
        assert_eq!(
            extract_youtube_id(&Url::parse("https://youtu.be/xWF2IuEO2nw").unwrap()).as_deref(),
            Some("xWF2IuEO2nw")
        );
    }
}
