use std::path::{Path, PathBuf};
use std::sync::Arc;

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use futures_util::stream::{FuturesOrdered, StreamExt};
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncWriteExt, BufWriter};
use tokio::sync::Semaphore;
use url::Url;

use crate::downloader::ProgressCallback;
use crate::error::{ReelGetError, Result};
use crate::http::HttpSession;
use crate::models::{ProgressEvent, QualityPreference};
use crate::muxer;

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

#[derive(Debug, Clone)]
struct HlsVariant {
    url: Url,
    bandwidth: u64,
    height: Option<u32>,
}

#[derive(Debug, Clone)]
enum HlsKeyMethod {
    None,
    Aes128 {
        uri: Url,
        iv: Option<[u8; 16]>,
    },
}

#[derive(Debug, Clone)]
struct HlsSegment {
    url: Url,
    sequence: u64,
    key: HlsKeyMethod,
}

#[derive(Debug, Clone)]
struct MediaPlaylist {
    init_map_url: Option<Url>,
    segments: Vec<HlsSegment>,
}

pub async fn download_hls_stream(
    session: &HttpSession,
    playlist_url: &Url,
    extra_headers: &[(String, String)],
    dest_path: &Path,
    quality: &QualityPreference,
    concurrency: usize,
    ffmpeg_path: Option<&Path>,
    progress: Option<&ProgressCallback>,
) -> Result<(PathBuf, u64)> {
    let (resolved_url, playlist_text) = session
        .get_text_with_final_url(playlist_url, extra_headers)
        .await?;

    let media_url = if playlist_text.contains("#EXT-X-STREAM-INF") {
        let variants = parse_master_playlist(&resolved_url, &playlist_text)?;
        select_variant(&variants, quality)
            .ok_or_else(|| ReelGetError::HlsParse("No variants in HLS master playlist".to_string()))?
            .url
            .clone()
    } else {
        resolved_url
    };

    let (media_base_url, media_text) = session
        .get_text_with_final_url(&media_url, extra_headers)
        .await?;
    let playlist = parse_media_playlist(&media_base_url, &media_text)?;

    if playlist.segments.is_empty() {
        return Err(ReelGetError::HlsParse(
            "HLS media playlist contains zero segments".to_string(),
        ));
    }

    if let Some(parent) = dest_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).await?;
        }
    }

    let is_fmp4 = playlist.init_map_url.is_some();
    let raw_ext = if is_fmp4 { "mp4" } else { "ts" };
    let raw_path = if !is_fmp4 && ffmpeg_path.is_some() {
        dest_path.with_extension("raw.ts")
    } else if !is_fmp4 {
        dest_path.with_extension("ts")
    } else {
        dest_path.to_path_buf()
    };

    let part_path = PathBuf::from(format!("{}.part", raw_path.display()));
    let total_segments = playlist.segments.len();

    if let Some(cb) = progress {
        cb(ProgressEvent::Started {
            total_bytes: None,
            total_segments: Some(total_segments),
            output_path: dest_path.to_path_buf(),
        });
    }

    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&part_path)
        .await?;
    let mut writer = BufWriter::with_capacity(128 * 1024, file);
    let mut total_written: u64 = 0;

    // Write fMP4 initialization segment if present
    if let Some(ref init_url) = playlist.init_map_url {
        let init_bytes = session.get_bytes(init_url, extra_headers).await?;
        writer.write_all(&init_bytes).await?;
        total_written += init_bytes.len() as u64;
    }

    // Cache fetched AES-128 keys by URI
    let mut key_cache: std::collections::HashMap<String, [u8; 16]> =
        std::collections::HashMap::new();
    for seg in &playlist.segments {
        if let HlsKeyMethod::Aes128 { uri, .. } = &seg.key {
            let key_str = uri.to_string();
            if !key_cache.contains_key(&key_str) {
                let key_bytes = session.get_bytes(uri, extra_headers).await?;
                if key_bytes.len() != 16 {
                    return Err(ReelGetError::Crypto(format!(
                        "Expected 16-byte AES-128 key, got {} bytes",
                        key_bytes.len()
                    )));
                }
                let mut arr = [0u8; 16];
                arr.copy_from_slice(&key_bytes);
                key_cache.insert(key_str, arr);
            }
        }
    }

    let sem = Arc::new(Semaphore::new(concurrency.max(1)));
    let key_cache = Arc::new(key_cache);
    let mut futures = FuturesOrdered::new();

    for seg in playlist.segments {
        let sem_clone = Arc::clone(&sem);
        let session_clone = session.clone();
        let headers_clone = extra_headers.to_vec();
        let keys_clone = Arc::clone(&key_cache);

        futures.push_back(tokio::spawn(async move {
            let _permit = sem_clone
                .acquire_owned()
                .await
                .map_err(|_| ReelGetError::Cancelled)?;
            let raw = session_clone.get_bytes(&seg.url, &headers_clone).await?;
            match seg.key {
                HlsKeyMethod::None => Ok::<Vec<u8>, ReelGetError>(raw.to_vec()),
                HlsKeyMethod::Aes128 { uri, iv } => {
                    let key = keys_clone
                        .get(uri.as_str())
                        .ok_or_else(|| ReelGetError::Crypto("Missing cached AES key".to_string()))?;
                    let iv_bytes = iv.unwrap_or_else(|| {
                        let mut buf = [0u8; 16];
                        buf[8..16].copy_from_slice(&seg.sequence.to_be_bytes());
                        buf
                    });
                    decrypt_aes128_cbc(&raw, key, &iv_bytes)
                }
            }
        }));
    }

    let mut completed = 0usize;
    while let Some(join_res) = futures.next().await {
        let seg_bytes = join_res.map_err(|e| ReelGetError::HlsParse(e.to_string()))??;
        writer.write_all(&seg_bytes).await?;
        total_written += seg_bytes.len() as u64;
        completed += 1;
        if let Some(cb) = progress {
            cb(ProgressEvent::Progress {
                downloaded_bytes: total_written,
                total_bytes: None,
                completed_segments: Some(completed),
                total_segments: Some(total_segments),
            });
        }
    }

    writer.flush().await?;
    drop(writer);

    if raw_path.exists() {
        let _ = fs::remove_file(&raw_path).await;
    }
    fs::rename(&part_path, &raw_path).await?;

    let final_path = if raw_ext == "ts" {
        if let Some(ffmpeg) = ffmpeg_path {
            if let Some(cb) = progress {
                cb(ProgressEvent::Muxing {
                    message: "Remuxing HLS MPEG-TS stream to MP4 via ffmpeg...".to_string(),
                });
            }
            let mp4_dest = dest_path.with_extension("mp4");
            muxer::remux_ts_to_mp4(ffmpeg, &raw_path, &mp4_dest).await?;
            let _ = fs::remove_file(&raw_path).await;
            total_written = fs::metadata(&mp4_dest)
                .await
                .map(|m| m.len())
                .unwrap_or(total_written);
            mp4_dest
        } else {
            raw_path
        }
    } else {
        raw_path
    };

    if let Some(cb) = progress {
        cb(ProgressEvent::Completed {
            output_path: final_path.clone(),
            bytes_written: total_written,
        });
    }

    Ok((final_path, total_written))
}

fn decrypt_aes128_cbc(data: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Result<Vec<u8>> {
    let mut buf = data.to_vec();
    let dec = Aes128CbcDec::new(key.into(), iv.into());
    let plain = dec
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|e| ReelGetError::Crypto(format!("AES-128 CBC decrypt failed: {e}")))?;
    Ok(plain.to_vec())
}

fn parse_master_playlist(base_url: &Url, text: &str) -> Result<Vec<HlsVariant>> {
    let mut variants = Vec::new();
    let mut pending_bw: Option<u64> = None;
    let mut pending_height: Option<u32> = None;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(attrs) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending_bw = parse_attr_u64(attrs, "BANDWIDTH");
            pending_height = parse_resolution_height(attrs);
        } else if !line.starts_with('#') {
            if let Some(bw) = pending_bw.take() {
                let url = base_url
                    .join(line)
                    .map_err(|e| ReelGetError::HlsParse(e.to_string()))?;
                variants.push(HlsVariant {
                    url,
                    bandwidth: bw,
                    height: pending_height.take(),
                });
            }
        }
    }

    Ok(variants)
}

fn select_variant<'a>(
    variants: &'a [HlsVariant],
    quality: &QualityPreference,
) -> Option<&'a HlsVariant> {
    match quality {
        QualityPreference::MaxHeight(cap) => {
            let capped: Vec<&HlsVariant> = variants
                .iter()
                .filter(|v| v.height.map(|h| h <= *cap).unwrap_or(true))
                .collect();
            if !capped.is_empty() {
                return capped
                    .into_iter()
                    .max_by_key(|v| (v.height.unwrap_or(0), v.bandwidth));
            }
            variants
                .iter()
                .max_by_key(|v| (v.height.unwrap_or(0), v.bandwidth))
        }
        _ => variants
            .iter()
            .max_by_key(|v| (v.height.unwrap_or(0), v.bandwidth)),
    }
}

fn parse_media_playlist(base_url: &Url, text: &str) -> Result<MediaPlaylist> {
    let mut init_map_url: Option<Url> = None;
    let mut segments = Vec::new();
    let mut sequence: u64 = 0;
    let mut current_key = HlsKeyMethod::None;
    let mut expect_segment_uri = false;

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(seq_str) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            sequence = seq_str.trim().parse::<u64>().unwrap_or(0);
        } else if let Some(attrs) = line.strip_prefix("#EXT-X-MAP:") {
            if let Some(uri_str) = parse_quoted_attr(attrs, "URI") {
                init_map_url = Some(
                    base_url
                        .join(&uri_str)
                        .map_err(|e| ReelGetError::HlsParse(e.to_string()))?,
                );
            }
        } else if let Some(attrs) = line.strip_prefix("#EXT-X-KEY:") {
            let method = parse_unquoted_attr(attrs, "METHOD").unwrap_or_default();
            let keyformat = parse_quoted_attr(attrs, "KEYFORMAT")
                .unwrap_or_default()
                .to_ascii_lowercase();

            if method.eq_ignore_ascii_case("SAMPLE-AES")
                || method.eq_ignore_ascii_case("SAMPLE-AES-CTR")
                || keyformat.contains("widevine")
                || keyformat.contains("fairplay")
                || keyformat.contains("playready")
                || keyformat.contains("streamingkeydelivery")
            {
                return Err(ReelGetError::DrmProtected(format!(
                    "HLS stream uses DRM ({method} / {keyformat})"
                )));
            }

            if method.eq_ignore_ascii_case("NONE") {
                current_key = HlsKeyMethod::None;
            } else if method.eq_ignore_ascii_case("AES-128") {
                let uri_str = parse_quoted_attr(attrs, "URI").ok_or_else(|| {
                    ReelGetError::HlsParse("AES-128 EXT-X-KEY missing URI".to_string())
                })?;
                let uri = base_url
                    .join(&uri_str)
                    .map_err(|e| ReelGetError::HlsParse(e.to_string()))?;
                let iv = parse_unquoted_attr(attrs, "IV").and_then(|s| parse_hex_iv(&s));
                current_key = HlsKeyMethod::Aes128 { uri, iv };
            }
        } else if line.starts_with("#EXTINF:") {
            expect_segment_uri = true;
        } else if !line.starts_with('#') && expect_segment_uri {
            let url = base_url
                .join(line)
                .map_err(|e| ReelGetError::HlsParse(e.to_string()))?;
            segments.push(HlsSegment {
                url,
                sequence,
                key: current_key.clone(),
            });
            sequence += 1;
            expect_segment_uri = false;
        }
    }

    Ok(MediaPlaylist {
        init_map_url,
        segments,
    })
}

fn parse_attr_u64(attrs: &str, key: &str) -> Option<u64> {
    parse_unquoted_attr(attrs, key)?.parse::<u64>().ok()
}

fn parse_resolution_height(attrs: &str) -> Option<u32> {
    let res = parse_unquoted_attr(attrs, "RESOLUTION")?;
    let (_, h) = res.split_once('x')?;
    h.parse::<u32>().ok()
}

fn parse_unquoted_attr(attrs: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=");
    let idx = attrs.find(&needle)?;
    let rest = &attrs[idx + needle.len()..];
    let val = rest.split(',').next()?.trim().trim_matches('"');
    Some(val.to_string())
}

fn parse_quoted_attr(attrs: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let idx = attrs.find(&needle)?;
    let rest = &attrs[idx + needle.len()..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn parse_hex_iv(raw: &str) -> Option<[u8; 16]> {
    let hex = raw
        .trim()
        .strip_prefix("0x")
        .or_else(|| raw.trim().strip_prefix("0X"))
        .unwrap_or(raw.trim());
    let padded = format!("{hex:0>32}");
    if padded.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&padded[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_master_and_media_playlist() {
        let base = Url::parse("https://cdn.example.com/hls/master.m3u8").unwrap();
        let master = "\
#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=800000,RESOLUTION=640x360
low/index.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=2500000,RESOLUTION=1280x720
hd/index.m3u8
";
        let variants = parse_master_playlist(&base, master).unwrap();
        assert_eq!(variants.len(), 2);
        let best = select_variant(&variants, &QualityPreference::Best).unwrap();
        assert_eq!(best.height, Some(720));
        assert_eq!(best.url.as_str(), "https://cdn.example.com/hls/hd/index.m3u8");
    }

    #[test]
    fn test_drm_detection() {
        let base = Url::parse("https://cdn.example.com/hls/index.m3u8").unwrap();
        let drm_m3u8 = "\
#EXTM3U
#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://key\",KEYFORMAT=\"com.apple.streamingkeydelivery\"
#EXTINF:4.0,
seg0.ts
";
        let err = parse_media_playlist(&base, drm_m3u8).unwrap_err();
        assert!(matches!(err, ReelGetError::DrmProtected(_)));
    }
}
