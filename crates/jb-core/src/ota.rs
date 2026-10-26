//! VK OTA source: query the latest stable firmware for a platform code
//! and download the package into the cache. Port of the inline python
//! from the legacy build script.
//!
//! For the jailbreak only the signed loader from the Capsule-14 package
//! is needed — it fits every supported board. `ensure_loader` keeps the
//! cache in `ota/`: the package under its versioned name + the extracted
//! `loader.bin`.

use crate::rkfw;
use rockusb::LoaderContainer;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const API_URL: &str = "https://vc.go.mail.ru/device/firmware";

#[derive(Debug, thiserror::Error)]
pub enum OtaError {
    #[error("VK API: {0}")]
    Api(String),
    #[error("download {url}: {msg}")]
    Download { url: String, msg: String },
    #[error("loader: {0}")]
    Loader(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct OtaMeta {
    pub version: String,
    pub url: String,
}

/// Shared agent: 30 s timeouts, rustls, no env proxies beyond defaults.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .build()
        .into()
}

/// Query the metadata of the latest stable firmware.
pub fn fetch_latest(ota_code: &str) -> Result<OtaMeta, OtaError> {
    let url = format!("{API_URL}?device_ver=0&platform={ota_code}&label=stable");
    let mut resp = agent()
        .get(&url)
        .call()
        .map_err(|e| OtaError::Api(e.to_string()))?;
    let json: serde_json::Value = resp
        .body_mut()
        .read_json()
        .map_err(|e| OtaError::Api(format!("not JSON: {e}")))?;
    let result = json
        .get("result")
        .ok_or_else(|| OtaError::Api("no result field".into()))?;
    Ok(OtaMeta {
        version: result["version"]
            .as_str()
            .ok_or_else(|| OtaError::Api("no version".into()))?
            .to_string(),
        url: result["url"]
            .as_str()
            .ok_or_else(|| OtaError::Api("no url".into()))?
            .to_string(),
    })
}

/// Download the package to `dest` (streamed, not held in memory).
pub fn download(
    url: &str,
    dest: &Path,
    progress: &dyn Fn(u64, Option<u64>),
) -> Result<(), OtaError> {
    let mut resp = agent().get(url).call().map_err(|e| OtaError::Download {
        url: url.to_string(),
        msg: e.to_string(),
    })?;
    let total: Option<u64> = resp
        .headers()
        .get("Content-Length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let mut reader = resp.body_mut().as_reader();
    let mut out = std::io::BufWriter::new(std::fs::File::create(dest)?);
    let mut buf = [0u8; 1 << 20];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| OtaError::Download {
            url: url.to_string(),
            msg: e.to_string(),
        })?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, total);
    }
    out.flush()?;
    Ok(())
}

/// Package path in the cache; downloads when missing. Returns the path
/// and whether it was downloaded.
pub fn ensure_cached(
    meta: &OtaMeta,
    ota_code: &str,
    cache_dir: &Path,
    progress: &dyn Fn(u64, Option<u64>),
) -> Result<(PathBuf, bool), OtaError> {
    std::fs::create_dir_all(cache_dir)?;
    let pkg = cache_dir.join(format!("{ota_code}_{}.bin", meta.version));
    let downloaded = if pkg.is_file() && std::fs::metadata(&pkg)?.len() > 0 {
        false
    } else {
        download(&meta.url, &pkg, progress)?;
        true
    };
    Ok((pkg, downloaded))
}

/// The loader file the tool loads into the board (MaskROM download).
pub const LOADER_FILE: &str = "loader.bin";

/// Make sure `ota_dir/loader.bin` exists and parses: the loader of the
/// latest Capsule-14 OTA (one loader for every supported board). On a
/// cache hit nothing is downloaded. Returns the container and the OTA
/// version when a fresh download happened.
pub fn ensure_loader(
    ota_dir: &Path,
    ota_code: &str,
    progress: &dyn Fn(Step),
) -> Result<(LoaderContainer, Option<String>), OtaError> {
    let loader_path = ota_dir.join(LOADER_FILE);
    if loader_path.is_file() {
        let container = LoaderContainer::parse_file(&loader_path).map_err(|e| {
            OtaError::Loader(format!(
                "cached {} is not a valid BOOT container: {e}",
                loader_path.display()
            ))
        })?;
        progress(Step::cached(&loader_path));
        return Ok((container, None));
    }

    progress(Step::fetching(ota_code));
    let meta = fetch_latest(ota_code)?;
    let (pkg, _downloaded) = ensure_cached(&meta, ota_code, ota_dir, &|done, total| {
        if let Some(t) = total {
            log::debug!("download: {done}/{t} bytes");
        }
    })?;
    progress(Step::unpacking(&pkg));
    let data = std::fs::read(&pkg)?;
    let loader = rkfw::extract_loader(&data)
        .map_err(|e| OtaError::Loader(format!("{}: {e}", pkg.display())))?;
    std::fs::write(&loader_path, &loader)?;
    let container = LoaderContainer::parse(loader)
        .map_err(|e| OtaError::Loader(format!("extracted {}: {e}", loader_path.display())))?;
    progress(Step::saved(&loader_path, meta.version.clone()));
    Ok((container, Some(meta.version)))
}

/// Progress of the OTA/loader preparation step (CLI/GUI).
#[derive(Debug, Clone)]
pub enum Step {
    Cached(String),
    Fetching(String),
    Unpacking(String),
    Saved { path: String, version: String },
}

impl Step {
    fn cached(path: &Path) -> Step {
        Step::Cached(path.display().to_string())
    }
    fn fetching(code: &str) -> Step {
        Step::Fetching(code.to_string())
    }
    fn unpacking(path: &Path) -> Step {
        Step::Unpacking(path.display().to_string())
    }
    fn saved(path: &Path, version: String) -> Step {
        Step::Saved {
            path: path.display().to_string(),
            version,
        }
    }
}
