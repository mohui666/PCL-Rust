//! Hash-addressed restartable transfers. See RFC 9110 sections 14.1 and 15.3.7.
//! Caller owns URL/redirect/credential policy; only verified bytes reach the destination.
use anyhow::{ensure, Context, Result};
use fs2::FileExt;
use reqwest::blocking::Response;
use sha1::Digest;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Condvar, Mutex, OnceLock,
    },
    time::Duration,
};
const CHUNK: u64 = 8 * 1024 * 1024;
pub(crate) fn parallelism(size: u64) -> u32 {
    (size.div_ceil(CHUNK).saturating_sub(1).clamp(1, 4) as u32)
        .min(crate::network::options().threads.into())
}
pub fn cache_root() -> Result<PathBuf> {
    Ok(dirs::cache_dir()
        .context("系统缓存目录不可用")?
        .join("pcl-rust/downloads"))
}
#[derive(Clone, Debug)]
pub struct Checksum {
    pub size: u64,
    pub sha1: Option<String>,
    pub sha256: Option<String>,
    pub sha512: Option<String>,
}
#[derive(Clone, Copy)]
pub enum TransferEvent {
    Started,
    Bytes(usize),
    Finished,
}
impl Checksum {
    fn validate(&self) -> Result<()> {
        ensure!(self.size <= 64 * 1024 * 1024 * 1024, "下载文件超过大小限制");
        ensure!(
            self.sha1.is_some() || self.sha256.is_some() || self.sha512.is_some(),
            "断点下载必须有可信校验值"
        );
        for (hash, len) in [(&self.sha1, 40), (&self.sha256, 64), (&self.sha512, 128)] {
            if let Some(hash) = hash {
                ensure!(
                    hash.len() == len && hash.bytes().all(|c| c.is_ascii_hexdigit()),
                    "下载校验值格式错误"
                );
            }
        }
        Ok(())
    }
    fn verify(&self, path: &Path, cancel: &AtomicBool) -> Result<bool> {
        if !path.exists() {
            return Ok(false);
        }
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "下载缓存不是普通文件"
        );
        if metadata.len() != self.size {
            return Ok(false);
        }
        let mut f = File::open(path)?;
        let (mut a, mut b, mut c) = (sha1::Sha1::new(), sha2::Sha256::new(), sha2::Sha512::new());
        let mut buf = [0; 65536];
        loop {
            crate::install::cancelled(cancel)?;
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            a.update(&buf[..n]);
            b.update(&buf[..n]);
            c.update(&buf[..n]);
        }
        Ok(self
            .sha1
            .as_ref()
            .is_none_or(|h| h.eq_ignore_ascii_case(&format!("{:x}", a.finalize())))
            && self
                .sha256
                .as_ref()
                .is_none_or(|h| h.eq_ignore_ascii_case(&format!("{:x}", b.finalize())))
            && self
                .sha512
                .as_ref()
                .is_none_or(|h| h.eq_ignore_ascii_case(&format!("{:x}", c.finalize()))))
    }
}
struct Slot;
fn slots() -> &'static (Mutex<usize>, Condvar) {
    static S: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();
    S.get_or_init(|| (Mutex::new(0), Condvar::new()))
}
impl Slot {
    fn acquire(cancel: &AtomicBool) -> Result<Self> {
        let (m, c) = slots();
        let mut n = m.lock().unwrap_or_else(|e| e.into_inner());
        while *n >= crate::network::options().threads as usize {
            crate::install::cancelled(cancel)?;
            n = c
                .wait_timeout(n, Duration::from_millis(25))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *n += 1;
        Ok(Self)
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        let (m, c) = slots();
        *m.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        c.notify_all();
    }
}
struct Activity<'a>(&'a (dyn Fn(TransferEvent) + Sync));
impl Drop for Activity<'_> {
    fn drop(&mut self) {
        (self.0)(TransferEvent::Finished);
    }
}
#[derive(Debug)]
struct NoRanges;
impl std::fmt::Display for NoRanges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("服务器未提供匹配的分段响应")
    }
}
impl std::error::Error for NoRanges {}
fn plain_file(path: &Path) -> Result<()> {
    if let Ok(m) = fs::symlink_metadata(path) {
        ensure!(
            m.is_file() && !m.file_type().is_symlink(),
            "断点缓存路径不是普通文件"
        );
    }
    Ok(())
}
fn cache_dir(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    }
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && !m.file_type().is_symlink(),
        "断点缓存目录无效"
    );
    Ok(())
}
#[allow(clippy::too_many_arguments)]
fn part(
    path: &Path,
    begin: u64,
    end: u64,
    total: u64,
    range: bool,
    fetch: &(dyn Fn(Option<(u64, u64)>) -> Result<Response> + Sync),
    cancel: &AtomicBool,
    event: &(dyn Fn(TransferEvent) + Sync),
) -> Result<()> {
    plain_file(path)?;
    let required = end - begin + 1;
    let existing = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if existing == required {
        return Ok(());
    }
    ensure!(existing < required, "断点片段超出长度");
    let _slot = Slot::acquire(cancel)?;
    event(TransferEvent::Started);
    let _active = Activity(event);
    let requested = if range {
        Some((begin + existing, end))
    } else {
        None
    };
    let mut response = fetch(requested)?;
    if range {
        if response.status().as_u16() != 206 {
            return Err(NoRanges.into());
        }
        let expected = format!("bytes {}-{end}/{total}", begin + existing);
        ensure!(
            response
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|h| h.to_str().ok())
                == Some(expected.as_str()),
            "服务器返回错误的分段边界"
        );
    } else {
        ensure!(
            response.status().as_u16() == 200,
            "完整下载响应必须为 HTTP 200"
        );
    }
    ensure!(
        response
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .is_none_or(|h| h == "identity"),
        "分段下载不能使用压缩传输编码"
    );
    let offset = if range { existing } else { 0 };
    if let Some(length) = response.content_length() {
        ensure!(length == required - offset, "分段响应长度错误");
    }
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .append(range)
        .truncate(!range)
        .open(path)?;
    let mut written = offset;
    let mut buf = [0; 65536];
    loop {
        crate::install::cancelled(cancel)?;
        let n = response.read(&mut buf)?;
        if n == 0 {
            break;
        }
        ensure!(written + n as u64 <= required, "下载内容超出分段长度");
        crate::network::throttle(n, cancel)?;
        f.write_all(&buf[..n])?;
        written += n as u64;
        event(TransferEvent::Bytes(n));
    }
    f.sync_all()?;
    ensure!(written == required, "下载响应提前结束，已保留断点");
    Ok(())
}
/// `cache_root` is a launcher-owned directory (not an archive-provided path).
/// A per-content OS lock survives neither normal exit nor process crash.
pub fn download(
    target: &Path,
    cache_root: &Path,
    checksum: &Checksum,
    cancel: &AtomicBool,
    fetch: impl Fn(Option<(u64, u64)>) -> Result<Response> + Sync,
    event: impl Fn(TransferEvent) + Sync,
) -> Result<()> {
    checksum.validate()?;
    plain_file(target)?;
    if checksum.verify(target, cancel)? {
        return Ok(());
    }
    // Debug mode bypasses cross-directory caches; valid destination files above
    // remain reusable, as in the original download diagnostic option.
    let private_cache = crate::system::debug_skip_copy()
        .then(tempfile::tempdir)
        .transpose()?;
    let cache_root = private_cache
        .as_ref()
        .map_or(cache_root, |temp| temp.path());
    fs::create_dir_all(cache_root)?;
    let meta = fs::symlink_metadata(cache_root)?;
    ensure!(
        !meta.file_type().is_symlink() && meta.is_dir(),
        "断点缓存根目录无效"
    );
    let identity = format!(
        "{}:{:?}:{:?}:{:?}",
        checksum.size, checksum.sha1, checksum.sha256, checksum.sha512
    );
    let key = format!("{:x}", sha2::Sha256::digest(identity.as_bytes()));
    let dir = cache_root.join(key);
    cache_dir(&dir)?;
    let lock_path = dir.join("lock");
    plain_file(&lock_path)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                crate::install::cancelled(cancel)?;
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(e.into()),
        }
    }
    crate::install::cancelled(cancel)?;
    let complete = dir.join("complete");
    if !checksum.verify(&complete, cancel)? {
        if complete.exists() {
            fs::remove_file(&complete)?;
        }
        let result = (|| -> Result<()> {
            if checksum.size == 0 {
                File::create(&complete)?;
                return Ok(());
            }
            let chunks = checksum.size.div_ceil(CHUNK);
            let first = dir.join("part-0");
            let mut ranges = match part(
                &first,
                0,
                (CHUNK - 1).min(checksum.size - 1),
                checksum.size,
                true,
                &fetch,
                cancel,
                &event,
            ) {
                Ok(()) => true,
                Err(e) if e.is::<NoRanges>() => false,
                Err(e) => return Err(e),
            };
            if ranges {
                let next = AtomicUsize::new(1);
                let failure = Mutex::new(None);
                let failed = AtomicBool::new(false);
                std::thread::scope(|scope| {
                    for _ in 0..4.min(chunks.saturating_sub(1)) {
                        let (next, failure, failed, dir, fetch, event) =
                            (&next, &failure, &failed, &dir, &fetch, &event);
                        scope.spawn(move || loop {
                            if failed.load(Ordering::Relaxed) {
                                break;
                            }
                            let i = next.fetch_add(1, Ordering::Relaxed) as u64;
                            if i >= chunks {
                                break;
                            }
                            if let Err(e) = part(
                                &dir.join(format!("part-{i}")),
                                i * CHUNK,
                                ((i + 1) * CHUNK - 1).min(checksum.size - 1),
                                checksum.size,
                                true,
                                fetch,
                                cancel,
                                event,
                            ) {
                                failed.store(true, Ordering::Relaxed);
                                let mut first = failure.lock().unwrap();
                                if first.is_none() {
                                    *first = Some(e);
                                }
                                break;
                            }
                        });
                    }
                });
                if let Some(e) = failure.into_inner().unwrap() {
                    if e.is::<NoRanges>() {
                        ranges = false;
                    } else {
                        return Err(e);
                    }
                }
            }
            crate::install::cancelled(cancel)?;
            let parts: Vec<PathBuf> = if ranges {
                (0..chunks).map(|i| dir.join(format!("part-{i}"))).collect()
            } else {
                let whole = dir.join("whole");
                // Servers ignoring Range restart the whole body instead of appending mismatched bytes.
                part(
                    &whole,
                    0,
                    checksum.size - 1,
                    checksum.size,
                    false,
                    &fetch,
                    cancel,
                    &event,
                )?;
                vec![whole]
            };
            let mut combined = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&complete)?;
            for path in &parts {
                let mut source = File::open(path)?;
                let mut buf = [0; 65536];
                loop {
                    crate::install::cancelled(cancel)?;
                    let n = source.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    combined.write_all(&buf[..n])?;
                }
            }
            combined.sync_all()?;
            Ok(())
        })();
        if result.is_err() && complete.exists() {
            fs::remove_file(&complete)?;
        }
        result?;
        if !checksum.verify(&complete, cancel)? {
            // Clear only this content's failed chunks. Never poison all subsequent retries.
            for e in fs::read_dir(&dir)? {
                let p = e?.path();
                if p.file_name().is_some_and(|n| n != "lock") {
                    plain_file(&p)?;
                    fs::remove_file(p)?;
                }
            }
            anyhow::bail!("下载校验失败，损坏断点已清除，原目标文件保留");
        }
    }
    let parent = target.parent().context("下载目标缺少父目录")?;
    fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    std::io::copy(&mut File::open(&complete)?, &mut staged)?;
    staged.as_file().sync_all()?;
    crate::install::cancelled(cancel)?;
    plain_file(target)?;
    staged.persist(target).map_err(|e| e.error)?;
    // Keep verified complete bytes reusable; temporary parts no longer consume duplicate disk space.
    for e in fs::read_dir(&dir)? {
        let p = e?.path();
        if p.file_name()
            .is_some_and(|n| n != "lock" && n != "complete")
        {
            plain_file(&p)?;
            fs::remove_file(p)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{net::TcpListener, sync::Arc, thread};
    type Ranges = Arc<Mutex<Vec<Option<(u64, u64)>>>>;
    struct Server {
        url: String,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
        ranges: Ranges,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.thread.take().unwrap().join().unwrap();
        }
    }
    fn server(bytes: Vec<u8>, ignore: bool, bad_range: bool) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/file", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let (thread_stop, seen) = (stop.clone(), ranges.clone());
        let bytes = Arc::new(bytes);
        let handle = thread::spawn(move || {
            let mut handlers = Vec::new();
            while !thread_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let bytes = bytes.clone();
                        let seen = seen.clone();
                        handlers.push(thread::spawn(move ||{
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();let mut request=Vec::new();let mut b=[0;1];while !request.ends_with(b"\r\n\r\n")&&request.len()<8192{if stream.read(&mut b).unwrap_or(0)==0{return;}request.push(b[0]);}
                let request=String::from_utf8(request).unwrap();let range=request.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("range: bytes=").map(str::to_owned)).map(|r|{let(a,b)=r.split_once('-').unwrap();(a.parse::<u64>().unwrap(),b.parse::<u64>().unwrap())});seen.lock().unwrap().push(range);
                let(start,end,partial)=match(range,ignore){(Some((a,b)),false)=>(a,b,true),_=>(0,bytes.len() as u64-1,false)};
                let head=if partial{format!("HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",end-start+1,if bad_range{start+1}else{start},end,bytes.len())}else{format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",bytes.len())};
                let _=stream.write_all(head.as_bytes());let _=stream.write_all(&bytes[start as usize..=end as usize]);
            }));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(e) => panic!("{e}"),
                }
            }
            for h in handlers {
                h.join().unwrap();
            }
        });
        Server {
            url,
            stop,
            thread: Some(handle),
            ranges,
        }
    }
    fn checksum(bytes: &[u8]) -> Checksum {
        Checksum {
            size: bytes.len() as u64,
            sha1: Some(format!("{:x}", sha1::Sha1::digest(bytes))),
            sha256: None,
            sha512: None,
        }
    }
    fn fetch(
        client: &reqwest::blocking::Client,
        url: &str,
        range: Option<(u64, u64)>,
    ) -> Result<Response> {
        let mut req = client.get(url).header("Accept-Encoding", "identity");
        if let Some((a, b)) = range {
            req = req.header("Range", format!("bytes={a}-{b}"));
        }
        Ok(req.send()?)
    }
    #[test]
    fn cancelled_body_resumes_exact_offset_after_restart_and_publishes_only_verified_bytes() {
        let bytes = vec![17; 1024 * 1024];
        let server = server(bytes.clone(), false, false);
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::write(&target, b"original").unwrap();
        let cache = dir.path().join("cache");
        let cancel = AtomicBool::new(false);
        let client = reqwest::blocking::Client::new();
        let result = download(
            &target,
            &cache,
            &checksum(&bytes),
            &cancel,
            |r| fetch(&client, &server.url, r),
            |event| {
                if matches!(event, TransferEvent::Bytes(_)) {
                    cancel.store(true, Ordering::Relaxed)
                }
            },
        );
        assert!(result.unwrap_err().is::<crate::model::OperationCancelled>());
        assert_eq!(fs::read(&target).unwrap(), b"original");
        let fresh = AtomicBool::new(false);
        download(
            &target,
            &cache,
            &checksum(&bytes),
            &fresh,
            |r| fetch(&client, &server.url, r),
            |_| (),
        )
        .unwrap();
        assert_eq!(fs::read(target).unwrap(), bytes);
        let ranges = server.ranges.lock().unwrap();
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0], Some((0, 1024 * 1024 - 1)));
        assert!(ranges[1].unwrap().0 > 0);
    }
    #[test]
    fn ignored_ranges_restart_full_body_instead_of_appending() {
        let bytes = vec![23; 256 * 1024];
        let server = server(bytes.clone(), true, false);
        let dir = tempfile::tempdir().unwrap();
        let client = reqwest::blocking::Client::new();
        let target = dir.path().join("target");
        download(
            &target,
            &dir.path().join("cache"),
            &checksum(&bytes),
            &AtomicBool::new(false),
            |r| fetch(&client, &server.url, r),
            |_| (),
        )
        .unwrap();
        assert_eq!(fs::read(target).unwrap(), bytes);
        assert_eq!(
            *server.ranges.lock().unwrap(),
            vec![Some((0, 256 * 1024 - 1)), None]
        );
    }
    #[test]
    fn wrong_ranges_and_bad_hashes_never_replace_user_file() {
        for bad_range in [false, true] {
            let bytes = vec![31; 10000];
            let server = server(bytes.clone(), false, bad_range);
            let dir = tempfile::tempdir().unwrap();
            let client = reqwest::blocking::Client::new();
            let target = dir.path().join("target");
            fs::write(&target, b"preserve").unwrap();
            let mut hash = checksum(&bytes);
            if !bad_range {
                hash.sha1 = Some("a".repeat(40));
            }
            assert!(download(
                &target,
                &dir.path().join("cache"),
                &hash,
                &AtomicBool::new(false),
                |r| fetch(&client, &server.url, r),
                |_| ()
            )
            .is_err());
            assert_eq!(fs::read(target).unwrap(), b"preserve");
        }
    }
    #[test]
    fn large_body_is_split_and_merged_in_file_order() {
        let bytes = (0..CHUNK as usize * 2 + 193)
            .map(|i| (i % 251) as u8)
            .collect::<Vec<_>>();
        let server = server(bytes.clone(), false, false);
        let dir = tempfile::tempdir().unwrap();
        let client = reqwest::blocking::Client::new();
        let target = dir.path().join("target");
        download(
            &target,
            &dir.path().join("cache"),
            &checksum(&bytes),
            &AtomicBool::new(false),
            |r| fetch(&client, &server.url, r),
            |_| (),
        )
        .unwrap();
        assert_eq!(fs::read(target).unwrap(), bytes);
        let mut ranges = server.ranges.lock().unwrap().clone();
        ranges.sort();
        assert_eq!(
            ranges,
            vec![
                Some((0, CHUNK - 1)),
                Some((CHUNK, CHUNK * 2 - 1)),
                Some((CHUNK * 2, CHUNK * 2 + 192))
            ]
        );
    }
    #[test]
    fn zero_length_file_needs_no_request_but_still_checks_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty");
        download(
            &path,
            &dir.path().join("cache"),
            &checksum(b""),
            &AtomicBool::new(false),
            |_| panic!("no network for empty file"),
            |_| (),
        )
        .unwrap();
        assert_eq!(fs::metadata(path).unwrap().len(), 0);
    }
    #[test]
    fn cached_first_part_can_fall_back_when_server_stops_supporting_ranges() {
        let bytes = vec![31; CHUNK as usize + 100];
        let source = server(bytes.clone(), false, false);
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("cache");
        let target = dir.path().join("target");
        let cancel = AtomicBool::new(false);
        let client = reqwest::blocking::Client::new();
        let received = AtomicUsize::new(0);
        assert!(download(
            &target,
            &cache,
            &checksum(&bytes),
            &cancel,
            |r| fetch(&client, &source.url, r),
            |event| if let TransferEvent::Bytes(n) = event {
                if received.fetch_add(n, Ordering::Relaxed) + n >= CHUNK as usize {
                    cancel.store(true, Ordering::Relaxed)
                }
            }
        )
        .is_err());
        let replacement = server(bytes.clone(), true, false);
        download(
            &target,
            &cache,
            &checksum(&bytes),
            &AtomicBool::new(false),
            |r| fetch(&client, &replacement.url, r),
            |_| (),
        )
        .unwrap();
        assert_eq!(fs::read(target).unwrap(), bytes);
        assert_eq!(
            *replacement.ranges.lock().unwrap(),
            vec![Some((CHUNK, CHUNK + 99)), None]
        );
    }
}
