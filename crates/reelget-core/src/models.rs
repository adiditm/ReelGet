use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::auth::AuthConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Facebook,
    Instagram,
    TikTok,
    YouTube,
    TwitterX,
    Threads,
    Direct,
    Generic,
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Platform::Facebook => "Facebook",
            Platform::Instagram => "Instagram",
            Platform::TikTok => "TikTok",
            Platform::YouTube => "YouTube",
            Platform::TwitterX => "X (Twitter)",
            Platform::Threads => "Threads",
            Platform::Direct => "Direct",
            Platform::Generic => "Generic",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamProtocol {
    Direct,
    Hls,
    Dash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    VideoAndAudio,
    VideoOnly,
    AudioOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaFormat {
    pub format_id: String,
    pub url: String,
    pub protocol: StreamProtocol,
    pub kind: MediaKind,
    pub ext: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bitrate_bps: Option<u64>,
    pub filesize_bytes: Option<u64>,
    pub codecs: Option<String>,
    pub quality_label: Option<String>,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

impl MediaFormat {
    pub fn resolution_score(&self) -> u64 {
        let h = self.height.unwrap_or(0) as u64;
        let w = self.width.unwrap_or(0) as u64;
        let br = self.bitrate_bps.unwrap_or(0);
        (h * 1_000_000_000) + (w * 1_000_000) + (br / 1000)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaInfo {
    pub id: String,
    pub platform: Platform,
    pub webpage_url: String,
    pub title: String,
    pub uploader: Option<String>,
    pub duration_secs: Option<f64>,
    pub thumbnail_url: Option<String>,
    pub is_private_or_auth: bool,
    pub formats: Vec<MediaFormat>,
}

impl MediaInfo {
    /// Select the best matching video (or audio) format and optional separate audio format
    /// when adaptive streams are chosen and `can_mux` (e.g. ffmpeg available) is true.
    pub fn select_formats(
        &self,
        quality: &QualityPreference,
        can_mux: bool,
    ) -> Option<(MediaFormat, Option<MediaFormat>)> {
        if self.formats.is_empty() {
            return None;
        }

        match quality {
            QualityPreference::FormatId(id) => {
                let fmt = self.formats.iter().find(|f| f.format_id == *id)?.clone();
                let audio = if fmt.kind == MediaKind::VideoOnly && can_mux {
                    self.best_audio_format()
                } else {
                    None
                };
                Some((fmt, audio))
            }
            QualityPreference::AudioOnly => {
                if let Some(audio) = self.best_audio_format() {
                    return Some((audio, None));
                }
                self.best_muxed_format(None).map(|f| (f, None))
            }
            QualityPreference::Best => self.select_best_with_cap(None, can_mux),
            QualityPreference::MaxHeight(max_h) => self.select_best_with_cap(Some(*max_h), can_mux),
        }
    }

    fn select_best_with_cap(
        &self,
        max_height: Option<u32>,
        can_mux: bool,
    ) -> Option<(MediaFormat, Option<MediaFormat>)> {
        let best_muxed = self.best_muxed_format(max_height);
        let best_video_only = self.best_video_only_format(max_height);
        let best_audio = self.best_audio_format();

        if can_mux {
            if let (Some(v), Some(a)) = (&best_video_only, &best_audio) {
                let muxed_score = best_muxed
                    .as_ref()
                    .map(|m| m.resolution_score())
                    .unwrap_or(0);
                if v.resolution_score() > muxed_score {
                    return Some((v.clone(), Some(a.clone())));
                }
            }
        }

        if let Some(m) = best_muxed {
            return Some((m, None));
        }

        if let Some(v) = best_video_only {
            let audio = if can_mux { best_audio } else { None };
            return Some((v, audio));
        }

        self.formats.first().cloned().map(|f| (f, None))
    }

    fn best_muxed_format(&self, max_height: Option<u32>) -> Option<MediaFormat> {
        let mut candidates: Vec<&MediaFormat> = self
            .formats
            .iter()
            .filter(|f| f.kind == MediaKind::VideoAndAudio)
            .collect();

        if let Some(cap) = max_height {
            let capped: Vec<&MediaFormat> = candidates
                .iter()
                .copied()
                .filter(|f| f.height.map(|h| h <= cap).unwrap_or(true))
                .collect();
            if !capped.is_empty() {
                candidates = capped;
            }
        }

        candidates
            .into_iter()
            .max_by_key(|f| {
                let mp4_bonus = if f.ext == "mp4" { 10 } else { 0 };
                let direct_bonus = if f.protocol == StreamProtocol::Direct {
                    5
                } else {
                    0
                };
                (f.resolution_score(), mp4_bonus + direct_bonus)
            })
            .cloned()
    }

    fn best_video_only_format(&self, max_height: Option<u32>) -> Option<MediaFormat> {
        let mut candidates: Vec<&MediaFormat> = self
            .formats
            .iter()
            .filter(|f| f.kind == MediaKind::VideoOnly)
            .collect();

        if let Some(cap) = max_height {
            let capped: Vec<&MediaFormat> = candidates
                .iter()
                .copied()
                .filter(|f| f.height.map(|h| h <= cap).unwrap_or(true))
                .collect();
            if !capped.is_empty() {
                candidates = capped;
            }
        }

        candidates
            .into_iter()
            .max_by_key(|f| {
                let mp4_bonus = if f.ext == "mp4" { 10 } else { 0 };
                (f.resolution_score(), mp4_bonus)
            })
            .cloned()
    }

    fn best_audio_format(&self) -> Option<MediaFormat> {
        self.formats
            .iter()
            .filter(|f| f.kind == MediaKind::AudioOnly)
            .max_by_key(|f| {
                let m4a_bonus = if f.ext == "m4a" || f.ext == "mp4" {
                    10
                } else {
                    0
                };
                (f.bitrate_bps.unwrap_or(0), m4a_bonus)
            })
            .cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QualityPreference {
    #[default]
    Best,
    MaxHeight(u32),
    AudioOnly,
    FormatId(String),
}

impl FromStr for QualityPreference {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        let lower = trimmed.to_ascii_lowercase();
        if lower.is_empty() || lower == "best" || lower == "highest" {
            return Ok(QualityPreference::Best);
        }
        if lower == "audio" || lower == "audio_only" || lower == "mp3" || lower == "m4a" {
            return Ok(QualityPreference::AudioOnly);
        }
        let digits = lower.trim_end_matches('p');
        if let Ok(h) = digits.parse::<u32>() {
            if (144..=4320).contains(&h) {
                return Ok(QualityPreference::MaxHeight(h));
            }
        }
        Ok(QualityPreference::FormatId(trimmed.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipRange {
    pub start_secs: Option<f64>,
    pub end_secs: Option<f64>,
}

impl ClipRange {
    pub fn new(start_secs: Option<f64>, end_secs: Option<f64>) -> Result<Self, String> {
        let range = Self {
            start_secs,
            end_secs,
        };
        range.validate()?;
        Ok(range)
    }

    pub fn from_hms(
        start: Option<(u32, u32, u32)>,
        end: Option<(u32, u32, u32)>,
    ) -> Result<Self, String> {
        let start_secs = start.map(|(h, m, s)| (h as f64) * 3600.0 + (m as f64) * 60.0 + (s as f64));
        let end_secs = end.map(|(h, m, s)| (h as f64) * 3600.0 + (m as f64) * 60.0 + (s as f64));
        Self::new(start_secs, end_secs)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.start_secs.is_none() && self.end_secs.is_none() {
            return Err("At least one of start or end time must be specified".to_string());
        }
        if let Some(s) = self.start_secs {
            if !s.is_finite() || s < 0.0 {
                return Err("Start time must be >= 0".to_string());
            }
        }
        if let Some(e) = self.end_secs {
            if !e.is_finite() || e <= 0.0 {
                return Err("End time must be > 0".to_string());
            }
        }
        if let (Some(s), Some(e)) = (self.start_secs, self.end_secs) {
            if e <= s {
                return Err(format!(
                    "End time ({}) must be greater than start time ({})",
                    Self::format_hms(e),
                    Self::format_hms(s)
                ));
            }
        }
        Ok(())
    }

    /// Parse `"HH:MM:SS"`, `"MM:SS"`, or `"SS"` (with optional fractional seconds).
    pub fn parse_timestamp(raw: &str) -> Result<f64, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("Timestamp cannot be empty".to_string());
        }
        let parts: Vec<&str> = trimmed.split(':').collect();
        let secs = match parts.len() {
            1 => parts[0]
                .parse::<f64>()
                .map_err(|_| format!("Invalid seconds '{trimmed}'"))?,
            2 => {
                let m = parts[0]
                    .parse::<u64>()
                    .map_err(|_| format!("Invalid minutes in '{trimmed}'"))?;
                let s = parts[1]
                    .parse::<f64>()
                    .map_err(|_| format!("Invalid seconds in '{trimmed}'"))?;
                if s >= 60.0 || s < 0.0 {
                    return Err(format!("Seconds must be 0..60 in '{trimmed}'"));
                }
                (m as f64) * 60.0 + s
            }
            3 => {
                let h = parts[0]
                    .parse::<u64>()
                    .map_err(|_| format!("Invalid hours in '{trimmed}'"))?;
                let m = parts[1]
                    .parse::<u64>()
                    .map_err(|_| format!("Invalid minutes in '{trimmed}'"))?;
                let s = parts[2]
                    .parse::<f64>()
                    .map_err(|_| format!("Invalid seconds in '{trimmed}'"))?;
                if m >= 60 || s >= 60.0 || s < 0.0 {
                    return Err(format!("Minutes/seconds out of range in '{trimmed}'"));
                }
                (h as f64) * 3600.0 + (m as f64) * 60.0 + s
            }
            _ => {
                return Err(format!(
                    "Invalid timestamp '{trimmed}', expected HH:MM:SS, MM:SS, or SS"
                ))
            }
        };
        if !secs.is_finite() || secs < 0.0 {
            return Err(format!("Invalid timestamp '{trimmed}'"));
        }
        Ok(secs)
    }

    pub fn format_hms(secs: f64) -> String {
        let total = secs.max(0.0).round() as u64;
        let h = total / 3600;
        let m = (total % 3600) / 60;
        let s = total % 60;
        format!("{h:02}:{m:02}:{s:02}")
    }

    pub fn filename_tag(&self) -> String {
        let start = Self::format_hms(self.start_secs.unwrap_or(0.0)).replace(':', "-");
        let end = match self.end_secs {
            Some(e) => Self::format_hms(e).replace(':', "-"),
            None => "end".to_string(),
        };
        format!("{start}_to_{end}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    #[default]
    Original,
    Mp3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mp3Bitrate {
    Kbps64,
    Kbps96,
    Kbps128,
    #[default]
    Kbps192,
    Kbps256,
    Kbps320,
}

impl Mp3Bitrate {
    pub const ALL: [Mp3Bitrate; 6] = [
        Mp3Bitrate::Kbps64,
        Mp3Bitrate::Kbps96,
        Mp3Bitrate::Kbps128,
        Mp3Bitrate::Kbps192,
        Mp3Bitrate::Kbps256,
        Mp3Bitrate::Kbps320,
    ];

    pub fn kbps(self) -> u32 {
        match self {
            Mp3Bitrate::Kbps64 => 64,
            Mp3Bitrate::Kbps96 => 96,
            Mp3Bitrate::Kbps128 => 128,
            Mp3Bitrate::Kbps192 => 192,
            Mp3Bitrate::Kbps256 => 256,
            Mp3Bitrate::Kbps320 => 320,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Mp3Bitrate::Kbps64 => "64 kbps",
            Mp3Bitrate::Kbps96 => "96 kbps",
            Mp3Bitrate::Kbps128 => "128 kbps",
            Mp3Bitrate::Kbps192 => "192 kbps",
            Mp3Bitrate::Kbps256 => "256 kbps",
            Mp3Bitrate::Kbps320 => "320 kbps",
        }
    }
}

impl FromStr for Mp3Bitrate {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let cleaned = s
            .trim()
            .to_ascii_lowercase()
            .trim_end_matches("bps")
            .trim_end_matches('k')
            .trim()
            .to_string();
        match cleaned.as_str() {
            "64" => Ok(Mp3Bitrate::Kbps64),
            "96" => Ok(Mp3Bitrate::Kbps96),
            "128" => Ok(Mp3Bitrate::Kbps128),
            "192" => Ok(Mp3Bitrate::Kbps192),
            "256" => Ok(Mp3Bitrate::Kbps256),
            "320" => Ok(Mp3Bitrate::Kbps320),
            _ => Err(format!(
                "Unsupported MP3 bitrate '{s}'. Supported: 64, 96, 128, 192, 256, 320"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub output_dir: PathBuf,
    pub filename: Option<String>,
    pub quality: QualityPreference,
    pub output_format: OutputFormat,
    pub mp3_bitrate: Mp3Bitrate,
    pub overwrite: bool,
    pub resume: bool,
    pub concurrency: usize,
    pub retries: usize,
    pub ffmpeg_path: Option<PathBuf>,
    pub clip: Option<ClipRange>,
    pub auth: AuthConfig,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            output_dir: PathBuf::from("."),
            filename: None,
            quality: QualityPreference::Best,
            output_format: OutputFormat::Original,
            mp3_bitrate: Mp3Bitrate::Kbps192,
            overwrite: false,
            resume: true,
            concurrency: 4,
            retries: 3,
            ffmpeg_path: None,
            clip: None,
            auth: AuthConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ProgressEvent {
    Extracting {
        url: String,
    },
    Extracted {
        title: String,
        platform: Platform,
        format_count: usize,
    },
    Started {
        total_bytes: Option<u64>,
        total_segments: Option<usize>,
        output_path: PathBuf,
    },
    Progress {
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        completed_segments: Option<usize>,
        total_segments: Option<usize>,
    },
    Muxing {
        message: String,
    },
    Completed {
        output_path: PathBuf,
        bytes_written: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadResult {
    pub info: MediaInfo,
    pub output_path: PathBuf,
    pub bytes_written: u64,
    pub selected_format: MediaFormat,
    pub muxed_audio_format: Option<MediaFormat>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clip_range_parse_and_validate() {
        assert_eq!(ClipRange::parse_timestamp("01:02:03").unwrap(), 3723.0);
        assert_eq!(ClipRange::parse_timestamp("02:30").unwrap(), 150.0);
        assert_eq!(ClipRange::parse_timestamp("45.5").unwrap(), 45.5);
        assert!(ClipRange::parse_timestamp("00:75:00").is_err());

        let clip = ClipRange::from_hms(Some((0, 0, 10)), Some((0, 0, 25))).unwrap();
        assert_eq!(clip.filename_tag(), "00-00-10_to_00-00-25");
        assert!(ClipRange::from_hms(Some((0, 1, 0)), Some((0, 0, 30))).is_err());
    }

    #[test]
    fn test_mp3_bitrate_parse() {
        assert_eq!(Mp3Bitrate::from_str("64").unwrap(), Mp3Bitrate::Kbps64);
        assert_eq!(Mp3Bitrate::from_str("128k").unwrap(), Mp3Bitrate::Kbps128);
        assert_eq!(Mp3Bitrate::from_str("192kbps").unwrap(), Mp3Bitrate::Kbps192);
        assert_eq!(Mp3Bitrate::from_str("320").unwrap(), Mp3Bitrate::Kbps320);
        assert!(Mp3Bitrate::from_str("999").is_err());
    }
}
