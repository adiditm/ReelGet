use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sha1::{Digest, Sha1};
use url::Url;

use crate::error::Result;

#[derive(Debug, Clone, Default)]
pub struct AuthConfig {
    pub cookies_file: Option<PathBuf>,
    pub raw_cookie: Option<String>,
    pub headers: Vec<(String, String)>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CookieEntry {
    pub domain: String,
    pub include_subdomains: bool,
    pub path: String,
    pub secure: bool,
    pub expires: u64,
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Default)]
pub struct NetscapeCookieJar {
    entries: Vec<CookieEntry>,
}

impl NetscapeCookieJar {
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(Self::parse(&content))
    }

    pub fn parse(content: &str) -> Self {
        let mut entries = Vec::new();
        for raw_line in content.lines() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }
            let effective = if let Some(rest) = line.strip_prefix("#HttpOnly_") {
                rest
            } else if line.starts_with('#') {
                continue;
            } else {
                line
            };

            let parts: Vec<&str> = effective.split('\t').collect();
            if parts.len() < 7 {
                continue;
            }

            let domain = parts[0].trim().to_ascii_lowercase();
            let include_subdomains = parts[1].trim().eq_ignore_ascii_case("TRUE");
            let path = parts[2].trim().to_string();
            let secure = parts[3].trim().eq_ignore_ascii_case("TRUE");
            let expires = parts[4].trim().parse::<u64>().unwrap_or(0);
            let name = parts[5].trim().to_string();
            let value = parts[6..].join("\t");

            if domain.is_empty() || name.is_empty() {
                continue;
            }

            entries.push(CookieEntry {
                domain,
                include_subdomains,
                path: if path.is_empty() {
                    "/".to_string()
                } else {
                    path
                },
                secure,
                expires,
                name,
                value,
            });
        }
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[CookieEntry] {
        &self.entries
    }

    pub fn matching_cookies(&self, url: &Url) -> Vec<&CookieEntry> {
        let Some(host) = url.host_str() else {
            return Vec::new();
        };
        let host_lower = host.to_ascii_lowercase();
        let req_path = url.path();
        let is_https = url.scheme() == "https";
        let now = current_epoch_secs();

        self.entries
            .iter()
            .filter(|c| {
                if c.secure && !is_https {
                    return false;
                }
                if c.expires != 0 && c.expires < now {
                    return false;
                }
                if !domain_matches(&host_lower, &c.domain, c.include_subdomains) {
                    return false;
                }
                req_path.starts_with(&c.path)
            })
            .collect()
    }

    pub fn cookie_header_for_url(&self, url: &Url) -> Option<String> {
        let matched = self.matching_cookies(url);
        if matched.is_empty() {
            return None;
        }
        let joined = matched
            .into_iter()
            .map(|c| format!("{}={}", c.name, c.value))
            .collect::<Vec<_>>()
            .join("; ");
        Some(joined)
    }

    pub fn get_cookie_value(&self, domain_suffix: &str, name: &str) -> Option<&str> {
        let suffix = domain_suffix.to_ascii_lowercase();
        let now = current_epoch_secs();
        self.entries
            .iter()
            .find(|c| {
                (c.expires == 0 || c.expires >= now)
                    && c.name == name
                    && (c.domain == suffix
                        || c.domain.trim_start_matches('.') == suffix.trim_start_matches('.')
                        || c.domain.ends_with(&format!(".{}", suffix.trim_start_matches('.'))))
            })
            .map(|c| c.value.as_str())
    }
}

fn domain_matches(host: &str, cookie_domain: &str, include_subdomains: bool) -> bool {
    let clean_cookie = cookie_domain.trim_start_matches('.');
    if host == clean_cookie {
        return true;
    }
    if include_subdomains || cookie_domain.starts_with('.') {
        return host.ends_with(&format!(".{clean_cookie}"));
    }
    false
}

fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Parse an inline `Cookie` header string (`"a=1; b=2"`) and look up a named cookie.
pub fn find_cookie_in_raw(raw_cookie: &str, name: &str) -> Option<String> {
    for part in raw_cookie.split(';') {
        let trimmed = part.trim();
        if let Some((k, v)) = trimmed.split_once('=') {
            if k.trim() == name {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// Compute YouTube's `SAPISIDHASH` Authorization header value for authenticated InnerTube calls.
pub fn compute_sapisidhash(sapisid: &str, origin: &str) -> String {
    let ts = current_epoch_secs();
    let input = format!("{ts} {sapisid} {origin}");
    let mut hasher = Sha1::new();
    hasher.update(input.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("SAPISIDHASH {ts}_{hex}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_netscape_cookie_parse_and_match() {
        let sample = "\
# Netscape HTTP Cookie File
.instagram.com\tTRUE\t/\tTRUE\t0\tsessionid\tabc123
#HttpOnly_.facebook.com\tTRUE\t/\tTRUE\t0\txs\txyz789
";
        let jar = NetscapeCookieJar::parse(sample);
        assert_eq!(jar.entries().len(), 2);

        let ig_url = Url::parse("https://www.instagram.com/reel/ChxvmGJLfsW/").unwrap();
        assert_eq!(
            jar.cookie_header_for_url(&ig_url).as_deref(),
            Some("sessionid=abc123")
        );
        assert_eq!(jar.get_cookie_value("instagram.com", "sessionid"), Some("abc123"));
        assert_eq!(jar.get_cookie_value("facebook.com", "xs"), Some("xyz789"));
    }

    #[test]
    fn test_sapisidhash_format() {
        let h = compute_sapisidhash("test_sapisid", "https://www.youtube.com");
        assert!(h.starts_with("SAPISIDHASH "));
        assert!(h.contains('_'));
    }
}
