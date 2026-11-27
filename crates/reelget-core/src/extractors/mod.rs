pub mod direct;
pub mod facebook;
pub mod generic;
pub mod instagram;
pub mod threads;
pub mod tiktok;
pub mod twitter;
pub mod youtube;

use async_trait::async_trait;
use url::Url;

use crate::error::{ReelGetError, Result};
use crate::http::HttpSession;
use crate::models::MediaInfo;

#[async_trait]
pub trait Extractor: Send + Sync {
    fn name(&self) -> &'static str;
    fn matches(&self, url: &Url) -> bool;
    async fn extract(&self, url: &Url, session: &HttpSession) -> Result<MediaInfo>;
}

pub struct ExtractorRegistry {
    extractors: Vec<Box<dyn Extractor>>,
}

impl Default for ExtractorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtractorRegistry {
    pub fn new() -> Self {
        Self {
            extractors: vec![
                Box::new(facebook::FacebookExtractor),
                Box::new(instagram::InstagramExtractor),
                Box::new(threads::ThreadsExtractor),
                Box::new(tiktok::TikTokExtractor),
                Box::new(twitter::TwitterXExtractor),
                Box::new(youtube::YouTubeExtractor),
                Box::new(direct::DirectExtractor),
                Box::new(generic::GenericExtractor),
            ],
        }
    }

    pub async fn extract(&self, raw_url: &str, session: &HttpSession) -> Result<MediaInfo> {
        let url = Url::parse(raw_url.trim())
            .map_err(|e| ReelGetError::InvalidUrl(format!("{raw_url}: {e}")))?;

        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(ReelGetError::InvalidUrl(format!(
                "Unsupported URL scheme '{}'",
                url.scheme()
            )));
        }

        for extractor in &self.extractors {
            if extractor.matches(&url) {
                tracing::debug!(extractor = extractor.name(), url = %url, "Matched extractor");
                match extractor.extract(&url, session).await {
                    Ok(info) if !info.formats.is_empty() => return Ok(info),
                    Ok(_) => {
                        if extractor.name() != "direct" && extractor.name() != "generic" {
                            return Err(ReelGetError::MediaNotFound(url.to_string()));
                        }
                    }
                    Err(e) => {
                        // If direct extractor didn't match by Content-Type, fall through to generic
                        if extractor.name() == "direct" {
                            tracing::debug!(error = %e, "Direct probe fell through to generic");
                            continue;
                        }
                        return Err(e);
                    }
                }
            }
        }

        Err(ReelGetError::UnsupportedPlatform(raw_url.to_string()))
    }
}
