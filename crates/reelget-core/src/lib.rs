pub mod auth;
pub mod downloader;
pub mod error;
pub mod extractors;
pub mod hls;
pub mod http;
pub mod models;
pub mod muxer;
pub mod utils;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::fs;

use crate::auth::AuthConfig;
use crate::downloader::{
    download_direct_file, download_direct_file_with_completion, ProgressCallback,
};
use crate::error::{ReelGetError, Result};
use crate::extractors::ExtractorRegistry;
use crate::hls::download_hls_stream;
use crate::http::HttpSession;
use crate::models::{
    ClipRange, DownloadOptions, DownloadResult, MediaFormat, MediaInfo, MediaKind, OutputFormat,
    ProgressEvent, QualityPreference, StreamProtocol,
};
use crate::utils::sanitize_filename;

pub struct ReelGetClient {
    registry: ExtractorRegistry,
}

impl Default for ReelGetClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ReelGetClient {
    pub fn new() -> Self {
        Self {
            registry: ExtractorRegistry::new(),
        }
    }

    pub fn check_ffmpeg(custom_path: Option<&Path>) -> Option<PathBuf> {
        muxer::find_ffmpeg(custom_path)
    }

    pub async fn extract_info(
        &self,
        url: &str,
        auth: AuthConfig,
        retries: usize,
    ) -> Result<MediaInfo> {
        let session = HttpSession::new(auth, retries)?;
        self.registry.extract(url, &session).await
    }

    pub async fn download(
        &self,
        url: &str,
        options: DownloadOptions,
        progress: Option<ProgressCallback>,
    ) -> Result<DownloadResult> {
        if let Some(ref clip) = options.clip {
            clip.validate().map_err(ReelGetError::InvalidClipRange)?;
        }

        let ffmpeg = muxer::find_ffmpeg(options.ffmpeg_path.as_deref());
        if options.clip.is_some()
            && options.output_format == OutputFormat::Original
            && ffmpeg.is_none()
        {
            return Err(ReelGetError::Muxer(
                "Video clipping requires ffmpeg. Install ffmpeg or specify --ffmpeg-path."
                    .to_string(),
            ));
        }

        if let Some(ref cb) = progress {
            cb(ProgressEvent::Extracting {
                url: url.to_string(),
            });
        }

        let session = HttpSession::new(options.auth.clone(), options.retries)?;
        let info = self.registry.extract(url, &session).await?;

        if let Some(ref cb) = progress {
            cb(ProgressEvent::Extracted {
                title: info.title.clone(),
                platform: info.platform,
                format_count: info.formats.len(),
            });
        }

        let effective_quality = match (&options.output_format, &options.quality) {
            (OutputFormat::Mp3, QualityPreference::FormatId(_)) => options.quality.clone(),
            (OutputFormat::Mp3, _) => QualityPreference::AudioOnly,
            (OutputFormat::Original, q) => q.clone(),
        };

        let can_mux = ffmpeg.is_some() && options.output_format == OutputFormat::Original;
        let (selected_format, audio_format) = info
            .select_formats(&effective_quality, can_mux)
            .ok_or_else(|| ReelGetError::MediaNotFound("No matching format available".to_string()))?;

        let base_stem = match &options.filename {
            Some(custom) => sanitize_filename(custom, &info.id),
            None => sanitize_filename(&info.title, &info.id),
        };

        let final_stem = match (&options.filename, &options.clip) {
            (None, Some(clip)) => format!("{base_stem} [{}]", clip.filename_tag()),
            _ => base_stem,
        };

        let final_ext = if options.output_format == OutputFormat::Mp3 {
            "mp3".to_string()
        } else if audio_format.is_some() {
            "mp4".to_string()
        } else {
            selected_format.ext.clone()
        };

        let final_dest = options.output_dir.join(format!("{final_stem}.{final_ext}"));
        let needs_postprocess =
            options.output_format == OutputFormat::Mp3 || options.clip.is_some();

        if needs_postprocess && final_dest.exists() && !options.overwrite {
            let meta = fs::metadata(&final_dest).await?;
            if meta.len() > 0 {
                if let Some(ref cb) = progress {
                    cb(ProgressEvent::Completed {
                        output_path: final_dest.clone(),
                        bytes_written: meta.len(),
                    });
                }
                return Ok(DownloadResult {
                    info,
                    output_path: final_dest,
                    bytes_written: meta.len(),
                    selected_format,
                    muxed_audio_format: audio_format,
                });
            }
        }

        let download_stem = if needs_postprocess {
            format!(".{final_stem}.full")
        } else {
            final_stem.clone()
        };

        let inner_progress: Option<ProgressCallback> = if needs_postprocess {
            progress.as_ref().map(|cb| {
                let cb = Arc::clone(cb);
                Arc::new(move |ev: ProgressEvent| {
                    if !matches!(ev, ProgressEvent::Completed { .. }) {
                        cb(ev);
                    }
                }) as ProgressCallback
            })
        } else {
            progress.clone()
        };

        let mut result = download_media_streams(
            &session,
            info,
            selected_format,
            audio_format,
            &options,
            &download_stem,
            ffmpeg.as_deref(),
            needs_postprocess,
            inner_progress.as_ref(),
        )
        .await?;

        if options.output_format == OutputFormat::Mp3 {
            if let Some(ref cb) = progress {
                cb(ProgressEvent::Muxing {
                    message: format!(
                        "Converting to MP3 ({})...",
                        options.mp3_bitrate.label()
                    ),
                });
            }

            let mp3_res = muxer::convert_to_mp3(
                ffmpeg.as_deref(),
                &result.output_path,
                &final_dest,
                options.mp3_bitrate,
                options.clip.as_ref(),
            )
            .await;
            let _ = fs::remove_file(&result.output_path).await;
            mp3_res?;

            let bytes_written = fs::metadata(&final_dest)
                .await
                .map(|m| m.len())
                .unwrap_or(0);

            if let Some(ref cb) = progress {
                cb(ProgressEvent::Completed {
                    output_path: final_dest.clone(),
                    bytes_written,
                });
            }

            result.output_path = final_dest;
            result.bytes_written = bytes_written;
        } else if let (Some(ref clip), Some(ref ffmpeg_bin)) = (&options.clip, &ffmpeg) {
            let start_str = ClipRange::format_hms(clip.start_secs.unwrap_or(0.0));
            let end_str = clip
                .end_secs
                .map(ClipRange::format_hms)
                .unwrap_or_else(|| "end".to_string());

            if let Some(ref cb) = progress {
                cb(ProgressEvent::Muxing {
                    message: format!("Clipping {start_str} -> {end_str} via ffmpeg..."),
                });
            }

            let clipped_ext = result
                .output_path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or(&final_ext);
            let clipped_dest = options.output_dir.join(format!("{final_stem}.{clipped_ext}"));

            let clip_res =
                muxer::clip_media(ffmpeg_bin, &result.output_path, &clipped_dest, clip).await;
            let _ = fs::remove_file(&result.output_path).await;
            clip_res?;

            let bytes_written = fs::metadata(&clipped_dest)
                .await
                .map(|m| m.len())
                .unwrap_or(0);

            if let Some(ref cb) = progress {
                cb(ProgressEvent::Completed {
                    output_path: clipped_dest.clone(),
                    bytes_written,
                });
            }

            result.output_path = clipped_dest;
            result.bytes_written = bytes_written;
        }

        Ok(result)
    }
}

#[allow(clippy::too_many_arguments)]
async fn download_media_streams(
    session: &HttpSession,
    info: MediaInfo,
    selected_format: MediaFormat,
    audio_format: Option<MediaFormat>,
    options: &DownloadOptions,
    stem: &str,
    ffmpeg: Option<&Path>,
    force_overwrite: bool,
    progress: Option<&ProgressCallback>,
) -> Result<DownloadResult> {
    let out_ext = if audio_format.is_some() {
        "mp4"
    } else {
        selected_format.ext.as_str()
    };
    let dest_path = options.output_dir.join(format!("{stem}.{out_ext}"));
    let overwrite = options.overwrite || force_overwrite;

    // Case 1: Adaptive video + separate audio muxed via ffmpeg
    if let (Some(ffmpeg_bin), Some(ref audio_fmt)) = (ffmpeg, &audio_format) {
        let video_tmp = options
            .output_dir
            .join(format!(".{stem}.video.{}", selected_format.ext));
        let audio_tmp = options
            .output_dir
            .join(format!(".{stem}.audio.{}", audio_fmt.ext));

        let v_url = url::Url::parse(&selected_format.url)
            .map_err(|e| ReelGetError::InvalidUrl(e.to_string()))?;
        let a_url = url::Url::parse(&audio_fmt.url)
            .map_err(|e| ReelGetError::InvalidUrl(e.to_string()))?;

        let adaptive_res = async {
            download_direct_file_with_completion(
                session,
                &v_url,
                &selected_format.headers,
                &video_tmp,
                options.resume,
                true,
                progress,
                false,
            )
            .await?;

            download_direct_file_with_completion(
                session,
                &a_url,
                &audio_fmt.headers,
                &audio_tmp,
                options.resume,
                true,
                None,
                false,
            )
            .await?;

            if let Some(cb) = progress {
                cb(ProgressEvent::Muxing {
                    message: "Muxing video and audio tracks via ffmpeg...".to_string(),
                });
            }

            muxer::remux_av(ffmpeg_bin, &video_tmp, &audio_tmp, &dest_path).await?;
            Ok::<(), ReelGetError>(())
        }
        .await;

        let _ = fs::remove_file(&video_tmp).await;
        let _ = fs::remove_file(&audio_tmp).await;
        let _ = fs::remove_file(format!("{}.part", video_tmp.display())).await;
        let _ = fs::remove_file(format!("{}.part", audio_tmp.display())).await;

        match adaptive_res {
            Ok(()) => {
                let bytes_written = fs::metadata(&dest_path).await.map(|m| m.len()).unwrap_or(0);
                if let Some(cb) = progress {
                    cb(ProgressEvent::Completed {
                        output_path: dest_path.clone(),
                        bytes_written,
                    });
                }

                return Ok(DownloadResult {
                    info,
                    output_path: dest_path,
                    bytes_written,
                    selected_format,
                    muxed_audio_format: audio_format,
                });
            }
            Err(ReelGetError::HttpStatus { status: 403, .. }) => {
                return fallback_to_progressive(
                    session,
                    info,
                    options,
                    stem,
                    ffmpeg,
                    overwrite,
                    progress,
                    selected_format.url,
                )
                .await;
            }
            Err(e) => return Err(e),
        }
    }

    // Case 2: HLS stream
    let media_url = url::Url::parse(&selected_format.url)
        .map_err(|e| ReelGetError::InvalidUrl(e.to_string()))?;

    if selected_format.protocol == StreamProtocol::Hls {
        let (final_path, bytes_written) = download_hls_stream(
            session,
            &media_url,
            &selected_format.headers,
            &dest_path,
            &options.quality,
            options.concurrency,
            ffmpeg,
            progress,
        )
        .await?;

        return Ok(DownloadResult {
            info,
            output_path: final_path,
            bytes_written,
            selected_format,
            muxed_audio_format: None,
        });
    }

    // Case 3: Direct progressive MP4/WebM/M4A (with 403 fallback if adaptive AudioOnly/VideoOnly failed)
    match download_direct_file(
        session,
        &media_url,
        &selected_format.headers,
        &dest_path,
        options.resume,
        overwrite,
        progress,
    )
    .await
    {
        Ok(bytes_written) => Ok(DownloadResult {
            info,
            output_path: dest_path,
            bytes_written,
            selected_format,
            muxed_audio_format: None,
        }),
        Err(ReelGetError::HttpStatus { status: 403, .. })
            if selected_format.kind != MediaKind::VideoAndAudio =>
        {
            let _ = fs::remove_file(&dest_path).await;
            let _ = fs::remove_file(format!("{}.part", dest_path.display())).await;
            fallback_to_progressive(
                session,
                info,
                options,
                stem,
                ffmpeg,
                overwrite,
                progress,
                selected_format.url,
            )
            .await
        }
        Err(e) => Err(e),
    }
}

#[allow(clippy::too_many_arguments)]
async fn fallback_to_progressive(
    session: &HttpSession,
    info: MediaInfo,
    options: &DownloadOptions,
    stem: &str,
    ffmpeg: Option<&Path>,
    overwrite: bool,
    progress: Option<&ProgressCallback>,
    failed_url: String,
) -> Result<DownloadResult> {
    if let Some(fallback_fmt) = info
        .formats
        .iter()
        .filter(|f| f.kind == MediaKind::VideoAndAudio)
        .max_by_key(|f| f.resolution_score())
        .cloned()
    {
        tracing::warn!(
            "Adaptive stream returned 403; falling back to progressive format {}",
            fallback_fmt.format_id
        );
        let fb_dest = options
            .output_dir
            .join(format!("{stem}.{}", fallback_fmt.ext));
        let fb_url = url::Url::parse(&fallback_fmt.url)
            .map_err(|e| ReelGetError::InvalidUrl(e.to_string()))?;
        if fallback_fmt.protocol == StreamProtocol::Hls {
            let (p, b) = download_hls_stream(
                session,
                &fb_url,
                &fallback_fmt.headers,
                &fb_dest,
                &options.quality,
                options.concurrency,
                ffmpeg,
                progress,
            )
            .await?;
            return Ok(DownloadResult {
                info,
                output_path: p,
                bytes_written: b,
                selected_format: fallback_fmt,
                muxed_audio_format: None,
            });
        } else {
            let bytes_written = download_direct_file(
                session,
                &fb_url,
                &fallback_fmt.headers,
                &fb_dest,
                options.resume,
                overwrite,
                progress,
            )
            .await?;
            return Ok(DownloadResult {
                info,
                output_path: fb_dest,
                bytes_written,
                selected_format: fallback_fmt,
                muxed_audio_format: None,
            });
        }
    }
    Err(ReelGetError::HttpStatus {
        status: 403,
        url: failed_url,
    })
}
