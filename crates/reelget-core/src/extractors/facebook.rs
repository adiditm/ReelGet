use async_trait::async_trait;
use regex::Regex;
use scraper::{Html, Selector};
use serde_json::Value;
use url::Url;

use crate::error::{ReelGetError, Result};
use crate::extractors::Extractor;
use crate::http::{browser_navigation_headers, HttpSession};
use crate::models::{MediaFormat, MediaInfo, MediaKind, Platform, StreamProtocol};
use crate::utils::{collect_json_objects, extract_json_scripts, find_json_object, unescape_url};

pub struct FacebookExtractor;

#[async_trait]
impl Extractor for FacebookExtractor {
    fn name(&self) -> &'static str {
        "facebook"
    }

    fn matches(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        let h = host.to_ascii_lowercase();
        h == "facebook.com"
            || h.ends_with(".facebook.com")
            || h == "fb.watch"
            || h == "fb.com"
            || h.ends_with(".fb.com")
    }

    async fn extract(&self, url: &Url, session: &HttpSession) -> Result<MediaInfo> {
        let headers = browser_navigation_headers();
        let target_id = extract_fb_video_id(url);

        let (final_url, html) = match session.get_text_with_final_url(url, &headers).await {
            Ok(res) => res,
            Err(e) => {
                // Facebook `/watch/?v=<id>` often triggers `_rdc=N&_rdr` redirect loops on non-browser TLS;
                // fall back to `/video.php?v=<id>` and `/reel/<id>` which resolve directly.
                if let Some(ref vid) = target_id {
                    let alt1 = Url::parse(&format!("https://www.facebook.com/video.php?v={vid}"))
                        .map_err(|err| ReelGetError::InvalidUrl(err.to_string()))?;
                    if let Ok(res) = session.get_text_with_final_url(&alt1, &headers).await {
                        res
                    } else {
                        let alt2 = Url::parse(&format!("https://www.facebook.com/reel/{vid}"))
                            .map_err(|err| ReelGetError::InvalidUrl(err.to_string()))?;
                        session.get_text_with_final_url(&alt2, &headers).await?
                    }
                } else {
                    return Err(e);
                }
            }
        };

        let target_id = extract_fb_video_id(&final_url).or(target_id);
        let has_auth_cookies = session.get_cookie_value("facebook.com", "c_user").is_some()
            && session.get_cookie_value("facebook.com", "xs").is_some();

        let mut formats = Vec::new();
        let mut title = extract_html_title(&html).unwrap_or_else(|| "Facebook Video".to_string());
        let mut uploader: Option<String> = None;

        let scripts = extract_json_scripts(&html);
        for script_val in &scripts {
            // Prefer exact match on target video ID so we never pick recommended feed reels
            let matched_obj = if let Some(ref vid) = target_id {
                find_json_object(script_val, &|map| {
                    map.get("id").and_then(|v| v.as_str()) == Some(vid.as_str())
                        && has_fb_video_fields(map)
                })
            } else {
                None
            };

            let fallback_obj = if matched_obj.is_none() {
                // Look inside primary video/creation_story nodes, avoiding lasso_blue_feed recommendations
                let mut candidates = Vec::new();
                collect_json_objects(script_val, &has_fb_video_fields, &mut candidates);
                candidates.into_iter().next()
            } else {
                None
            };

            if let Some(obj) = matched_obj.or(fallback_obj) {
                collect_formats_from_fb_obj(obj, &mut formats);
            }

            if uploader.is_none() {
                if let Some(owner) = find_json_object(script_val, &|map| {
                    map.get("__typename").and_then(|v| v.as_str()) == Some("User")
                        && map.get("name").and_then(|v| v.as_str()).is_some()
                }) {
                    uploader = owner
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                }
            }

            if !formats.is_empty() {
                break;
            }
        }

        // Regex fallback if JSON script parsing missed inline JS payloads
        if formats.is_empty() {
            extract_fb_regex_fallback(&html, &mut formats);
        }

        if formats.is_empty() {
            if is_fb_login_wall(&final_url, &html) {
                return Err(ReelGetError::AuthenticationRequired {
                    platform: "Facebook".to_string(),
                    hint: "This video appears private or login-gated. Supply Facebook cookies (c_user, xs) via --cookies or --cookie.".to_string(),
                });
            }
            return Err(ReelGetError::MediaNotFound(format!(
                "Could not locate Facebook video stream for {final_url}"
            )));
        }

        // Clean up generic Facebook title suffix
        if let Some(stripped) = title.strip_suffix(" | Facebook") {
            title = stripped.trim().to_string();
        }

        let id = target_id.unwrap_or_else(|| "facebook_video".to_string());
        Ok(MediaInfo {
            id,
            platform: Platform::Facebook,
            webpage_url: final_url.to_string(),
            title,
            uploader,
            duration_secs: None,
            thumbnail_url: extract_og_image(&html),
            is_private_or_auth: has_auth_cookies,
            formats,
        })
    }
}

fn extract_fb_video_id(url: &Url) -> Option<String> {
    for (k, v) in url.query_pairs() {
        if k == "v" && !v.is_empty() {
            return Some(v.to_string());
        }
    }
    let segments: Vec<&str> = url.path_segments()?.collect();
    for (i, seg) in segments.iter().enumerate() {
        if (*seg == "reel" || *seg == "videos" || *seg == "v") && i + 1 < segments.len() {
            let candidate = segments[i + 1].trim_matches('/');
            if !candidate.is_empty() && candidate.chars().all(|c| c.is_ascii_digit()) {
                return Some(candidate.to_string());
            }
        }
    }
    None
}

fn has_fb_video_fields(map: &serde_json::Map<String, Value>) -> bool {
    map.get("browser_native_hd_url")
        .and_then(|v| v.as_str())
        .is_some()
        || map
            .get("browser_native_sd_url")
            .and_then(|v| v.as_str())
            .is_some()
        || map
            .get("playable_url_quality_hd")
            .and_then(|v| v.as_str())
            .is_some()
        || map.get("playable_url").and_then(|v| v.as_str()).is_some()
}

fn collect_formats_from_fb_obj(
    obj: &serde_json::Map<String, Value>,
    formats: &mut Vec<MediaFormat>,
) {
    let hd_keys = ["browser_native_hd_url", "playable_url_quality_hd"];
    for key in hd_keys {
        if let Some(raw) = obj.get(key).and_then(|v| v.as_str()) {
            let url = unescape_url(raw);
            if url.starts_with("http") && !formats.iter().any(|f| f.url == url) {
                formats.push(MediaFormat {
                    format_id: "hd".to_string(),
                    url,
                    protocol: StreamProtocol::Direct,
                    kind: MediaKind::VideoAndAudio,
                    ext: "mp4".to_string(),
                    width: None,
                    height: Some(720),
                    bitrate_bps: None,
                    filesize_bytes: None,
                    codecs: None,
                    quality_label: Some("HD".to_string()),
                    headers: vec![("Referer".to_string(), "https://www.facebook.com/".to_string())],
                });
            }
        }
    }

    let sd_keys = ["browser_native_sd_url", "playable_url"];
    for key in sd_keys {
        if let Some(raw) = obj.get(key).and_then(|v| v.as_str()) {
            let url = unescape_url(raw);
            if url.starts_with("http") && !formats.iter().any(|f| f.url == url) {
                formats.push(MediaFormat {
                    format_id: "sd".to_string(),
                    url,
                    protocol: StreamProtocol::Direct,
                    kind: MediaKind::VideoAndAudio,
                    ext: "mp4".to_string(),
                    width: None,
                    height: Some(360),
                    bitrate_bps: None,
                    filesize_bytes: None,
                    codecs: None,
                    quality_label: Some("SD".to_string()),
                    headers: vec![("Referer".to_string(), "https://www.facebook.com/".to_string())],
                });
            }
        }
    }
}

fn extract_fb_regex_fallback(html: &str, formats: &mut Vec<MediaFormat>) {
    let patterns = [
        ("hd", Some(720), "HD", r#""browser_native_hd_url"\s*:\s*"([^"]+)""#),
        ("hd", Some(720), "HD", r#""playable_url_quality_hd"\s*:\s*"([^"]+)""#),
        ("sd", Some(360), "SD", r#""browser_native_sd_url"\s*:\s*"([^"]+)""#),
        ("sd", Some(360), "SD", r#""playable_url"\s*:\s*"([^"]+)""#),
    ];
    for (fid, height, label, pat) in patterns {
        if let Ok(re) = Regex::new(pat) {
            if let Some(cap) = re.captures(html) {
                let url = unescape_url(&cap[1]);
                if url.starts_with("http") && !formats.iter().any(|f| f.url == url) {
                    formats.push(MediaFormat {
                        format_id: fid.to_string(),
                        url,
                        protocol: StreamProtocol::Direct,
                        kind: MediaKind::VideoAndAudio,
                        ext: "mp4".to_string(),
                        width: None,
                        height,
                        bitrate_bps: None,
                        filesize_bytes: None,
                        codecs: None,
                        quality_label: Some(label.to_string()),
                        headers: vec![(
                            "Referer".to_string(),
                            "https://www.facebook.com/".to_string(),
                        )],
                    });
                }
            }
        }
    }
}

fn is_fb_login_wall(final_url: &Url, html: &str) -> bool {
    final_url.path().contains("/login")
        || html.contains("id=\"login_form\"")
        || html.contains("You must log in to continue")
}

fn extract_html_title(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    if let Ok(sel) = Selector::parse(r#"meta[property="og:title"]"#) {
        if let Some(el) = doc.select(&sel).next() {
            if let Some(c) = el.value().attr("content") {
                let t = c.trim();
                if !t.is_empty() {
                    return Some(t.to_string());
                }
            }
        }
    }
    if let Ok(sel) = Selector::parse("title") {
        if let Some(el) = doc.select(&sel).next() {
            let t: String = el.text().collect();
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn extract_og_image(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    let sel = Selector::parse(r#"meta[property="og:image"]"#).ok()?;
    doc.select(&sel)
        .next()
        .and_then(|el| el.value().attr("content"))
        .map(|s| s.to_string())
}
