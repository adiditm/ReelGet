use async_trait::async_trait;
use scraper::{Html, Selector};
use serde_json::Value;
use url::Url;

use crate::error::{ReelGetError, Result};
use crate::extractors::Extractor;
use crate::http::{browser_navigation_headers, HttpSession};
use crate::models::{MediaFormat, MediaInfo, MediaKind, Platform, StreamProtocol};
use crate::utils::find_json_object;

pub struct GenericExtractor;

#[async_trait]
impl Extractor for GenericExtractor {
    fn name(&self) -> &'static str {
        "generic"
    }

    fn matches(&self, _url: &Url) -> bool {
        true
    }

    async fn extract(&self, url: &Url, session: &HttpSession) -> Result<MediaInfo> {
        let headers = browser_navigation_headers();
        let (final_url, html) = session.get_text_with_final_url(url, &headers).await?;
        let doc = Html::parse_document(&html);

        let mut title = extract_meta(&doc, r#"meta[property="og:title"]"#)
            .or_else(|| extract_title(&doc))
            .unwrap_or_else(|| "Video".to_string());
        let thumbnail_url = extract_meta(&doc, r#"meta[property="og:image"]"#);

        let mut candidate_urls: Vec<String> = Vec::new();

        // 1. OpenGraph & Twitter meta tags
        for sel in [
            r#"meta[property="og:video:secure_url"]"#,
            r#"meta[property="og:video:url"]"#,
            r#"meta[property="og:video"]"#,
            r#"meta[name="twitter:player:stream"]"#,
        ] {
            if let Some(u) = extract_meta(&doc, sel) {
                push_resolved(&final_url, &u, &mut candidate_urls);
            }
        }

        // 2. JSON-LD VideoObject
        if let Ok(sel) = Selector::parse(r#"script[type="application/ld+json"]"#) {
            for el in doc.select(&sel) {
                let txt: String = el.text().collect();
                if let Ok(val) = serde_json::from_str::<Value>(txt.trim()) {
                    if let Some(obj) = find_json_object(&val, &|m| {
                        m.get("@type").and_then(|t| t.as_str()) == Some("VideoObject")
                            || m.get("contentUrl").is_some()
                    }) {
                        if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
                            if !name.trim().is_empty() {
                                title = name.trim().to_string();
                            }
                        }
                        if let Some(cu) = obj.get("contentUrl").and_then(|v| v.as_str()) {
                            push_resolved(&final_url, cu, &mut candidate_urls);
                        }
                    }
                }
            }
        }

        // 3. HTML5 <video> and <source> tags
        for sel_str in ["video[src]", "video source[src]", "source[type^=\"video/\"]"] {
            if let Ok(sel) = Selector::parse(sel_str) {
                for el in doc.select(&sel) {
                    if let Some(src) = el.value().attr("src") {
                        push_resolved(&final_url, src, &mut candidate_urls);
                    }
                }
            }
        }

        let mut formats = Vec::new();
        for (idx, media_url) in candidate_urls.into_iter().enumerate() {
            let lower = media_url.to_ascii_lowercase();
            let (protocol, ext) = if lower.contains(".m3u8") {
                (StreamProtocol::Hls, "mp4".to_string())
            } else if lower.contains(".webm") {
                (StreamProtocol::Direct, "webm".to_string())
            } else if lower.contains(".mov") {
                (StreamProtocol::Direct, "mov".to_string())
            } else {
                (StreamProtocol::Direct, "mp4".to_string())
            };

            formats.push(MediaFormat {
                format_id: format!("generic_{idx}"),
                url: media_url,
                protocol,
                kind: MediaKind::VideoAndAudio,
                ext,
                width: None,
                height: None,
                bitrate_bps: None,
                filesize_bytes: None,
                codecs: None,
                quality_label: Some("Extracted".to_string()),
                headers: vec![("Referer".to_string(), final_url.to_string())],
            });
        }

        if formats.is_empty() {
            return Err(ReelGetError::MediaNotFound(format!(
                "No video tags or OpenGraph video metadata found at {final_url}"
            )));
        }

        Ok(MediaInfo {
            id: final_url
                .host_str()
                .unwrap_or("generic")
                .replace('.', "_"),
            platform: Platform::Generic,
            webpage_url: final_url.to_string(),
            title,
            uploader: final_url.host_str().map(|s| s.to_string()),
            duration_secs: None,
            thumbnail_url,
            is_private_or_auth: false,
            formats,
        })
    }
}

fn push_resolved(base: &Url, raw: &str, out: &mut Vec<String>) {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with("blob:") || trimmed.starts_with("data:") {
        return;
    }
    if let Ok(resolved) = base.join(trimmed) {
        let s = resolved.to_string();
        if (s.starts_with("http://") || s.starts_with("https://")) && !out.contains(&s) {
            out.push(s);
        }
    }
}

fn extract_meta(doc: &Html, selector: &str) -> Option<String> {
    let sel = Selector::parse(selector).ok()?;
    doc.select(&sel)
        .next()
        .and_then(|el| el.value().attr("content"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn extract_title(doc: &Html) -> Option<String> {
    let sel = Selector::parse("title").ok()?;
    let t: String = doc.select(&sel).next()?.text().collect();
    let trimmed = t.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
