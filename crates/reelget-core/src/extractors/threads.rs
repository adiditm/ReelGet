use async_trait::async_trait;
use scraper::{Html, Selector};
use serde_json::Value;
use url::Url;

use crate::error::{ReelGetError, Result};
use crate::extractors::instagram::push_video_versions;
use crate::extractors::Extractor;
use crate::http::{browser_navigation_headers, HttpSession};
use crate::models::{MediaFormat, MediaInfo, Platform};
use crate::utils::{extract_json_scripts, find_json_object, unescape_url};

pub struct ThreadsExtractor;

#[async_trait]
impl Extractor for ThreadsExtractor {
    fn name(&self) -> &'static str {
        "threads"
    }

    fn matches(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        let h = host.to_ascii_lowercase();
        h == "threads.net"
            || h.ends_with(".threads.net")
            || h == "threads.com"
            || h.ends_with(".threads.com")
    }

    async fn extract(&self, url: &Url, session: &HttpSession) -> Result<MediaInfo> {
        let post_code = extract_threads_code(url);
        let has_session = session.get_cookie_value("threads.com", "sessionid").is_some()
            || session.get_cookie_value("threads.net", "sessionid").is_some()
            || session.get_cookie_value("instagram.com", "sessionid").is_some();

        let headers = browser_navigation_headers();
        let (final_url, html) = session.get_text_with_final_url(url, &headers).await?;

        let mut formats: Vec<MediaFormat> = Vec::new();
        let mut title = extract_og_meta(&html, "og:title").unwrap_or_else(|| {
            post_code
                .as_ref()
                .map(|c| format!("Threads Post {c}"))
                .unwrap_or_else(|| "Threads Video".to_string())
        });
        let mut uploader: Option<String> = None;
        let mut thumbnail_url = extract_og_meta(&html, "og:image");

        let scripts = extract_json_scripts(&html);
        for script_val in &scripts {
            let matched = if let Some(ref code) = post_code {
                find_json_object(script_val, &|map| {
                    map.get("code").and_then(|v| v.as_str()) == Some(code.as_str())
                        && (map.get("video_versions").is_some()
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
                extract_threads_obj(
                    obj,
                    &mut formats,
                    &mut title,
                    &mut uploader,
                    &mut thumbnail_url,
                );
                if !formats.is_empty() {
                    break;
                }
            }
        }

        if formats.is_empty() {
            if final_url.path().contains("/login") || !has_session {
                return Err(ReelGetError::AuthenticationRequired {
                    platform: "Threads".to_string(),
                    hint: "This Threads post may be private or contain no video. For private posts, supply Threads/Instagram cookies (sessionid) via --cookies or --cookie.".to_string(),
                });
            }
            return Err(ReelGetError::MediaNotFound(format!(
                "No video stream found in Threads post {final_url}"
            )));
        }

        Ok(MediaInfo {
            id: post_code.unwrap_or_else(|| "threads_post".to_string()),
            platform: Platform::Threads,
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

fn extract_threads_obj(
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
        push_video_versions(vv, formats, "https://www.threads.com/");
    }

    if formats.is_empty() {
        if let Some(carousel) = obj.get("carousel_media").and_then(|v| v.as_array()) {
            for item in carousel {
                if let Some(vv) = item.get("video_versions").and_then(|v| v.as_array()) {
                    push_video_versions(vv, formats, "https://www.threads.com/");
                    if !formats.is_empty() {
                        break;
                    }
                }
            }
        }
    }
}

fn extract_threads_code(url: &Url) -> Option<String> {
    let segs: Vec<&str> = url.path_segments()?.collect();
    for (i, seg) in segs.iter().enumerate() {
        if (*seg == "post" || *seg == "t") && i + 1 < segs.len() {
            let code = segs[i + 1].trim_matches('/');
            if !code.is_empty() {
                return Some(code.to_string());
            }
        }
    }
    None
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
