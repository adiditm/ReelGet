use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let ico_path = manifest_dir.join("assets").join("icon.ico");
    if !ico_path.is_file() {
        println!("cargo:warning=Missing Windows icon at {}", ico_path.display());
        return;
    }

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon(ico_path.to_str().unwrap());
        res.set("ProductName", "ReelGet");
        res.set("FileDescription", "ReelGet Video & Reel Downloader");
        if let Err(e) = res.compile() {
            println!("cargo:warning=winresource failed: {e}");
        }
        return;
    }

    #[cfg(not(windows))]
    {
        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        let rc_path = out_dir.join("reelget_icon.rc");
        let res_path = out_dir.join("reelget_icon.res");
        let obj_path = out_dir.join("reelget_icon.o");

        let ico_escaped = ico_path.to_string_lossy().replace('\\', "/");
        let rc_content = format!("1 ICON \"{ico_escaped}\"\n");
        if std::fs::write(&rc_path, rc_content).is_err() {
            return;
        }

        let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        let machine = match target_arch.as_str() {
            "aarch64" => "ARM64",
            "x86" => "X86",
            _ => "X64",
        };

        // 1. Try `zig rc` or `llvm-rc` -> `.res`, then `llvm-cvtres` -> COFF `.o`
        let rc_ok = run_rc(&rc_path, &res_path);
        if rc_ok {
            if let Some(cvtres) = find_tool("llvm-cvtres") {
                let status = Command::new(cvtres)
                    .arg(format!("/MACHINE:{machine}"))
                    .arg(format!("/OUT:{}", obj_path.display()))
                    .arg(&res_path)
                    .status();
                if matches!(status, Ok(s) if s.success()) && obj_path.is_file() {
                    println!("cargo:rustc-link-arg-bin=reelget-gui={}", obj_path.display());
                    return;
                }
            }
        }

        // 2. Fallback: try `llvm-windres` or `<arch>-w64-mingw32-windres`
        let windres_target = if target_arch == "aarch64" {
            "aarch64-w64-mingw32"
        } else {
            "x86_64-w64-mingw32"
        };
        if let Some(windres) = find_tool("llvm-windres") {
            let status = Command::new(windres)
                .arg(format!("--target={windres_target}"))
                .arg("-i")
                .arg(&rc_path)
                .arg("-o")
                .arg(&obj_path)
                .arg("-O")
                .arg("coff")
                .status();
            if matches!(status, Ok(s) if s.success()) && obj_path.is_file() {
                println!("cargo:rustc-link-arg-bin=reelget-gui={}", obj_path.display());
                return;
            }
        }

        println!("cargo:warning=Could not embed Windows .ico resource (zig/llvm-cvtres not found)");
    }
}

#[cfg(not(windows))]
fn run_rc(rc_path: &Path, res_path: &Path) -> bool {
    if let Some(zig) = find_tool("zig") {
        let status = Command::new(zig)
            .arg("rc")
            .arg(rc_path)
            .arg(res_path)
            .status();
        if matches!(status, Ok(s) if s.success()) && res_path.is_file() {
            return true;
        }
    }
    if let Some(llvm_rc) = find_tool("llvm-rc") {
        let status = Command::new(llvm_rc)
            .arg("/FO")
            .arg(res_path)
            .arg(rc_path)
            .status();
        if matches!(status, Ok(s) if s.success()) && res_path.is_file() {
            return true;
        }
    }
    false
}

#[cfg(not(windows))]
fn find_tool(name: &str) -> Option<PathBuf> {
    if let Some(path_var) = env::var_os("PATH") {
        for dir in env::split_paths(&path_var) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for extra_dir in [
        "/opt/homebrew/opt/llvm@22/bin",
        "/opt/homebrew/opt/llvm/bin",
        "/opt/homebrew/bin",
        "/usr/local/opt/llvm/bin",
        "/usr/local/bin",
    ] {
        let candidate = Path::new(extra_dir).join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
