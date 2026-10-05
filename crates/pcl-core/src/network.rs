//! Launcher download preferences. Account endpoints never use this routing policy.
use anyhow::{bail, ensure, Result};
use reqwest::{
    blocking::{Client, Response},
    redirect::Policy,
    Url,
};
use serde::{Deserialize, Serialize};
use std::{
    sync::{atomic::AtomicBool, Mutex, OnceLock, RwLock},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourcePreference {
    MirrorFirst,
    #[default]
    OfficialFirst,
    OfficialOnly,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DownloadOptions {
    pub file_source: SourcePreference,
    pub version_source: SourcePreference,
    pub threads: u16,
    pub speed_limit_kib: u32,
}
impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            file_source: SourcePreference::OfficialFirst,
            version_source: SourcePreference::OfficialFirst,
            threads: 64,
            speed_limit_kib: 0,
        }
    }
}
impl DownloadOptions {
    pub fn validate(&self) -> Result<()> {
        ensure!((1..=256).contains(&self.threads), "下载线程数必须为 1–256");
        ensure!(self.speed_limit_kib <= 1024 * 1024, "下载限速超过 1 GiB/s");
        Ok(())
    }
}
fn preferences() -> &'static RwLock<DownloadOptions> {
    static VALUE: OnceLock<RwLock<DownloadOptions>> = OnceLock::new();
    VALUE.get_or_init(|| RwLock::new(DownloadOptions::default()))
}
pub fn options() -> DownloadOptions {
    preferences()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}
pub fn configure(value: &DownloadOptions) -> Result<()> {
    value.validate()?;
    *preferences().write().unwrap_or_else(|e| e.into_inner()) = value.clone();
    Ok(())
}

fn clean_https(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.port_or_known_default() == Some(443)
}
/// Only known publisher paths have mirror equivalents. Never route authentication.
pub fn mirror_url(original: &Url) -> Option<Url> {
    if !clean_https(original) || original.query().is_some() {
        return None;
    }
    let path = match original.host_str()? {
        "piston-meta.mojang.com"
        | "piston-data.mojang.com"
        | "launchermeta.mojang.com"
        | "launcher.mojang.com" => original.path().to_owned(),
        "resources.download.minecraft.net" => format!("/assets{}", original.path()),
        "libraries.minecraft.net" | "maven.minecraftforge.net" | "maven.fabricmc.net" => {
            format!("/maven{}", original.path())
        }
        "meta.fabricmc.net" => format!("/fabric-meta{}", original.path()),
        "maven.neoforged.net" if original.path().starts_with("/releases/net/neoforged/") => {
            format!("/maven{}", original.path().strip_prefix("/releases")?)
        }
        _ => return None,
    };
    Url::parse(&format!("https://bmclapi2.bangbang93.com{path}")).ok()
}
fn public_ipv4(ip: std::net::Ipv4Addr) -> bool {
    !ip.is_private()
        && !ip.is_loopback()
        && !ip.is_link_local()
        && !ip.is_unspecified()
        && !ip.is_broadcast()
        && !ip.is_multicast()
}
fn mirror_redirect(url: &Url, verified: bool) -> bool {
    if !clean_https(url) {
        return false;
    }
    match url.host_str() {
        Some("bmclapi2.bangbang93.com" | "bmclapi.bangbang93.com") => true,
        Some(host) if verified => {
            let host = host.trim_end_matches('.');
            !host.ends_with(".local")
                && !host.ends_with(".localhost")
                && host != "localhost"
                && match host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
                    Ok(std::net::IpAddr::V4(ip)) => public_ipv4(ip),
                    Ok(std::net::IpAddr::V6(ip)) => {
                        !ip.is_loopback()
                            && !ip.is_multicast()
                            && ip.to_ipv4_mapped().is_none_or(public_ipv4)
                            && !ip.is_unspecified()
                            && (ip.segments()[0] & 0xfe00) != 0xfc00
                            && (ip.segments()[0] & 0xffc0) != 0xfe80
                    }
                    _ => true,
                }
        }
        _ => false,
    }
}
/// Caller validates the original publisher URL and verifies bytes before commit.
/// Mirrors with distributed nodes are usable only with an expected content hash.
pub(crate) fn request(
    client: &Client,
    original: Url,
    metadata: bool,
    verified: bool,
    cancel: &AtomicBool,
) -> Result<Response> {
    request_range(client, original, metadata, verified, cancel, None)
}
pub(crate) fn request_range(
    client: &Client,
    original: Url,
    metadata: bool,
    verified: bool,
    cancel: &AtomicBool,
    range: Option<(u64, u64)>,
) -> Result<Response> {
    crate::system::debug_delay(cancel, crate::system::DebugPhase::Request)?;
    let prefs = options();
    let source = if metadata {
        prefs.version_source
    } else {
        prefs.file_source
    };
    let mirror = mirror_url(&original);
    let urls = match (source, mirror) {
        (SourcePreference::MirrorFirst, Some(m)) => vec![(m, true), (original, false)],
        (SourcePreference::OfficialFirst, Some(m)) => vec![(original, false), (m, true)],
        _ => vec![(original, false)],
    };
    let mut failures = Vec::new();
    for (url, mirrored) in urls {
        crate::install::cancelled(cancel)?;
        let response = if mirrored {
            Client::builder()
                .user_agent("PCL-Rust/0.1 (BMCLAPI)")
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(120))
                .redirect(Policy::custom(move |a| {
                    if a.previous().len() < 5 && mirror_redirect(a.url(), verified) {
                        a.follow()
                    } else {
                        a.error("拒绝不安全的镜像跳转")
                    }
                }))
                .build()?
                .get(url)
                .headers(range_headers(range))
                .send()
        } else {
            let request = client.get(url).headers(range_headers(range));
            // Official-first falls back on a bounded slow request; official-only
            // preserves the publisher client's timeout for large artifacts.
            if source == SourcePreference::OfficialFirst {
                request
                    .timeout(Duration::from_secs(if metadata { 12 } else { 45 }))
                    .send()
            } else {
                request.send()
            }
        };
        crate::install::cancelled(cancel)?;
        match response {
            Ok(r) if r.status().is_success() => return Ok(r),
            Ok(r) => failures.push(format!(
                "{} HTTP {}",
                if mirrored { "BMCLAPI" } else { "官方源" },
                r.status().as_u16()
            )),
            Err(e) => failures.push(format!(
                "{}{}",
                if mirrored { "BMCLAPI" } else { "官方源" },
                if e.is_timeout() {
                    "请求超时"
                } else {
                    "连接失败"
                }
            )),
        }
    }
    bail!("{}", failures.join("；"))
}

fn range_headers(range: Option<(u64, u64)>) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT_ENCODING,
        reqwest::header::HeaderValue::from_static("identity"),
    );
    if let Some((first, last)) = range {
        headers.insert(
            reqwest::header::RANGE,
            reqwest::header::HeaderValue::from_str(&format!("bytes={first}-{last}")).unwrap(),
        );
    }
    headers
}

struct RateState {
    updated: Instant,
    speed: u32,
    tokens: f64,
}
impl RateState {
    fn consume(&mut self, now: Instant, speed: u32, needed: usize) -> usize {
        let rate = f64::from(speed) * 1024.0;
        if self.speed != speed {
            self.speed = speed;
            self.tokens = 0.0;
            self.updated = now;
        }
        self.tokens = (self.tokens
            + now.saturating_duration_since(self.updated).as_secs_f64() * rate)
            .min((rate * 0.05).max(1.0));
        self.updated = now;
        let used = needed.min(self.tokens.floor() as usize);
        self.tokens -= used as f64;
        used
    }
}
/// Aggregate token budget. Waiting requests never reserve future capacity, so
/// cancellation cannot leave seconds of unused debt for subsequent transfers.
pub(crate) fn throttle(bytes: usize, cancel: &AtomicBool) -> Result<()> {
    static RATE: OnceLock<Mutex<RateState>> = OnceLock::new();
    let mut remaining = bytes;
    while remaining > 0 {
        crate::install::cancelled(cancel)?;
        let speed = options().speed_limit_kib;
        if speed == 0 {
            return Ok(());
        }
        let now = Instant::now();
        let used = RATE
            .get_or_init(|| {
                Mutex::new(RateState {
                    updated: now,
                    speed,
                    tokens: 0.0,
                })
            })
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .consume(now, speed, remaining);
        remaining -= used;
        if remaining > 0 {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    crate::install::cancelled(cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routing_never_mirrors_accounts_or_unknown_hosts() {
        for value in [
            "https://login.live.com/oauth20_token.srf",
            "https://api.minecraftservices.com/minecraft/profile",
            "https://example.com/a",
            "http://libraries.minecraft.net/a",
            "https://libraries.minecraft.net/a?secret=x",
        ] {
            assert!(mirror_url(&Url::parse(value).unwrap()).is_none());
        }
        assert_eq!(
            mirror_url(&Url::parse("https://resources.download.minecraft.net/ab/abcd").unwrap())
                .unwrap()
                .as_str(),
            "https://bmclapi2.bangbang93.com/assets/ab/abcd"
        );
        for local in [
            "https://127.0.0.1/a",
            "https://[::1]/a",
            "https://[::ffff:127.0.0.1]/a",
            "https://localhost./a",
            "https://[fe80::1]/a",
            "https://[fc00::1]/a",
        ] {
            assert!(
                !mirror_redirect(&Url::parse(local).unwrap(), true),
                "{local}"
            );
        }
        assert!(!mirror_redirect(
            &Url::parse("http://cdn.example/a").unwrap(),
            true
        ));
        assert!(!mirror_redirect(
            &Url::parse("https://cdn.example/a").unwrap(),
            false
        ));
    }
    #[test]
    fn waiting_transfers_never_reserve_future_rate_budget() {
        let now = Instant::now();
        let mut budget = RateState {
            updated: now,
            speed: 1,
            tokens: 0.0,
        };
        for _ in 0..4 {
            assert_eq!(budget.consume(now, 1, 65536), 0);
        }
        // All four callers may cancel. A fresh caller receives the next 50 ms,
        // rather than inheriting 256 seconds of unconsumed reservations.
        assert_eq!(
            budget.consume(now + Duration::from_millis(50), 1, 65536),
            51
        );
        assert!(budget.updated <= now + Duration::from_millis(50));
    }
}
