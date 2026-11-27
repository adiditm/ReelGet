use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::StreamExt;
use reqwest::header::{CONTENT_RANGE, RANGE};
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncWriteExt, BufWriter};
use url::Url;

use crate::error::{ReelGetError, Result};
use crate::http::HttpSession;
use crate::models::ProgressEvent;

pub type ProgressCallback = Arc<dyn Fn(ProgressEvent) + Send + Sync>;

pub async fn download_direct_file(
    session: &HttpSession,
    url: &Url,
    extra_headers: &[(String, String)],
    dest_path: &Path,
    resume: bool,
    overwrite: bool,
    progress: Option<&ProgressCallback>,
) -> Result<u64> {
    download_direct_file_with_completion(
        session,
        url,
        extra_headers,
        dest_path,
        resume,
        overwrite,
        progress,
        true,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn download_direct_file_with_completion(
    session: &HttpSession,
    url: &Url,
    extra_headers: &[(String, String)],
    dest_path: &Path,
    resume: bool,
    overwrite: bool,
    progress: Option<&ProgressCallback>,
    emit_completed: bool,
) -> Result<u64> {
    if dest_path.exists() && !overwrite {
        let meta = fs::metadata(dest_path).await?;
        if meta.len() > 0 {
            if emit_completed {
                if let Some(cb) = progress {
                    cb(ProgressEvent::Completed {
                        output_path: dest_path.to_path_buf(),
                        bytes_written: meta.len(),
                    });
                }
            }
            return Ok(meta.len());
        }
    }

    if let Some(parent) = dest_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).await?;
        }
    }

    let part_path = part_file_path(dest_path);
    let mut existing_len: u64 = if resume && part_path.exists() {
        fs::metadata(&part_path).await.map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };

    let mut req_headers = extra_headers.to_vec();
    if existing_len > 0 {
        req_headers.push((RANGE.as_str().to_string(), format!("bytes={existing_len}-")));
    }

    let headers = session.build_headers(url, &req_headers);
    let resp = session.client().get(url.clone()).headers(headers).send().await?;
    let status = resp.status();

    // Handle 416 Range Not Satisfiable (file may already be complete or server changed size)
    if status.as_u16() == 416 && existing_len > 0 {
        let _ = fs::remove_file(&part_path).await;
        return Box::pin(download_direct_file(
            session,
            url,
            extra_headers,
            dest_path,
            false,
            overwrite,
            progress,
        ))
        .await;
    }

    if !status.is_success() && status.as_u16() != 206 {
        return Err(ReelGetError::HttpStatus {
            status: status.as_u16(),
            url: url.to_string(),
        });
    }

    let is_partial = status.as_u16() == 206;
    if !is_partial {
        existing_len = 0;
    }

    let total_bytes = if is_partial {
        parse_content_range_total(resp.headers().get(CONTENT_RANGE).and_then(|v| v.to_str().ok()))
            .or_else(|| resp.content_length().map(|cl| existing_len + cl))
    } else {
        resp.content_length()
    };

    if let Some(cb) = progress {
        cb(ProgressEvent::Started {
            total_bytes,
            total_segments: None,
            output_path: dest_path.to_path_buf(),
        });
    }

    let file = if is_partial && existing_len > 0 {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&part_path)
            .await?
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&part_path)
            .await?
    };

    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    let mut downloaded = existing_len;
    let mut stream = resp.bytes_stream();

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res?;
        writer.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;
        if let Some(cb) = progress {
            cb(ProgressEvent::Progress {
                downloaded_bytes: downloaded,
                total_bytes,
                completed_segments: None,
                total_segments: None,
            });
        }
    }

    writer.flush().await?;
    drop(writer);

    if dest_path.exists() {
        let _ = fs::remove_file(dest_path).await;
    }
    fs::rename(&part_path, dest_path).await?;

    if emit_completed {
        if let Some(cb) = progress {
            cb(ProgressEvent::Completed {
                output_path: dest_path.to_path_buf(),
                bytes_written: downloaded,
            });
        }
    }

    Ok(downloaded)
}

fn part_file_path(dest: &Path) -> PathBuf {
    let mut os = dest.as_os_str().to_os_string();
    os.push(".part");
    PathBuf::from(os)
}

fn parse_content_range_total(header: Option<&str>) -> Option<u64> {
    let h = header?;
    let (_, total_str) = h.rsplit_once('/')?;
    if total_str == "*" {
        return None;
    }
    total_str.trim().parse::<u64>().ok()
}
