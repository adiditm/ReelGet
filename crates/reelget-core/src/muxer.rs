use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use shine_rs::{Mp3Encoder, Mp3EncoderConfig, StereoMode};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use tokio::process::Command;

use crate::error::{ReelGetError, Result};
use crate::models::{ClipRange, Mp3Bitrate};

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

/// Convert a downloaded video/audio file to MP3 at the requested bitrate (and optional clip range).
/// Uses `ffmpeg` (`libmp3lame`) when available, falling back to pure-Rust `symphonia` + `shine-rs`.
pub async fn convert_to_mp3(
    ffmpeg: Option<&Path>,
    input_path: &Path,
    output_path: &Path,
    bitrate: Mp3Bitrate,
    clip: Option<&ClipRange>,
) -> Result<()> {
    if let Some(c) = clip {
        c.validate().map_err(ReelGetError::InvalidClipRange)?;
    }

    if let Some(ffmpeg_bin) = ffmpeg {
        let mut cmd = Command::new(ffmpeg_bin);
        cmd.arg("-y").arg("-loglevel").arg("error");

        if let Some(c) = clip {
            let start = c.start_secs.unwrap_or(0.0);
            cmd.arg("-ss").arg(format!("{start:.3}"));
        }

        cmd.arg("-i").arg(input_path);

        if let Some(c) = clip {
            let start = c.start_secs.unwrap_or(0.0);
            if let Some(end) = c.end_secs {
                let dur = (end - start).max(0.001);
                cmd.arg("-t").arg(format!("{dur:.3}"));
            }
        }

        let status = cmd
            .arg("-vn")
            .arg("-c:a")
            .arg("libmp3lame")
            .arg("-b:a")
            .arg(format!("{}k", bitrate.kbps()))
            .arg("-id3v2_version")
            .arg("3")
            .arg(output_path)
            .status()
            .await;

        if matches!(status, Ok(s) if s.success()) {
            return Ok(());
        }
        tracing::warn!("ffmpeg libmp3lame failed or unavailable; falling back to pure-Rust MP3 encoder");
    }

    let in_owned = input_path.to_path_buf();
    let out_owned = output_path.to_path_buf();
    let clip_owned = clip.cloned();

    tokio::task::spawn_blocking(move || {
        transcode_to_mp3_pure_rust(&in_owned, &out_owned, bitrate, clip_owned.as_ref())
    })
    .await
    .map_err(|e| ReelGetError::Muxer(format!("MP3 encoder task panicked: {e}")))?
}

fn transcode_to_mp3_pure_rust(
    input_path: &Path,
    output_path: &Path,
    bitrate: Mp3Bitrate,
    clip: Option<&ClipRange>,
) -> Result<()> {
    let file = File::open(input_path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = input_path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| ReelGetError::Muxer(format!("Failed to probe audio container: {e}")))?;

    let (track_id, audio_params) = {
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.as_ref().and_then(|p| p.audio()).is_some())
            .ok_or_else(|| {
                ReelGetError::Muxer("No supported audio track found in downloaded media".to_string())
            })?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .cloned()
            .ok_or_else(|| ReelGetError::Muxer("Missing audio codec parameters".to_string()))?;
        (track.id, params)
    };

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&audio_params, &AudioDecoderOptions::default())
        .map_err(|e| ReelGetError::Muxer(format!("Unsupported audio codec for pure-Rust MP3: {e}")))?;

    let out_file = File::create(output_path)?;
    let mut writer = BufWriter::with_capacity(64 * 1024, out_file);

    let mut encoder: Option<Mp3Encoder> = None;
    let mut src_rate: u32 = 0;
    let mut out_rate: u32 = 44100;
    let mut out_channels: usize = 2;
    let mut pkt_pcm: Vec<i16> = Vec::new();
    let mut total_src_frames: u64 = 0;

    let start_secs = clip.and_then(|c| c.start_secs).unwrap_or(0.0);
    let end_secs = clip.and_then(|c| c.end_secs);

    loop {
        let packet = match format.next_packet() {
            Ok(Some(pkt)) => pkt,
            Ok(None) => break,
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(e) => {
                return Err(ReelGetError::Muxer(format!("Audio demux error: {e}")));
            }
        };

        if packet.track_id != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => {
                return Err(ReelGetError::Muxer(format!("Audio decode error: {e}")));
            }
        };

        let spec = decoded.spec().clone();
        let pkt_rate = spec.rate();
        let src_ch = spec.channels().count();
        if pkt_rate == 0 || src_ch == 0 {
            continue;
        }

        if encoder.is_none() {
            src_rate = pkt_rate;
            out_rate = if matches!(src_rate, 32000 | 44100 | 48000) {
                src_rate
            } else {
                44100
            };
            out_channels = if src_ch == 1 { 1 } else { 2 };
            let stereo_mode = if out_channels == 1 {
                StereoMode::Mono
            } else {
                StereoMode::JointStereo
            };
            let cfg = Mp3EncoderConfig::new()
                .sample_rate(out_rate)
                .bitrate(bitrate.kbps())
                .channels(out_channels as u8)
                .stereo_mode(stereo_mode);
            let enc = Mp3Encoder::new(cfg)
                .map_err(|e| ReelGetError::Muxer(format!("MP3 encoder init failed: {e}")))?;
            encoder = Some(enc);
        }

        pkt_pcm.clear();
        decoded.copy_to_vec_interleaved(&mut pkt_pcm);
        let frames_in_pkt = pkt_pcm.len() / src_ch;
        if frames_in_pkt == 0 {
            continue;
        }

        let start_frame = (start_secs * (src_rate as f64)).round() as u64;
        let end_frame = end_secs.map(|e| (e * (src_rate as f64)).round() as u64);

        let pkt_start = total_src_frames;
        let pkt_end = total_src_frames + frames_in_pkt as u64;
        total_src_frames = pkt_end;

        if pkt_end <= start_frame {
            continue;
        }
        if let Some(ef) = end_frame {
            if pkt_start >= ef {
                break;
            }
        }

        let slice_start_frame = start_frame.saturating_sub(pkt_start) as usize;
        let slice_end_frame = match end_frame {
            Some(ef) => (ef.saturating_sub(pkt_start) as usize).min(frames_in_pkt),
            None => frames_in_pkt,
        };
        if slice_start_frame >= slice_end_frame {
            continue;
        }

        // Normalize channels to 1 or 2
        let mut norm_pcm = Vec::with_capacity((slice_end_frame - slice_start_frame) * out_channels);
        for f in slice_start_frame..slice_end_frame {
            let base = f * src_ch;
            if out_channels == 1 {
                norm_pcm.push(pkt_pcm[base]);
            } else if src_ch == 1 {
                let s = pkt_pcm[base];
                norm_pcm.push(s);
                norm_pcm.push(s);
            } else {
                norm_pcm.push(pkt_pcm[base]);
                norm_pcm.push(pkt_pcm[base + 1]);
            }
        }

        let final_pcm = if src_rate != out_rate {
            resample_linear_interleaved(&norm_pcm, out_channels, src_rate, out_rate)
        } else {
            norm_pcm
        };

        if let Some(ref mut enc) = encoder {
            let frames = enc
                .encode_interleaved(&final_pcm)
                .map_err(|e| ReelGetError::Muxer(format!("MP3 encode error: {e}")))?;
            for frame in frames {
                writer.write_all(&frame)?;
            }
        }
    }

    let Some(mut enc) = encoder else {
        return Err(ReelGetError::Muxer(
            "No audio frames decoded from media file".to_string(),
        ));
    };

    let tail = enc
        .finish()
        .map_err(|e| ReelGetError::Muxer(format!("MP3 flush error: {e}")))?;
    writer.write_all(&tail)?;
    writer.flush()?;
    Ok(())
}

fn resample_linear_interleaved(
    input: &[i16],
    channels: usize,
    in_rate: u32,
    out_rate: u32,
) -> Vec<i16> {
    let in_frames = input.len() / channels;
    if in_frames == 0 || in_rate == out_rate {
        return input.to_vec();
    }
    let out_frames = ((in_frames as u64 * out_rate as u64) / in_rate as u64).max(1) as usize;
    let mut out = Vec::with_capacity(out_frames * channels);
    let ratio = in_rate as f64 / out_rate as f64;

    for i in 0..out_frames {
        let pos = (i as f64) * ratio;
        let idx0 = (pos.floor() as usize).min(in_frames - 1);
        let idx1 = (idx0 + 1).min(in_frames - 1);
        let frac = pos - (idx0 as f64);
        for ch in 0..channels {
            let s0 = input[idx0 * channels + ch] as f64;
            let s1 = input[idx1 * channels + ch] as f64;
            let interp = s0 + (s1 - s0) * frac;
            out.push(interp.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pure_rust_mp3_encode_roundtrip() {
        let tmp_dir = std::env::temp_dir();
        let wav_path = tmp_dir.join("reelget_test_tone.wav");
        let mp3_path = tmp_dir.join("reelget_test_tone.mp3");

        // Write a tiny 0.5s 44.1kHz stereo PCM WAV file
        let sample_rate = 44100u32;
        let num_samples = (sample_rate / 2) as usize;
        let data_bytes = (num_samples * 4) as u32;
        let mut wav = Vec::with_capacity(44 + data_bytes as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&2u16.to_le_bytes()); // stereo
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * 4).to_le_bytes());
        wav.extend_from_slice(&4u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_bytes.to_le_bytes());
        for i in 0..num_samples {
            let t = i as f32 / sample_rate as f32;
            let s = ((t * 440.0 * std::f32::consts::TAU).sin() * 12000.0) as i16;
            wav.extend_from_slice(&s.to_le_bytes());
            wav.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(&wav_path, &wav).unwrap();

        transcode_to_mp3_pure_rust(&wav_path, &mp3_path, Mp3Bitrate::Kbps192, None).unwrap();
        let mp3_bytes = std::fs::read(&mp3_path).unwrap();
        assert!(mp3_bytes.len() > 1000);
        // MPEG frame sync word 0xFF 0xFB / 0xFA
        assert_eq!(mp3_bytes[0], 0xFF);

        let mp3_clip_path = tmp_dir.join("reelget_test_tone_clip.mp3");
        let clip = ClipRange::new(Some(0.1), Some(0.3)).unwrap();
        transcode_to_mp3_pure_rust(&wav_path, &mp3_clip_path, Mp3Bitrate::Kbps128, Some(&clip))
            .unwrap();
        let clip_bytes = std::fs::read(&mp3_clip_path).unwrap();
        assert!(clip_bytes.len() > 500 && clip_bytes.len() < mp3_bytes.len());
        assert_eq!(clip_bytes[0], 0xFF);

        let _ = std::fs::remove_file(&wav_path);
        let _ = std::fs::remove_file(&mp3_path);
        let _ = std::fs::remove_file(&mp3_clip_path);
    }
}
