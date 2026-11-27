use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use reqwest::cookie::Jar;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, USER_AGENT};
use reqwest::{Client, Response};
use serde_json::Value;
use url::Url;

use crate::auth::{find_cookie_in_raw, AuthConfig, NetscapeCookieJar};
use crate::error::{ReelGetError, Result};

pub const DESKTOP_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
pub const MOBILE_SAFARI_USER_AGENT: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1";

#[derive(Clone)]
pub struct HttpSession {
    client: Client,
    cookie_jar: Arc<Jar>,
    netscape_jar: NetscapeCookieJar,
    auth: AuthConfig,
    retries: usize,
}

impl HttpSession {
    pub fn new(auth: AuthConfig, retries: usize) -> Result<Self> {
        let netscape_jar = match &auth.cookies_file {
            Some(path) => NetscapeCookieJar::from_file(path)?,
            None => NetscapeCookieJar::default(),
        };

        let cookie_jar = Arc::new(Jar::default());
        for entry in netscape_jar.entries() {
            let clean_domain = entry.domain.trim_start_matches('.');
            let scheme = if entry.secure { "https" } else { "http" };
            if let Ok(url) = Url::parse(&format!("{scheme}://{clean_domain}/")) {
                let cookie_str = format!(
                    "{}={}; Domain={}; Path={}",
                    entry.name, entry.value, entry.domain, entry.path
                );
                cookie_jar.add_cookie_str(&cookie_str, &url);
            }
        }

        let client = Client::builder()
            .cookie_provider(Arc::clone(&cookie_jar))
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::limited(20))
            .build()?;

        Ok(Self {
            client,
            cookie_jar,
            netscape_jar,
            auth,
            retries,
        })
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn auth(&self) -> &AuthConfig {
        &self.auth
    }

    pub fn user_agent(&self) -> &str {
        self.auth
            .user_agent
            .as_deref()
            .unwrap_or(DESKTOP_USER_AGENT)
    }

    pub fn get_cookie_value(&self, domain_suffix: &str, name: &str) -> Option<String> {
        if let Some(v) = self.netscape_jar.get_cookie_value(domain_suffix, name) {
            return Some(v.to_string());
        }
        if let Some(raw) = &self.auth.raw_cookie {
            if let Some(v) = find_cookie_in_raw(raw, name) {
                return Some(v);
            }
        }
        None
    }

    pub fn seed_raw_cookies_for_url(&self, url: &Url) {
        if let Some(raw) = &self.auth.raw_cookie {
            for part in raw.split(';') {
                let trimmed = part.trim();
                if !trimmed.is_empty() && trimmed.contains('=') {
                    self.cookie_jar.add_cookie_str(trimmed, url);
                }
            }
        }
    }

    pub fn build_headers(&self, url: &Url, extra: &[(String, String)]) -> HeaderMap {
        self.seed_raw_cookies_for_url(url);

        let mut map = HeaderMap::new();
        if let Ok(ua) = HeaderValue::from_str(self.user_agent()) {
            map.insert(USER_AGENT, ua);
        }

        for (k, v) in &self.auth.headers {
            if let (Ok(name), Ok(val)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            ) {
                map.insert(name, val);
            }
        }

        for (k, v) in extra {
            if let (Ok(name), Ok(val)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            ) {
                map.insert(name, val);
            }
        }

        map
    }

    pub async fn get_response(&self, url: &Url, extra: &[(String, String)]) -> Result<Response> {
        let mut last_err: Option<ReelGetError> = None;
        for attempt in 0..=self.retries {
            let headers = self.build_headers(url, extra);
            match self.client.get(url.clone()).headers(headers).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() || status.as_u16() == 206 {
                        return Ok(resp);
                    }
                    if (status.as_u16() == 429 || status.is_server_error()) && attempt < self.retries
                    {
                        tokio::time::sleep(Duration::from_millis(400 * (attempt as u64 + 1))).await;
                        continue;
                    }
                    return Err(ReelGetError::HttpStatus {
                        status: status.as_u16(),
                        url: url.to_string(),
                    });
                }
                Err(e) => {
                    last_err = Some(ReelGetError::Http(e));
                    if attempt < self.retries {
                        tokio::time::sleep(Duration::from_millis(400 * (attempt as u64 + 1))).await;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| ReelGetError::InvalidUrl(url.to_string())))
    }

    pub async fn get_text_with_final_url(
        &self,
        url: &Url,
        extra: &[(String, String)],
    ) -> Result<(Url, String)> {
        let resp = self.get_response(url, extra).await?;
        let final_url = resp.url().clone();
        let text = resp.text().await?;
        Ok((final_url, text))
    }

    pub async fn get_bytes(&self, url: &Url, extra: &[(String, String)]) -> Result<Bytes> {
        let resp = self.get_response(url, extra).await?;
        Ok(resp.bytes().await?)
    }

    pub async fn post_json(
        &self,
        url: &Url,
        body: &Value,
        extra: &[(String, String)],
    ) -> Result<Value> {
        let headers = self.build_headers(url, extra);
        let resp = self
            .client
            .post(url.clone())
            .headers(headers)
            .json(body)
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ReelGetError::HttpStatus {
                status: status.as_u16(),
                url: url.to_string(),
            });
        }
        let val = resp.json::<Value>().await?;
        Ok(val)
    }

    pub async fn head_or_probe(&self, url: &Url, extra: &[(String, String)]) -> Result<Response> {
        let headers = self.build_headers(url, extra);
        let resp = self.client.head(url.clone()).headers(headers).send().await?;
        Ok(resp)
    }
}

pub fn browser_navigation_headers() -> Vec<(String, String)> {
    vec![
        (
            "Accept".to_string(),
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8"
                .to_string(),
        ),
        ("Accept-Language".to_string(), "en-US,en;q=0.9".to_string()),
        ("Sec-Fetch-Dest".to_string(), "document".to_string()),
        ("Sec-Fetch-Mode".to_string(), "navigate".to_string()),
        ("Sec-Fetch-Site".to_string(), "none".to_string()),
        ("Sec-Fetch-User".to_string(), "?1".to_string()),
        ("Upgrade-Insecure-Requests".to_string(), "1".to_string()),
    ]
}
