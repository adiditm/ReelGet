use async_trait::async_trait;
use serde_json::Value;
use url::Url;

use crate::error::{ReelGetError, Result};
use crate::extractors::Extractor;
use crate::http::{HttpSession, MOBILE_SAFARI_USER_AGENT};
use crate::models::{MediaFormat, MediaInfo, MediaKind, Platform, StreamProtocol};
use crate::utils::{extract_json_script_by_id, unescape_url};

pub struct TikTokExtractor;

#[async_trait]
impl Extractor for TikTokExtractor {
    fn name(&self) -> &'static str {
        "tiktok"
    }

    fn matches(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        let h = host.to_ascii_lowercase();
        h == "tiktok.com" || h.ends_with(".tiktok.com")
    }

    async fn extract(&self, url: &Url, session: &HttpSession) -> Result<MediaInfo> {
        let ua = session
            .auth()
            .user_agent
            .as_deref()
            .unwrap_or(MOBILE_SAFARI_USER_AGENT)
            .to_string();

        let extra = vec![
            ("User-Agent".to_string(), ua.clone()),
            (
                "Accept".to_string(),
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".to_string(),
            ),
            ("Accept-Language".to_string(), "en-US,en;q=0.9".to_string()),
        ];

        let (final_url, html) = session.get_text_with_final_url(url, &extra).await?;
        let has_session = session.get_cookie_value("tiktok.com", "sessionid").is_some()
            || session.get_cookie_value("tiktok.com", "sid_tt").is_some();

        let item_struct = extract_tiktok_item_struct(&html);
        let Some(item) = item_struct else {
            if html.contains("SlardarWAF") {
                return Err(ReelGetError::ExtractorFailed {
                    platform: "TikTok".to_string(),
                    message: "TikTok returned a WAF challenge. Try supplying TikTok cookies via --cookies.".to_string(),
                });
            }
            if !has_session {
                return Err(ReelGetError::AuthenticationRequired {
                    platform: "TikTok".to_string(),
                    hint: "Video may be private or friends-only. Supply TikTok cookies (sessionid, sid_tt) via --cookies or --cookie.".to_string(),
                });
            }
            return Err(ReelGetError::MediaNotFound(format!(
                "Could not locate TikTok video metadata for {final_url}"
            )));
        };

        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| extract_tiktok_id(&final_url))
            .unwrap_or_else(|| "tiktok_video".to_string());

        let title = item
            .get("desc")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("TikTok {id}"));

        let uploader = item
            .get("author")
            .and_then(|a| a.get("uniqueId").or_else(|| a.get("nickname")))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let video = item
            .get("video")
            .and_then(|v| v.as_object())
            .ok_or_else(|| ReelGetError::MediaNotFound("Missing video object in TikTok JSON".to_string()))?;

        let duration_secs = video.get("duration").and_then(|v| v.as_f64());
        let thumbnail_url = video
            .get("cover")
            .or_else(|| video.get("originCover"))
            .and_then(|v| v.as_str())
            .map(unescape_url);

        let width = video.get("width").and_then(|v| v.as_u64()).map(|n| n as u32);
        let height = video.get("height").and_then(|v| v.as_u64()).map(|n| n as u32);

        let format_headers = vec![
            ("User-Agent".to_string(), ua),
            ("Referer".to_string(), "https://www.tiktok.com/".to_string()),
        ];

        let mut formats = Vec::new();

        // 1. bitrateInfo variants (often highest quality / multiple bitrates)
        if let Some(bitrates) = video.get("bitrateInfo").and_then(|v| v.as_array()) {
            for (idx, br) in bitrates.iter().enumerate() {
                let bitrate_bps = br.get("Bitrate").and_then(|v| v.as_u64());
                let gear = br
                    .get("GearName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("variant");
                let play_addr = br.get("PlayAddr");
                let w = play_addr
                    .and_then(|p| p.get("Width"))
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32)
                    .or(width);
                let h = play_addr
                    .and_then(|p| p.get("Height"))
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32)
                    .or(height);
                if let Some(urls) = play_addr
                    .and_then(|p| p.get("UrlList"))
                    .and_then(|v| v.as_array())
                {
                    if let Some(first_url) = urls.iter().filter_map(|u| u.as_str()).next() {
                        let clean = unescape_url(first_url);
                        if clean.starts_with("http") && !formats.iter().any(|f: &MediaFormat| f.url == clean) {
                            formats.push(MediaFormat {
                                format_id: format!("br_{idx}_{gear}"),
                                url: clean,
                                protocol: StreamProtocol::Direct,
                                kind: MediaKind::VideoAndAudio,
                                ext: "mp4".to_string(),
                                width: w,
                                height: h,
                                bitrate_bps,
                                filesize_bytes: None,
                                codecs: br
                                    .get("CodecType")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string()),
                                quality_label: Some(gear.to_string()),
                                headers: format_headers.clone(),
                            });
                        }
                    }
                }
            }
        }

        // 2. playAddr & downloadAddr
        for (fid, key, label) in [
            ("play", "playAddr", "Play"),
            ("download", "downloadAddr", "Download"),
        ] {
            if let Some(raw) = video.get(key).and_then(|v| v.as_str()) {
                let clean = unescape_url(raw);
                if clean.starts_with("http") && !formats.iter().any(|f| f.url == clean) {
                    formats.push(MediaFormat {
                        format_id: fid.to_string(),
                        url: clean,
                        protocol: StreamProtocol::Direct,
                        kind: MediaKind::VideoAndAudio,
                        ext: "mp4".to_string(),
                        width,
                        height,
                        bitrate_bps: video.get("bitrate").and_then(|v| v.as_u64()),
                        filesize_bytes: None,
                        codecs: None,
                        quality_label: Some(label.to_string()),
                        headers: format_headers.clone(),
                    });
                }
            }
        }

        if formats.is_empty() {
            return Err(ReelGetError::MediaNotFound(format!(
                "No playable TikTok video URLs found for {final_url}"
            )));
        }

        Ok(MediaInfo {
            id,
            platform: Platform::TikTok,
            webpage_url: final_url.to_string(),
            title,
            uploader,
            duration_secs,
            thumbnail_url,
            is_private_or_auth: has_session,
            formats,
        })
    }
}

fn extract_tiktok_item_struct(html: &str) -> Option<Value> {
    // 1. Mobile SSR <script id="api-data">
    if let Some(api_data) = extract_json_script_by_id(html, "api-data") {
        if let Some(item) = api_data
            .get("videoDetail")
            .and_then(|v| v.get("itemInfo"))
            .and_then(|v| v.get("itemStruct"))
        {
            if item.get("video").is_some() {
                return Some(item.clone());
            }
        }
    }

    // 2. Desktop SSR <script id="__UNIVERSAL_DATA_FOR_REHYDRATION__">
    if let Some(univ) = extract_json_script_by_id(html, "__UNIVERSAL_DATA_FOR_REHYDRATION__") {
        if let Some(item) = univ
            .get("__DEFAULT_SCOPE__")
            .and_then(|s| s.get("webapp.video-detail"))
            .and_then(|v| v.get("itemInfo"))
            .and_then(|v| v.get("itemStruct"))
        {
            if item.get("video").is_some() {
                return Some(item.clone());
            }
        }
    }

    // 3. Legacy <script id="SIGI_STATE">
    if let Some(sigi) = extract_json_script_by_id(html, "SIGI_STATE") {
        if let Some(item_mod) = sigi.get("ItemModule").and_then(|v| v.as_object()) {
            if let Some((_, first_item)) = item_mod.iter().next() {
                if first_item.get("video").is_some() {
                    return Some(first_item.clone());
                }
            }
        }
    }

    None
}

fn extract_tiktok_id(url: &Url) -> Option<String> {
    let segs: Vec<&str> = url.path_segments()?.collect();
    for (i, seg) in segs.iter().enumerate() {
        if (*seg == "video" || *seg == "v") && i + 1 < segs.len() {
            let id = segs[i + 1].trim_matches('/');
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}
