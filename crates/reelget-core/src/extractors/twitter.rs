use async_trait::async_trait;
use regex::Regex;
use serde_json::Value;
use url::Url;

use crate::error::{ReelGetError, Result};
use crate::extractors::Extractor;
use crate::http::HttpSession;
use crate::models::{MediaFormat, MediaInfo, MediaKind, Platform, StreamProtocol};
use crate::utils::find_json_object;

const X_PUBLIC_BEARER: &str = "AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs%3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA";

pub struct TwitterXExtractor;

#[async_trait]
impl Extractor for TwitterXExtractor {
    fn name(&self) -> &'static str {
        "twitter_x"
    }

    fn matches(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        let h = host.to_ascii_lowercase();
        h == "x.com"
            || h.ends_with(".x.com")
            || h == "twitter.com"
            || h.ends_with(".twitter.com")
            || h == "fxtwitter.com"
            || h == "vxtwitter.com"
    }

    async fn extract(&self, url: &Url, session: &HttpSession) -> Result<MediaInfo> {
        let tweet_id = extract_tweet_id(url).ok_or_else(|| {
            ReelGetError::InvalidUrl(format!("Could not extract tweet status ID from {url}"))
        })?;

        let auth_token = session
            .get_cookie_value("x.com", "auth_token")
            .or_else(|| session.get_cookie_value("twitter.com", "auth_token"));
        let ct0 = session
            .get_cookie_value("x.com", "ct0")
            .or_else(|| session.get_cookie_value("twitter.com", "ct0"));
        let has_auth = auth_token.is_some() && ct0.is_some();

        // 1. If authenticated cookies are present, try X GraphQL first (supports protected/private tweets)
        if let (Some(_at), Some(csrf)) = (&auth_token, &ct0) {
            if let Ok(info) = fetch_x_graphql(&tweet_id, url, csrf, session).await {
                return Ok(info);
            }
        }

        // 2. Public syndication API (fast, reliable, no auth needed for public tweets)
        match fetch_x_syndication(&tweet_id, url, session).await {
            Ok(info) => Ok(info),
            Err(e) => {
                if !has_auth {
                    if matches!(e, ReelGetError::MediaNotFound(_) | ReelGetError::HttpStatus { status: 404 | 403 | 401, .. }) {
                        return Err(ReelGetError::AuthenticationRequired {
                            platform: "X (Twitter)".to_string(),
                            hint: "Tweet may be from a protected account, age-restricted, or deleted. Supply X cookies (auth_token, ct0) via --cookies or --cookie.".to_string(),
                        });
                    }
                }
                Err(e)
            }
        }
    }
}

async fn fetch_x_syndication(
    tweet_id: &str,
    orig_url: &Url,
    session: &HttpSession,
) -> Result<MediaInfo> {
    let token = compute_syndication_token(tweet_id);
    let api_url = Url::parse(&format!(
        "https://cdn.syndication.twimg.com/tweet-result?id={tweet_id}&token={token}"
    ))
    .map_err(|e| ReelGetError::InvalidUrl(e.to_string()))?;

    let (_, body) = session.get_text_with_final_url(&api_url, &[]).await?;
    let json: Value = serde_json::from_str(&body)?;

    if json.get("__typename").and_then(|v| v.as_str()) == Some("TweetTombstone") {
        return Err(ReelGetError::AuthenticationRequired {
            platform: "X (Twitter)".to_string(),
            hint: "Tweet is protected or age-restricted. Supply X cookies (auth_token, ct0) via --cookies or --cookie.".to_string(),
        });
    }

    let title = json
        .get("text")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("X Post {tweet_id}"));

    let uploader = json
        .get("user")
        .and_then(|u| u.get("screen_name").or_else(|| u.get("name")))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut formats = Vec::new();
    let mut thumbnail_url = None;

    if let Some(media_details) = json.get("mediaDetails").and_then(|v| v.as_array()) {
        for media in media_details {
            if thumbnail_url.is_none() {
                thumbnail_url = media
                    .get("media_url_https")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
            if let Some(variants) = media
                .get("video_info")
                .and_then(|vi| vi.get("variants"))
                .and_then(|v| v.as_array())
            {
                collect_x_variants(variants, &mut formats);
            }
        }
    }

    if formats.is_empty() {
        if let Some(variants) = json
            .get("video")
            .and_then(|v| v.get("variants"))
            .and_then(|v| v.as_array())
        {
            collect_x_variants(variants, &mut formats);
        }
    }

    if formats.is_empty() {
        return Err(ReelGetError::MediaNotFound(format!(
            "No video found in X post {tweet_id}"
        )));
    }

    Ok(MediaInfo {
        id: tweet_id.to_string(),
        platform: Platform::TwitterX,
        webpage_url: orig_url.to_string(),
        title,
        uploader,
        duration_secs: None,
        thumbnail_url,
        is_private_or_auth: false,
        formats,
    })
}

async fn fetch_x_graphql(
    tweet_id: &str,
    orig_url: &Url,
    ct0: &str,
    session: &HttpSession,
) -> Result<MediaInfo> {
    let variables = serde_json::json!({
        "tweetId": tweet_id,
        "withCommunity": false,
        "includePromotedContent": false,
        "withVoice": false
    });
    let features = serde_json::json!({
        "creator_subscriptions_tweet_preview_api_enabled": true,
        "tweetypie_unmention_optimization_enabled": true,
        "responsive_web_edit_tweet_api_enabled": true,
        "graphql_is_translatable_rweb_tweet_is_translatable_enabled": true,
        "view_counts_everywhere_api_enabled": true,
        "longform_notetweets_consumption_enabled": true,
        "responsive_web_twitter_article_tweet_consumption_enabled": false,
        "tweet_awards_web_tipping_enabled": false,
        "freedom_of_speech_not_reach_fetch_enabled": true,
        "standardized_nudges_misinfo": true,
        "tweet_with_visibility_results_prefer_gql_limited_actions_policy_enabled": true,
        "longform_notetweets_rich_text_read_enabled": true,
        "longform_notetweets_inline_media_enabled": true,
        "responsive_web_graphql_exclude_directive_enabled": true,
        "verified_phone_label_enabled": false,
        "responsive_web_media_download_video_enabled": true,
        "responsive_web_graphql_skip_user_profile_image_extensions_enabled": false,
        "responsive_web_graphql_timeline_navigation_enabled": true,
        "responsive_web_enhance_cards_enabled": false
    });

    let mut api_url = Url::parse("https://x.com/i/api/graphql/0hWvDhmW8YQ-S_ib3azIrw/TweetResultByRestId")
        .map_err(|e| ReelGetError::InvalidUrl(e.to_string()))?;
    api_url
        .query_pairs_mut()
        .append_pair("variables", &variables.to_string())
        .append_pair("features", &features.to_string());

    let extra = vec![
        ("Authorization".to_string(), format!("Bearer {X_PUBLIC_BEARER}")),
        ("x-csrf-token".to_string(), ct0.to_string()),
        ("x-twitter-auth-type".to_string(), "OAuth2Session".to_string()),
        ("x-twitter-active-user".to_string(), "yes".to_string()),
        ("Referer".to_string(), "https://x.com/".to_string()),
    ];

    let (_, body) = session.get_text_with_final_url(&api_url, &extra).await?;
    let json: Value = serde_json::from_str(&body)?;

    let mut formats = Vec::new();
    if let Some(vi_obj) = find_json_object(&json, &|map| {
        map.get("variants")
            .and_then(|v| v.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false)
    }) {
        if let Some(variants) = vi_obj.get("variants").and_then(|v| v.as_array()) {
            collect_x_variants(variants, &mut formats);
        }
    }

    if formats.is_empty() {
        return Err(ReelGetError::MediaNotFound(format!(
            "No video variants in X GraphQL response for {tweet_id}"
        )));
    }

    let title = find_json_object(&json, &|map| map.get("full_text").is_some())
        .and_then(|m| m.get("full_text"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| format!("X Post {tweet_id}"));

    let uploader = find_json_object(&json, &|map| map.get("screen_name").is_some())
        .and_then(|m| m.get("screen_name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(MediaInfo {
        id: tweet_id.to_string(),
        platform: Platform::TwitterX,
        webpage_url: orig_url.to_string(),
        title,
        uploader,
        duration_secs: None,
        thumbnail_url: None,
        is_private_or_auth: true,
        formats,
    })
}

fn collect_x_variants(variants: &[Value], formats: &mut Vec<MediaFormat>) {
    let dim_re = Regex::new(r"/(\d+)x(\d+)/").ok();

    for (idx, v) in variants.iter().enumerate() {
        let Some(url) = v
            .get("url")
            .or_else(|| v.get("src"))
            .and_then(|u| u.as_str())
        else {
            continue;
        };
        let content_type = v
            .get("content_type")
            .or_else(|| v.get("type"))
            .and_then(|c| c.as_str())
            .unwrap_or("");

        let (protocol, ext) = if content_type.contains("mpegURL") || url.contains(".m3u8") {
            (StreamProtocol::Hls, "mp4".to_string())
        } else if content_type.contains("mp4") || url.contains(".mp4") {
            (StreamProtocol::Direct, "mp4".to_string())
        } else {
            continue;
        };

        let bitrate_bps = v.get("bitrate").and_then(|b| b.as_u64());
        let (width, height) = dim_re
            .as_ref()
            .and_then(|re| re.captures(url))
            .map(|cap| {
                (
                    cap[1].parse::<u32>().ok(),
                    cap[2].parse::<u32>().ok(),
                )
            })
            .unwrap_or((None, None));

        let label = match (width, height, bitrate_bps) {
            (Some(w), Some(h), _) => format!("{w}x{h}"),
            (_, _, Some(br)) => format!("{}kbps", br / 1000),
            _ => if protocol == StreamProtocol::Hls {
                "HLS".to_string()
            } else {
                format!("variant_{idx}")
            },
        };

        if !formats.iter().any(|f| f.url == url) {
            formats.push(MediaFormat {
                format_id: format!("x_{idx}"),
                url: url.to_string(),
                protocol,
                kind: MediaKind::VideoAndAudio,
                ext,
                width,
                height,
                bitrate_bps,
                filesize_bytes: None,
                codecs: None,
                quality_label: Some(label),
                headers: vec![("Referer".to_string(), "https://x.com/".to_string())],
            });
        }
    }
}

fn extract_tweet_id(url: &Url) -> Option<String> {
    let segs: Vec<&str> = url.path_segments()?.collect();
    for (i, seg) in segs.iter().enumerate() {
        if (*seg == "status" || *seg == "statuses") && i + 1 < segs.len() {
            let id = segs[i + 1].trim_matches('/');
            if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn compute_syndication_token(id: &str) -> String {
    if let Ok(num) = id.parse::<f64>() {
        let val = (num / 1e15) * std::f64::consts::PI;
        let s = format!("{val:.12}");
        let cleaned: String = s
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(8)
            .collect();
        if !cleaned.is_empty() {
            return cleaned;
        }
    }
    "4".to_string()
}
