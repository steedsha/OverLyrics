//! Native Rust HTTP, used exclusively by background workers. No WebView.
use oly_engine::motion_cover::MIN_ASSET_BYTES;
use oly_engine::net::{HttpError, HttpRequest, HttpResponse, Transport};
use reqwest::blocking::Client;
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::OnceLock,
    time::Duration,
};
const MAX_BODY: u64 = 8 * 1024 * 1024;
const MAX_MOTION_MP4: u64 = 64 * 1024 * 1024;
const MAX_UPDATE_EXE: u64 = 128 * 1024 * 1024;
#[derive(Default)]
pub struct NativeHttp;
// Share the connection pool. A blocking client owns its own bounded runtime;
// building one per lyric request would add threads and discard TLS sessions.
static CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();

fn http_client() -> Result<&'static Client, String> {
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .user_agent(oly_engine::net::DEFAULT_UA)
                .pool_max_idle_per_host(2)
                .pool_idle_timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    if attempt.previous().len() >= 8 {
                        attempt.error("too many redirects")
                    } else if attempt.url().scheme() == "http"
                        && attempt
                            .previous()
                            .last()
                            .is_some_and(|u| u.scheme() == "https")
                    {
                        attempt.stop()
                    } else {
                        attempt.follow()
                    }
                }))
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn validate_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|e| e.to_string())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("unsupported URL".into());
    }
    Ok(url)
}

pub fn has_ftyp_box(header: &[u8]) -> bool {
    header.windows(4).any(|w| w == b"ftyp")
}

/// Atomically downloads an Apple Music motion MP4 into `dest`, validating
/// size and `ftyp` header so half-written/corrupt files never reach the cache.
pub fn download_motion_mp4(url: &str, dest: &Path, timeout_ms: u64) -> Result<u64, String> {
    let parsed = validate_url(url)?;
    let client = http_client()?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = dest.with_extension("mp4.part");
    let mut response = client
        .get(parsed)
        .timeout(Duration::from_millis(timeout_ms.max(1)))
        .send()
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    let write_result = (|| -> Result<u64, String> {
        let mut file = File::create(&tmp).map_err(|e| e.to_string())?;
        let mut buf = [0u8; 16 * 1024];
        let mut head = Vec::with_capacity(64);
        let mut total: u64 = 0;
        loop {
            let n = response.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            if head.len() < 64 {
                let take = (64 - head.len()).min(n);
                head.extend_from_slice(&buf[..take]);
            }
            total += n as u64;
            if total > MAX_MOTION_MP4 {
                return Err("motion mp4 exceeds 64 MiB".into());
            }
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        }
        if total < MIN_ASSET_BYTES || !has_ftyp_box(&head) {
            return Err(format!("invalid motion mp4 ({total} bytes)"));
        }
        file.flush().map_err(|e| e.to_string())?;
        drop(file);
        let _ = std::fs::remove_file(dest);
        std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
        Ok(total)
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    write_result
}

/// Performs a bounded HTTPS/HTTP GET and returns the UTF-8 body.
pub fn get_text(url: &str, headers: &[(&str, &str)], timeout_ms: u64) -> Result<String, String> {
    let parsed = validate_url(url)?;
    let client = http_client()?;
    let mut req = client.get(parsed).timeout(Duration::from_millis(timeout_ms.max(1)));
    for &(k, v) in headers {
        req = req.header(k, v);
    }
    let response = req.send().map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    let mut limited = response.take(MAX_BODY + 1);
    let mut bytes = Vec::new();
    limited.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BODY {
        return Err("response too large".into());
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

/// Downloads a Windows PE executable into `dest`, verifying the `MZ` header
/// and an optional lowercase/uppercase hex SHA-256 digest before atomically
/// moving it into place.
pub fn download_exe_verified(
    url: &str,
    dest: &Path,
    expected_sha256: Option<&str>,
    timeout_ms: u64,
) -> Result<u64, String> {
    use sha2::{Digest, Sha256};

    let parsed = validate_url(url)?;
    let client = http_client()?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = dest.with_extension("exe.part");
    let mut response = client
        .get(parsed)
        .timeout(Duration::from_millis(timeout_ms.max(1)))
        .send()
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    let write_result = (|| -> Result<u64, String> {
        let mut file = File::create(&tmp).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 32 * 1024];
        let mut head = [0u8; 2];
        let mut head_len = 0usize;
        let mut total: u64 = 0;
        loop {
            let n = response.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            if head_len < 2 {
                let take = (2 - head_len).min(n);
                head[head_len..head_len + take].copy_from_slice(&buf[..take]);
                head_len += take;
            }
            total += n as u64;
            if total > MAX_UPDATE_EXE {
                return Err("安装包体积超出上限 (128 MiB)".into());
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        }
        if total < 64 * 1024 || head != *b"MZ" {
            return Err("下载文件不是有效的 Windows 可执行程序".into());
        }
        if let Some(expected) = expected_sha256.map(str::trim).filter(|s| !s.is_empty()) {
            let digest = hasher.finalize();
            let actual: String = digest.iter().map(|b| format!("{b:02x}")).collect();
            if !actual.eq_ignore_ascii_case(expected) {
                return Err("文件 SHA-256 校验失败，已中止更新".into());
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        drop(file);
        let _ = std::fs::remove_file(dest);
        std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
        Ok(total)
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    write_result
}

fn map_error(e: reqwest::Error) -> HttpError {
    if e.is_timeout() {
        HttpError::Timeout
    } else {
        HttpError::Transport(e.to_string())
    }
}
impl Transport for NativeHttp {
    fn send(&self, req: &HttpRequest) -> Result<HttpResponse, HttpError> {
        let url = validate_url(&req.url).map_err(HttpError::Transport)?;
        let client = http_client().map_err(HttpError::Transport)?;
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|e| HttpError::Transport(e.to_string()))?;
        let mut request = client
            .request(method, url)
            .timeout(Duration::from_millis(req.timeout_ms.max(1)));
        for (name, value) in &req.headers {
            request = request.header(name, value);
        }
        if let Some(body) = &req.body {
            request = request.body(body.clone());
        }
        let response = request.send().map_err(map_error)?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let mut body = Vec::new();
        response
            .take(MAX_BODY + 1)
            .read_to_end(&mut body)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.get_ref()
                        .and_then(|e| e.downcast_ref::<reqwest::Error>())
                        .is_some_and(|e| e.is_timeout())
                {
                    HttpError::Timeout
                } else {
                    HttpError::Transport(e.to_string())
                }
            })?;
        if body.len() as u64 > MAX_BODY {
            return Err(HttpError::Transport("response exceeds 8 MiB".into()));
        }
        Ok(HttpResponse {
            status,
            content_type,
            body,
        })
    }
}
