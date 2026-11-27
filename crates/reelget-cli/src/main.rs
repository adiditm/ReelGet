use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use clap::{Args, Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use reelget_core::auth::AuthConfig;
use reelget_core::models::{ClipRange, DownloadOptions, ProgressEvent, QualityPreference};
use reelget_core::ReelGetClient;

#[derive(Parser, Debug)]
#[command(
    name = "reelget",
    version,
    about = "Multiplatform Rust video & reel downloader (public & authenticated private media)",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[command(flatten)]
    quick: DownloadArgs,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Download one or more videos/reels
    Download(DownloadArgs),
    /// Inspect video metadata and available formats without downloading
    Info(InfoArgs),
    /// Check runtime environment (target OS/arch, ffmpeg, TLS)
    CheckEnv,
}

#[derive(Args, Debug, Clone, Default)]
struct AuthArgs {
    /// Path to Netscape cookies.txt file (for private/authenticated videos)
    #[arg(long, value_name = "FILE")]
    cookies: Option<PathBuf>,

    /// Inline Cookie header string (e.g. "sessionid=...; csrftoken=...")
    #[arg(long, value_name = "STRING")]
    cookie: Option<String>,

    /// Custom HTTP header in "Key: Value" format (repeatable)
    #[arg(short = 'H', long = "header", value_name = "KEY:VALUE")]
    headers: Vec<String>,

    /// Override HTTP User-Agent
    #[arg(long, value_name = "UA")]
    user_agent: Option<String>,
}

impl AuthArgs {
    fn to_auth_config(&self) -> Result<AuthConfig, String> {
        let mut parsed_headers = Vec::new();
        for h in &self.headers {
            let (k, v) = h
                .split_once(':')
                .ok_or_else(|| format!("Invalid header '{h}', expected 'Key: Value'"))?;
            parsed_headers.push((k.trim().to_string(), v.trim().to_string()));
        }
        Ok(AuthConfig {
            cookies_file: self.cookies.clone(),
            raw_cookie: self.cookie.clone(),
            headers: parsed_headers,
            user_agent: self.user_agent.clone(),
        })
    }
}

#[derive(Args, Debug, Clone, Default)]
struct DownloadArgs {
    /// Video or reel URL(s) to download
    #[arg(value_name = "URL")]
    urls: Vec<String>,

    /// Read URLs from a text file (one URL per line)
    #[arg(long, value_name = "FILE")]
    batch: Option<PathBuf>,

    /// Output directory
    #[arg(short = 'o', long, default_value = ".", value_name = "DIR")]
    output: PathBuf,

    /// Custom output filename (without extension; applies to single URL)
    #[arg(short = 'f', long, value_name = "NAME")]
    filename: Option<String>,

    /// Quality preference: best, 1080p, 720p, 480p, 360p, audio, or format_id
    #[arg(short = 'q', long, default_value = "best", value_name = "QUALITY")]
    quality: String,

    /// Concurrent HLS segment downloads
    #[arg(long, default_value_t = 4)]
    concurrency: usize,

    /// HTTP retry attempts on transient errors
    #[arg(long, default_value_t = 3)]
    retries: usize,

    /// Explicit path to ffmpeg binary for adaptive/HLS remuxing and clipping
    #[arg(long, value_name = "PATH")]
    ffmpeg_path: Option<PathBuf>,

    /// Clip start timestamp (HH:MM:SS, MM:SS, or SS)
    #[arg(long = "from", value_name = "TIME")]
    clip_from: Option<String>,

    /// Clip end timestamp (HH:MM:SS, MM:SS, or SS)
    #[arg(long = "to", value_name = "TIME")]
    clip_to: Option<String>,

    /// Overwrite existing output files
    #[arg(long)]
    overwrite: bool,

    /// Disable HTTP Range resume for partial downloads
    #[arg(long)]
    no_resume: bool,

    /// Output machine-readable JSON
    #[arg(long)]
    json: bool,

    #[command(flatten)]
    auth: AuthArgs,
}

#[derive(Args, Debug, Clone)]
struct InfoArgs {
    /// Video or reel URL to inspect
    url: String,

    /// Output machine-readable JSON
    #[arg(long)]
    json: bool,

    #[command(flatten)]
    auth: AuthArgs,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let exit_code = match run(cli).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("Error: {e}");
            1
        }
    };
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let client = ReelGetClient::new();

    match cli.command {
        Some(Commands::CheckEnv) => {
            let ffmpeg = ReelGetClient::check_ffmpeg(None);
            println!("reelget v{}", env!("CARGO_PKG_VERSION"));
            println!("Target OS:   {}", std::env::consts::OS);
            println!("Target Arch: {}", std::env::consts::ARCH);
            println!("TLS Backend: rustls + webpki-roots (pure Rust CA bundle)");
            match ffmpeg {
                Some(p) => println!("ffmpeg:      {}", p.display()),
                None => println!("ffmpeg:      not found (progressive MP4 & fMP4/TS HLS still work)"),
            }
            Ok(())
        }
        Some(Commands::Info(args)) => {
            let auth = args.auth.to_auth_config()?;
            let info = client.extract_info(&args.url, auth, 3).await?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                println!("Platform:  {}", info.platform);
                println!("ID:        {}", info.id);
                println!("Title:     {}", info.title);
                if let Some(ref up) = info.uploader {
                    println!("Uploader:  {up}");
                }
                if let Some(dur) = info.duration_secs {
                    println!("Duration:  {dur:.1}s");
                }
                println!("Auth mode: {}", if info.is_private_or_auth { "authenticated" } else { "public" });
                println!("\nAvailable formats ({}):", info.formats.len());
                for f in &info.formats {
                    let res = match (f.width, f.height) {
                        (Some(w), Some(h)) => format!("{w}x{h}"),
                        _ => f.quality_label.clone().unwrap_or_else(|| "-".to_string()),
                    };
                    let br = f
                        .bitrate_bps
                        .map(|b| format!("{}kbps", b / 1000))
                        .unwrap_or_else(|| "-".to_string());
                    println!(
                        "  {:<14} {:<6} {:<14} {:<10} {:<10} {:?}",
                        f.format_id, f.ext, res, br, format!("{:?}", f.protocol), f.kind
                    );
                }
            }
            Ok(())
        }
        Some(Commands::Download(args)) => run_download(&client, args).await,
        None => {
            if cli.quick.urls.is_empty() && cli.quick.batch.is_none() {
                use clap::CommandFactory;
                Cli::command().print_help()?;
                println!();
                return Ok(());
            }
            run_download(&client, cli.quick).await
        }
    }
}

async fn run_download(
    client: &ReelGetClient,
    args: DownloadArgs,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut urls = args.urls.clone();
    if let Some(ref batch_path) = args.batch {
        let content = tokio::fs::read_to_string(batch_path).await?;
        for line in content.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                urls.push(trimmed.to_string());
            }
        }
    }

    if urls.is_empty() {
        return Err("No URLs provided. Pass one or more URLs or --batch <FILE>.".into());
    }

    let auth = args.auth.to_auth_config()?;
    let quality = QualityPreference::from_str(&args.quality).unwrap_or_default();
    let clip = match (&args.clip_from, &args.clip_to) {
        (None, None) => None,
        (from_opt, to_opt) => {
            let start_secs = from_opt
                .as_deref()
                .map(ClipRange::parse_timestamp)
                .transpose()?;
            let end_secs = to_opt
                .as_deref()
                .map(ClipRange::parse_timestamp)
                .transpose()?;
            Some(ClipRange::new(start_secs, end_secs)?)
        }
    };
    let single_url = urls.len() == 1;

    let mut results = Vec::new();
    let mut failures = 0usize;

    for url in &urls {
        let opts = DownloadOptions {
            output_dir: args.output.clone(),
            filename: if single_url { args.filename.clone() } else { None },
            quality: quality.clone(),
            overwrite: args.overwrite,
            resume: !args.no_resume,
            concurrency: args.concurrency,
            retries: args.retries,
            ffmpeg_path: args.ffmpeg_path.clone(),
            clip: clip.clone(),
            auth: auth.clone(),
        };

        let pb_holder: Arc<Mutex<Option<ProgressBar>>> = Arc::new(Mutex::new(None));
        let json_mode = args.json;

        let cb: reelget_core::downloader::ProgressCallback = {
            let pb_holder = Arc::clone(&pb_holder);
            Arc::new(move |ev| {
                if json_mode {
                    return;
                }
                let mut guard = pb_holder.lock().unwrap();
                match ev {
                    ProgressEvent::Extracting { url } => {
                        let pb = ProgressBar::new_spinner();
                        pb.set_style(
                            ProgressStyle::with_template("{spinner:.cyan} Extracting {msg}")
                                .unwrap(),
                        );
                        pb.set_message(url);
                        pb.enable_steady_tick(std::time::Duration::from_millis(80));
                        *guard = Some(pb);
                    }
                    ProgressEvent::Extracted {
                        title,
                        platform,
                        format_count,
                    } => {
                        if let Some(pb) = guard.take() {
                            pb.finish_and_clear();
                        }
                        eprintln!("[{platform}] {title} ({format_count} formats)");
                    }
                    ProgressEvent::Started {
                        total_bytes,
                        total_segments,
                        ..
                    } => {
                        if let Some(pb) = guard.take() {
                            pb.finish_and_clear();
                        }
                        let pb = if let Some(total) = total_bytes {
                            let p = ProgressBar::new(total);
                            p.set_style(
                                ProgressStyle::with_template(
                                    "{bar:36.cyan/blue} {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
                                )
                                .unwrap()
                                .progress_chars("=>-"),
                            );
                            p
                        } else if let Some(segs) = total_segments {
                            let p = ProgressBar::new(segs as u64);
                            p.set_style(
                                ProgressStyle::with_template(
                                    "{bar:36.cyan/blue} {pos}/{len} segments ({eta})",
                                )
                                .unwrap()
                                .progress_chars("=>-"),
                            );
                            p
                        } else {
                            let p = ProgressBar::new_spinner();
                            p.set_style(
                                ProgressStyle::with_template("{spinner:.cyan} Downloading {bytes}")
                                    .unwrap(),
                            );
                            p.enable_steady_tick(std::time::Duration::from_millis(80));
                            p
                        };
                        *guard = Some(pb);
                    }
                    ProgressEvent::Progress {
                        downloaded_bytes,
                        completed_segments,
                        ..
                    } => {
                        if let Some(ref pb) = *guard {
                            if let Some(seg) = completed_segments {
                                pb.set_position(seg as u64);
                            } else {
                                pb.set_position(downloaded_bytes);
                            }
                        }
                    }
                    ProgressEvent::Muxing { message } => {
                        if let Some(pb) = guard.take() {
                            pb.finish_and_clear();
                        }
                        let pb = ProgressBar::new_spinner();
                        pb.set_style(
                            ProgressStyle::with_template("{spinner:.yellow} {msg}").unwrap(),
                        );
                        pb.set_message(message);
                        pb.enable_steady_tick(std::time::Duration::from_millis(80));
                        *guard = Some(pb);
                    }
                    ProgressEvent::Completed {
                        output_path,
                        bytes_written,
                    } => {
                        if let Some(pb) = guard.take() {
                            pb.finish_and_clear();
                        }
                        eprintln!(
                            "Saved: {} ({:.2} MB)",
                            output_path.display(),
                            bytes_written as f64 / (1024.0 * 1024.0)
                        );
                    }
                }
            })
        };

        match client.download(url, opts, Some(cb)).await {
            Ok(res) => results.push(res),
            Err(e) => {
                if let Some(pb) = pb_holder.lock().unwrap().take() {
                    pb.finish_and_clear();
                }
                failures += 1;
                if args.json {
                    eprintln!(
                        "{}",
                        serde_json::json!({ "ok": false, "url": url, "error": e.to_string() })
                    );
                } else {
                    eprintln!("Failed [{url}]: {e}");
                }
            }
        }
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    }

    if failures > 0 {
        return Err(format!("{failures} download(s) failed").into());
    }

    Ok(())
}
