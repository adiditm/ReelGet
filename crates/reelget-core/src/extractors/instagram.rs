use async_trait::async_trait;
use regex::Regex;
use scraper::{Html, Selector};
use serde_json::Value;
use url::Url;

use crate::error::{ReelGetError, Result};
use crate::extractors::Extractor;
use crate::http::{browser_navigation_headers, HttpSession};
use crate::models::{MediaFormat, MediaInfo, MediaKind, Platform, StreamProtocol};
use crate::utils::{extract_json_scripts, find_json_object, unescape_url};

pub struct InstagramExtractor;

#[async_trait]
impl Extractor for InstagramExtractor {
    fn name(&self) -> &'static str {
        "instagram"
    }

    fn matches(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        let h = host.to_ascii_lowercase();
        h == "instagram.com" || h.ends_with(".instagram.com") || h == "instagr.am"
    }

    async fn extract(&self, url: &Url, session: &HttpSession) -> Result<MediaInfo> {
        let shortcode = extract_ig_shortcode(url);
        let has_session = session.get_cookie_value("instagram.com", "sessionid").is_some();

        let headers = browser_navigation_headers();
        let (final_url, html) = session.get_text_with_final_url(url, &headers).await?;

        let mut formats = Vec::new();
        let mut title = extract_og_title(&html).unwrap_or_else(|| {
            shortcode
                .as_ref()
                .map(|s| format!("Instagram Reel {s}"))
                .unwrap_or_else(|| "Instagram Video".to_string())
        });
        let mut uploader: Option<String> = None;
        let mut thumbnail_url = extract_og_meta(&html, "og:image");

        // Strategy 1: Walk embedded Relay/BBox JSON scripts
        let scripts = extract_json_scripts(&html);
        for script_val in &scripts {
            let matched = if let Some(ref sc) = shortcode {
                find_json_object(script_val, &|map| {
                    (map.get("code").and_then(|v| v.as_str()) == Some(sc.as_str())
                        || map.get("shortcode").and_then(|v| v.as_str()) == Some(sc.as_str()))
                        && (map.get("video_versions").is_some()
                            || map.get("video_url").is_some()
                            || map.get("carousel_media").is_some())
                })
            } else {
                None
            };

            let fallback = if matched.is_none() {
                find_json_object(script_val, &|map| {
                    map.get("video_versions")
                        .and_then(|v| v.as_array())
                        .map(|a| !a.is_empty())
                        .unwrap_or(false)
                })
            } else {
                None
            };

            if let Some(obj) = matched.or(fallback) {
                extract_ig_media_obj(obj, &mut formats, &mut title, &mut uploader, &mut thumbnail_url);
                if !formats.is_empty() {
                    break;
                }
            }
        }

        // Strategy 2: Authenticated Instagram Web API fallback (especially for private accounts)
        if formats.is_empty() && has_session {
            if let Some(ref sc) = shortcode {
                if let Some(media_id) = shortcode_to_media_id(sc) {
                    if let Ok(api_info) =
                        fetch_ig_api_media_info(&media_id, sc, session).await
                    {
                        return Ok(api_info);
                    }
                }
            }
        }

        // Strategy 3: Regex / OpenGraph fallback
        if formats.is_empty() {
            extract_ig_regex_fallback(&html, &mut formats);
        }
        if formats.is_empty() {
            if let Some(og_vid) = extract_og_meta(&html, "og:video")
                .or_else(|| extract_og_meta(&html, "og:video:secure_url"))
            {
                formats.push(MediaFormat {
                    format_id: "og_video".to_string(),
                    url: og_vid,
                    protocol: StreamProtocol::Direct,
                    kind: MediaKind::VideoAndAudio,
                    ext: "mp4".to_string(),
                    width: None,
                    height: None,
                    bitrate_bps: None,
                    filesize_bytes: None,
                    codecs: None,
                    quality_label: Some("Default".to_string()),
                    headers: vec![(
                        "Referer".to_string(),
                        "https://www.instagram.com/".to_string(),
                    )],
                });
            }
        }

        if formats.is_empty() {
            if final_url.path().contains("/accounts/login")
                || html.contains("if_not_gated_logged_out\":null")
                || !has_session
            {
                return Err(ReelGetError::AuthenticationRequired {
                    platform: "Instagram".to_string(),
                    hint: "This reel/post may be private or login-gated. Supply Instagram cookies (sessionid, csrftoken) via --cookies or --cookie.".to_string(),
                });
            }
            return Err(ReelGetError::MediaNotFound(format!(
                "No video stream found for {final_url}"
            )));
        }

        Ok(MediaInfo {
            id: shortcode.unwrap_or_else(|| "instagram_reel".to_string()),
            platform: Platform::Instagram,
            webpage_url: final_url.to_string(),
            title,
            uploader,
            duration_secs: None,
            thumbnail_url,
            is_private_or_auth: has_session,
            formats,
        })
    }
}

async fn fetch_ig_api_media_info(
    media_id: &str,
    shortcode: &str,
    session: &HttpSession,
) -> Result<MediaInfo> {
    let api_url = Url::parse(&format!(
        "https://www.instagram.com/api/v1/media/{media_id}/info/"
    ))
    .map_err(|e| ReelGetError::InvalidUrl(e.to_string()))?;

    let mut extra = vec![
        ("X-IG-App-ID".to_string(), "936619743392459".to_string()),
        ("X-Requested-With".to_string(), "XMLHttpRequest".to_string()),
        ("Referer".to_string(), "https://www.instagram.com/".to_string()),
    ];
    if let Some(csrf) = session.get_cookie_value("instagram.com", "csrftoken") {
        extra.push(("X-CSRFToken".to_string(), csrf));
    }

    let (_, body) = session.get_text_with_final_url(&api_url, &extra).await?;
    let json: Value = serde_json::from_str(&body)?;
    let item = json
        .get("items")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_object())
        .ok_or_else(|| ReelGetError::MediaNotFound("Empty Instagram API items".to_string()))?;

    let mut formats = Vec::new();
    let mut title = format!("Instagram Reel {shortcode}");
    let mut uploader = None;
    let mut thumb = None;
    extract_ig_media_obj(item, &mut formats, &mut title, &mut uploader, &mut thumb);

    if formats.is_empty() {
        return Err(ReelGetError::MediaNotFound(
            "No video_versions in Instagram API response".to_string(),
        ));
    }

    Ok(MediaInfo {
        id: shortcode.to_string(),
        platform: Platform::Instagram,
        webpage_url: format!("https://www.instagram.com/reel/{shortcode}/"),
        title,
        uploader,
        duration_secs: item.get("video_duration").and_then(|v| v.as_f64()),
        thumbnail_url: thumb,
        is_private_or_auth: true,
        formats,
    })
}

fn extract_ig_media_obj(
    obj: &serde_json::Map<String, Value>,
    formats: &mut Vec<MediaFormat>,
    title: &mut String,
    uploader: &mut Option<String>,
    thumbnail_url: &mut Option<String>,
) {
    if let Some(cap) = obj
        .get("caption")
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
    {
        if !cap.trim().is_empty() {
            *title = cap.trim().to_string();
        }
    }
    if uploader.is_none() {
        *uploader = obj
            .get("user")
            .and_then(|u| u.get("username"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
    }
    if thumbnail_url.is_none() {
        *thumbnail_url = obj
            .get("image_versions2")
            .and_then(|iv| iv.get("candidates"))
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|f| f.get("url"))
            .and_then(|u| u.as_str())
            .map(unescape_url);
    }

    if let Some(vv) = obj.get("video_versions").and_then(|v| v.as_array()) {
        push_video_versions(vv, formats, "https://www.instagram.com/");
    }

    if let Some(vurl) = obj.get("video_url").and_then(|v| v.as_str()) {
        let url = unescape_url(vurl);
        if url.starts_with("http") && !formats.iter().any(|f| f.url == url) {
            formats.push(MediaFormat {
                format_id: "video_url".to_string(),
                url,
                protocol: StreamProtocol::Direct,
                kind: MediaKind::VideoAndAudio,
                ext: "mp4".to_string(),
                width: obj
                    .get("original_width")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32),
                height: obj
                    .get("original_height")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32),
                bitrate_bps: None,
                filesize_bytes: None,
                codecs: None,
                quality_label: Some("Original".to_string()),
                headers: vec![(
                    "Referer".to_string(),
                    "https://www.instagram.com/".to_string(),
                )],
            });
        }
    }

    if formats.is_empty() {
        if let Some(carousel) = obj.get("carousel_media").and_then(|v| v.as_array()) {
            for item in carousel {
                if let Some(vv) = item.get("video_versions").and_then(|v| v.as_array()) {
                    push_video_versions(vv, formats, "https://www.instagram.com/");
                    if !formats.is_empty() {
                        break;
                    }
                }
            }
        }
    }
}

pub(crate) fn push_video_versions(
    vv: &[Value],
    formats: &mut Vec<MediaFormat>,
    referer: &str,
) {
    for (idx, item) in vv.iter().enumerate() {
        let Some(raw_url) = item.get("url").and_then(|v| v.as_str()) else {
            continue;
        };
        let url = unescape_url(raw_url);
        if !url.starts_with("http") || formats.iter().any(|f| f.url == url) {
            continue;
        }
        let width = item.get("width").and_then(|v| v.as_u64()).map(|n| n as u32);
        let height = item.get("height").and_then(|v| v.as_u64()).map(|n| n as u32);
        let vtype = item.get("type").and_then(|v| v.as_u64()).unwrap_or(idx as u64);
        let label = match (width, height) {
            (Some(w), Some(h)) => format!("{w}x{h}"),
            _ => format!("type_{vtype}"),
        };
        formats.push(MediaFormat {
            format_id: format!("v_{vtype}_{idx}"),
            url,
            protocol: StreamProtocol::Direct,
            kind: MediaKind::VideoAndAudio,
            ext: "mp4".to_string(),
            width,
            height,
            bitrate_bps: None,
            filesize_bytes: None,
            codecs: None,
            quality_label: Some(label),
            headers: vec![("Referer".to_string(), referer.to_string())],
        });
    }
}

fn extract_ig_regex_fallback(html: &str, formats: &mut Vec<MediaFormat>) {
    if let Ok(re) = Regex::new(r#""video_versions"\s*:\s*(\[[^\]]+\])"#) {
        if let Some(cap) = re.captures(html) {
            if let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(&cap[1]) {
                push_video_versions(&arr, formats, "https://www.instagram.com/");
            }
        }
    }
    if formats.is_empty() {
        if let Ok(re) = Regex::new(r#""video_url"\s*:\s*"([^"]+)""#) {
            if let Some(cap) = re.captures(html) {
                let url = unescape_url(&cap[1]);
                if url.starts_with("http") {
                    formats.push(MediaFormat {
                        format_id: "video_url".to_string(),
                        url,
                        protocol: StreamProtocol::Direct,
                        kind: MediaKind::VideoAndAudio,
                        ext: "mp4".to_string(),
                        width: None,
                        height: None,
                        bitrate_bps: None,
                        filesize_bytes: None,
                        codecs: None,
                        quality_label: Some("Default".to_string()),
                        headers: vec![(
                            "Referer".to_string(),
                            "https://www.instagram.com/".to_string(),
                        )],
                    });
                }
            }
        }
    }
}

fn extract_ig_shortcode(url: &Url) -> Option<String> {
    let segs: Vec<&str> = url.path_segments()?.collect();
    for (i, seg) in segs.iter().enumerate() {
        if matches!(*seg, "reel" | "reels" | "p" | "tv") && i + 1 < segs.len() {
            let code = segs[i + 1].trim_matches('/');
            if !code.is_empty() {
                return Some(code.to_string());
            }
        }
    }
    None
}

/// Convert an Instagram shortcode (base64url alphabet) to its numeric media ID.
pub fn shortcode_to_media_id(shortcode: &str) -> Option<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut id: u128 = 0;
    for b in shortcode.bytes() {
        let pos = ALPHABET.iter().position(|&c| c == b)? as u128;
        id = id.checked_mul(64)?.checked_add(pos)?;
    }
    Some(id.to_string())
}

fn extract_og_title(html: &str) -> Option<String> {
    extract_og_meta(html, "og:title")
}

fn extract_og_meta(html: &str, prop: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse(&format!(r#"meta[property="{prop}"]"#)).ok()?;
    doc.select(&sel)
        .next()
        .and_then(|el| el.value().attr("content"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shortcode_to_media_id() {
        assert_eq!(
            shortcode_to_media_id("ChxvmGJLfsW").as_deref(),
            Some("2914319760008018710")
        );
    }
}
