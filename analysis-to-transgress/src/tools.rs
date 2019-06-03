#![allow(dead_code)]

use crate::{unblock, Error, Result};
use futures::channel::oneshot;
use futures_timer::FutureExt;
use log::{info, warn};
use rayon::prelude::*;
use reprieve::{unblocked, unblocked_};
use serde_json::{json, Value};
use std::env;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Ensure that rls analysis data is available and up to date.
pub async fn ensure_analysis(path: &Path) -> Result<()> {
    let path_ = path.to_owned();
    let status = unblocked! {
        Command::new("cargo")
            .args(&["check"])
            .current_dir(path_)
            .status()?
    }
    .await?;

    if !status.success() {
        return Err(Error::CargoCheckFailed);
    }

    let mut rls = Command::new("rls")
        .args(&["--cli"])
        .current_dir(path)
        .stdout(Stdio::piped())
        .spawn()?;

    let mut stdout = None;
    std::mem::swap(&mut rls.stdout, &mut stdout);

    let _rls = Killer(rls);

    let reader = BufReader::new(stdout.ok_or(Error::Other {
        cause: "can't get rls stdout",
    })?);

    let (sx, rx) = oneshot::channel();

    let result = unblocked! {
        let mut sx = Some(sx);
        for line in reader.lines() {
            if let Some(sx) = sx.take() {
                sx.send(()).ok().ok_or(Error::Other { cause: "rx died" })?;
            }
            let value = serde_json::from_str::<Value>(&line?);
            if let Ok(value) = value {
                if is_done(&value).is_some() {
                    return Ok(());
                }
            }
        }
    };

    // TODO config
    async {
        rx.await.ok().ok_or(Error::Other {
            cause: "listener died",
        })
    }
        .timeout(Duration::from_secs(5))
        .await?;

    result.await?;

    Ok(())
}

fn is_done(value: &Value) -> Option<()> {
    if value.get("jsonrpc") != Some(&json!("2.0")) {
        warn!("unexpected RLS version: {:?}", value.get("jsonrpc"));
    }
    if value.get("method")? != &json!("window/progress") {
        return None;
    }

    let params = value.get("params")?.as_object()?;

    if params.get("title") == Some(&json!("Building")) {
        if params.get("done") == Some(&json!(true)) {
            return Some(());
        }
        info!("rls building: {}", params.get("message")?.as_str()?);
    }
    None
}

struct Killer(std::process::Child);
impl Drop for Killer {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

/// Fetch analysis
pub async fn fetch_analysis(path: &Path) -> Result<Vec<rls_data::Analysis>> {
    // TODO env / alt-target handling
    let dir = path
        .join("target")
        .join("rls")
        .join("debug") // TODO?
        .join("deps")
        .join("save-analysis")
        .into();

    let dirs = vec![dir, system_analysis_folder().await?];

    let mut targets: Vec<PathBuf> = vec![];

    for dir in dirs {
        if !dir.is_dir() {
            return Err(Error::MissingSaveAnalysis {
                dir: format!("{}", dir.display()),
            });
        }

        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if let Some(ext) = path.extension() {
                if ext == "json" {
                    targets.push(path.clone());
                }
            }
        }
    }
    targets.sort();

    let parsed = targets
        .par_iter()
        .flat_map(|target| -> Result<rls_data::Analysis> {
            info!("parsing {}", target.display());
            Ok(serde_json::from_reader(BufReader::new(File::open(
                target,
            )?))?)
        })
        .collect::<Vec<_>>();

    Ok(parsed)
}

// get system analysis folders
async fn system_analysis_folder() -> Result<PathBuf> {
    // based on: https://github.com/rust-lang/rls/blob/ca0456b/rls-analysis/src/loader.rs#L75-L91

    // TODO: libs_path and src_path both assume the default `libdir = "lib"`.
    let sys_root_path = sys_root_path().await?;
    let target_triple = extract_target_triple(sys_root_path.as_path()).await?;
    let libs_path = sys_root_path
        .join("lib")
        .join("rustlib")
        .join(&target_triple)
        .join("analysis");
    Ok(libs_path.into())
}

async fn extract_target_triple(sys_root_path: &Path) -> Result<String> {
    // First try to get the triple from the rustc version output,
    // otherwise fall back on the rustup-style toolchain path.
    // TODO: Both methods assume that the target is the host triple,
    // which isn't the case for cross-compilation (rust-lang/rls#309).
    info!("asking rustc for target triple");
    let host = extract_rustc_host_triple().await;
    if let Some(host) = host {
        Ok(host)
    } else {
        info!("parsing sysroot for target triple");
        extract_rustup_target_triple(sys_root_path)
    }
}

async fn extract_rustc_host_triple() -> Option<String> {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| String::from("rustc"));
    let verbose_version = unblocked_! {
        Command::new(rustc)
        .arg("--verbose")
        .arg("--version")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
    }.await?;

    // Extracts the triple from a line like `host: x86_64-unknown-linux-gnu`
    verbose_version
        .lines()
        .find(|line| line.starts_with("host: "))
        .and_then(|host| host.split_whitespace().nth(1))
        .map(String::from)
}

// TODO: This can fail when using a custom toolchain in rustup (often linked to
// `/$rust_repo/build/$target/stage2`)
fn extract_rustup_target_triple(sys_root_path: &Path) -> Result<String> {
    // Extracts nightly-x86_64-pc-windows-msvc from
    // $HOME/.rustup/toolchains/nightly-x86_64-pc-windows-msvc
    let toolchain = sys_root_path
        .iter()
        .last()
        .and_then(OsStr::to_str)
        .ok_or(Error::Other {
            cause: "extracting toolchain failed",
        })?;
    // Extracts x86_64-pc-windows-msvc from nightly-x86_64-pc-windows-pc
    Ok(toolchain
        .splitn(2, '-')
        .last()
        .map(String::from)
        .ok_or(Error::Other {
            cause: "extracting target triple failed",
        })?)
}

async fn sys_root_path() -> Result<PathBuf> {
    env::var("SYSROOT")
        .ok()
        .map(PathBuf::from)
        .or_else(|| unblocked! {
            Command::new(env::var("RUSTC").unwrap_or_else(|_| String::from("rustc")))
                .arg("--print")
                .arg("sysroot")
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|s| PathBuf::from(s.trim()))
        })
        .ok_or(Error::CantResolveSysroot)
}
