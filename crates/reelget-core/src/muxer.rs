use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::error::{ReelGetError, Result};
use crate::models::ClipRange;

/// Locate `ffmpeg` binary from an explicit path, executable directory, or system `PATH`.
pub fn find_ffmpeg(custom_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = custom_path {
        if p.is_file() {
            return Some(p.to_path_buf());
        }
    }

    let bin_name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let bundled = dir.join(bin_name);
            if bundled.is_file() {
                return Some(bundled);
            }
        }
    }

    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(bin_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // Common macOS Homebrew fallback paths when launched from Finder (.app bundle)
    #[cfg(target_os = "macos")]
    for fallback in ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"] {
        let p = Path::new(fallback);
        if p.is_file() {
            return Some(p.to_path_buf());
        }
    }

    None
}

/// Remux separate video and audio files into a single container without re-encoding.
pub async fn remux_av(
    ffmpeg: &Path,
    video_path: &Path,
    audio_path: &Path,
    output_path: &Path,
) -> Result<()> {
    let status = Command::new(ffmpeg)
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(video_path)
        .arg("-i")
        .arg(audio_path)
        .arg("-c")
        .arg("copy")
        .arg("-movflags")
        .arg("+faststart")
        .arg(output_path)
        .status()
        .await?;

    if !status.success() {
        return Err(ReelGetError::Muxer(format!(
            "ffmpeg exited with status {status} while muxing video + audio"
        )));
    }
    Ok(())
}

/// Remux an MPEG-TS file into an MP4 container without re-encoding.
pub async fn remux_ts_to_mp4(ffmpeg: &Path, ts_path: &Path, output_path: &Path) -> Result<()> {
    let status = Command::new(ffmpeg)
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(ts_path)
        .arg("-c")
        .arg("copy")
        .arg("-bsf:a")
        .arg("aac_adtstoasc")
        .arg("-movflags")
        .arg("+faststart")
        .arg(output_path)
        .status()
        .await?;

    if !status.success() {
        // Retry without aac_adtstoasc in case audio stream is already non-ADTS
        let retry = Command::new(ffmpeg)
            .arg("-y")
            .arg("-loglevel")
            .arg("error")
            .arg("-i")
            .arg(ts_path)
            .arg("-c")
            .arg("copy")
            .arg("-movflags")
            .arg("+faststart")
            .arg(output_path)
            .status()
            .await?;
        if !retry.success() {
            return Err(ReelGetError::Muxer(format!(
                "ffmpeg exited with status {retry} while remuxing TS to MP4"
            )));
        }
    }
    Ok(())
}

/// Clip a downloaded media file to `[start_secs, end_secs]` using ffmpeg.
/// Uses frame-accurate H.264/AAC encoding when possible, falling back to stream copy.
pub async fn clip_media(
    ffmpeg: &Path,
    input_path: &Path,
    output_path: &Path,
    clip: &ClipRange,
) -> Result<()> {
    clip.validate().map_err(ReelGetError::InvalidClipRange)?;

    let start = clip.start_secs.unwrap_or(0.0);
    let duration = clip.end_secs.map(|e| (e - start).max(0.001));
    let is_audio_only = output_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "m4a" | "mp3" | "aac" | "opus"))
        .unwrap_or(false);

    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-ss")
        .arg(format!("{start:.3}"))
        .arg("-i")
        .arg(input_path);

    if let Some(dur) = duration {
        cmd.arg("-t").arg(format!("{dur:.3}"));
    }

    if is_audio_only {
        cmd.arg("-vn").arg("-c:a").arg("aac");
    } else {
        cmd.arg("-c:v")
            .arg("libx264")
            .arg("-preset")
            .arg("veryfast")
            .arg("-crf")
            .arg("18")
            .arg("-c:a")
            .arg("aac")
            .arg("-movflags")
            .arg("+faststart");
    }

    let status = cmd.arg(output_path).status().await?;
    if status.success() {
        return Ok(());
    }

    // Fallback to stream copy if libx264/aac encoder is unavailable in a minimal ffmpeg build
    let mut fallback = Command::new(ffmpeg);
    fallback
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-ss")
        .arg(format!("{start:.3}"))
        .arg("-i")
        .arg(input_path);
    if let Some(dur) = duration {
        fallback.arg("-t").arg(format!("{dur:.3}"));
    }
    let fb_status = fallback
        .arg("-c")
        .arg("copy")
        .arg("-movflags")
        .arg("+faststart")
        .arg(output_path)
        .status()
        .await?;

    if !fb_status.success() {
        return Err(ReelGetError::Muxer(format!(
            "ffmpeg exited with status {fb_status} while clipping media"
        )));
    }
    Ok(())
}
