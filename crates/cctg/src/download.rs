//! The worker agent downloads the hub's release on ⬆️ Обновить (TASK-050).
//!
//! A hub built for a release tag sends it in `update` to an outdated agent.
//! The worker then reads that release's `SHA256SUMS` and, when the file it
//! was started from ([`crate::shim::SOURCE_VAR`]) is not the release's
//! binary for this platform, downloads `cctg-<tag>-<target>[.exe]`, checks
//! its sha256 and puts it in that file's place the way `install.sh` and
//! `cctg deploy` do: on Windows the running file is renamed to `cctg.old`
//! first, elsewhere the new one is renamed over it. Only then does the
//! update go on as before ([`crate::update::Worker::plan`] finds the newer
//! file). Any failure leaves the old file as it was.
//!
//! Only on the user's press, never by itself. Logs name the tag and the
//! target, never a URL.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use tracing::info;

use crate::client;
use crate::deploy::{self, Files};

/// Where releases are: `<base>/<tag>/<file>`.
pub const DEFAULT_BASE: &str = "https://github.com/pockerhead/cctg/releases/download";
/// Another base (a mirror, tests): https, or http only on this machine.
pub const BASE_VAR: &str = "CCTG_RELEASE_BASE_URL";
/// The largest binary taken; release builds are about a tenth of it.
pub const MAX_BINARY: usize = 64 << 20;
const MAX_SUMS: usize = 64 << 10;
/// The whole download, within the hub's wait for the answer
/// ([`crate::hub::slots::UPDATE_WAIT`]).
pub const TIMEOUT: Duration = Duration::from_secs(90);
/// How long the swap waits for a `cctg deploy` or another agent's download
/// in the same bin directory.
const LOCK_WAIT: Duration = Duration::from_secs(30);

/// Why nothing was put in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// Network, timeout, an HTTP error, a file too large, a write or rename.
    Download,
    /// The release has no binary for this platform.
    Missing,
    /// The binary does not match `SHA256SUMS`.
    Checksum,
}

/// What a successful fetch did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetched {
    /// The release's binary is now in place of the file.
    Installed,
    /// The file already was the release's binary: nothing downloaded.
    Present,
}

/// A tag as `build.rs` bakes it: 1 to 64 of `[A-Za-z0-9._-]`, and not a
/// path step.
pub fn is_tag(tag: &str) -> bool {
    (1..=64).contains(&tag.len())
        && !tag.starts_with('.')
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// The release target of this build (`release.yml`); `None` where there
/// are no release builds.
pub fn target() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("x86_64-pc-windows-msvc")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-unknown-linux-musl")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-apple-darwin")
    } else {
        None
    }
}

/// `cctg-<tag>-<target>`, `.exe` on Windows.
pub fn asset_name(tag: &str, target: &str) -> String {
    format!("cctg-{tag}-{target}{}", std::env::consts::EXE_SUFFIX)
}

/// The sha256 `SHA256SUMS` (`sha256sum` output) gives for `asset`, lowercase.
pub fn expected_sum(sums: &str, asset: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        let (hash, name) = (words.next()?, words.next()?);
        let named = name == asset || name.strip_prefix('*') == Some(asset);
        (named && hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

/// The release base: env [`BASE_VAR`] or [`DEFAULT_BASE`].
pub fn base_from_env() -> String {
    std::env::var(BASE_VAR)
        .ok()
        .map(|base| base.trim().to_owned())
        .filter(|base| !base.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_owned())
}

/// Whether the swap may still begin. The agent's exit closes it: a fetch
/// that has not begun to write then writes nothing, and one that has is
/// waited for, so an exit never cuts the swap in two.
#[derive(Debug, Default)]
pub struct Gate(AtomicU8);

const OPEN: u8 = 0;
const CLOSED: u8 = 1;
const SWAPPING: u8 = 2;

impl Gate {
    /// Closes the gate; `false` when a swap already runs (wait for it).
    pub fn close(&self) -> bool {
        match self
            .0
            .compare_exchange(OPEN, CLOSED, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => true,
            Err(state) => state == CLOSED,
        }
    }

    fn closed(&self) -> bool {
        self.0.load(Ordering::SeqCst) == CLOSED
    }

    /// The swap begins, unless the gate was closed.
    fn enter(&self) -> bool {
        self.0
            .compare_exchange(OPEN, SWAPPING, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

/// Puts release `tag`'s binary for this platform in place of `exe`,
/// unless `exe` already is it. Within [`TIMEOUT`]: no swap begins after
/// it, so the answer always says what is on disk.
pub async fn fetch(
    base: &str,
    tag: &str,
    exe: &Path,
    gate: &Arc<Gate>,
) -> Result<Fetched, Failure> {
    recover_blocking(exe).await;
    if !is_tag(tag) {
        info!("the hub's release tag is no tag; nothing downloaded");
        return Err(Failure::Missing);
    }
    let Some(target) = target() else {
        info!(tag, "no release builds for this platform");
        return Err(Failure::Missing);
    };
    let deadline = Instant::now() + TIMEOUT;
    let downloaded = tokio::time::timeout_at(deadline.into(), download(base, tag, target, exe))
        .await
        .unwrap_or(Err(Failure::Download));
    let outcome = match downloaded {
        Ok(Some((binary, want))) => {
            let (exe, gate) = (exe.to_owned(), gate.clone());
            tokio::task::spawn_blocking(move || install(&exe, &binary, &want, deadline, &gate))
                .await
                .unwrap_or(Err(Failure::Download))
        }
        Ok(None) => Ok(Fetched::Present),
        Err(failure) => Err(failure),
    };
    info!(tag, target, ?outcome, "release download");
    outcome
}

/// The checked binary and its sum, or `None` when `exe` already is it.
async fn download(
    base: &str,
    tag: &str,
    target: &str,
    exe: &Path,
) -> Result<Option<(Vec<u8>, String)>, Failure> {
    let base = base.trim_end_matches('/');
    let local = local_http(base);
    if !base.starts_with("https://") && !local {
        return Err(Failure::Download);
    }
    let https = !local;
    let mut builder = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if follows(https, attempt.previous().len(), attempt.url().scheme()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }));
    if local {
        builder = builder.no_proxy();
    }
    let http = builder.build().map_err(|_| Failure::Download)?;
    let asset = asset_name(tag, target);
    let sums = get(&http, &format!("{base}/{tag}/SHA256SUMS"), MAX_SUMS).await?;
    let sums = String::from_utf8_lossy(&sums);
    let want = expected_sum(&sums, &asset).ok_or(Failure::Missing)?;
    if is_file_with(exe.to_owned(), want.clone()).await {
        return Ok(None);
    }
    let binary = get(&http, &format!("{base}/{tag}/{asset}"), MAX_BINARY).await?;
    if sha256_hex(&binary) != want {
        return Err(Failure::Checksum);
    }
    Ok(Some((binary, want)))
}

/// A redirect is followed: at most 10, and from an https base never down
/// to http (GitHub sends the file from another host).
fn follows(https: bool, hops: usize, scheme: &str) -> bool {
    hops < 10 && (!https || scheme == "https")
}

/// Plain http to `127.0.0.1` or `localhost`, as `install.sh` allows; the
/// host as a URL parser sees it, without user info.
fn local_http(base: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base) else {
        return false;
    };
    url.scheme() == "http"
        && url.username().is_empty()
        && url.password().is_none()
        && matches!(url.host_str(), Some("127.0.0.1" | "localhost"))
}

/// The body of `url`, at most `max` bytes. 404 is [`Failure::Missing`].
async fn get(http: &reqwest::Client, url: &str, max: usize) -> Result<Vec<u8>, Failure> {
    // No reqwest error leaves here: its text carries the URL.
    let mut response = http.get(url).send().await.map_err(|_| Failure::Download)?;
    match response.status().as_u16() {
        200 => {}
        404 => return Err(Failure::Missing),
        _ => return Err(Failure::Download),
    }
    if response
        .content_length()
        .is_some_and(|length| length > max as u64)
    {
        return Err(Failure::Download);
    }
    let mut body = Vec::new();
    while let Some(piece) = response.chunk().await.map_err(|_| Failure::Download)? {
        if body.len() + piece.len() > max {
            return Err(Failure::Download);
        }
        body.extend_from_slice(&piece);
    }
    Ok(body)
}

async fn is_file_with(path: std::path::PathBuf, sum: String) -> bool {
    tokio::task::spawn_blocking(move || client::build_of(&path).is_ok_and(|hash| hash == sum))
        .await
        .unwrap_or(false)
}

fn sha256_hex(bytes: &[u8]) -> String {
    aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Writes `binary` next to `exe` and swaps it in, under the deploy lock of
/// that bin directory, unless `deadline` passed or `gate` closed first.
/// Blocking.
fn install(
    exe: &Path,
    binary: &[u8],
    sum: &str,
    deadline: Instant,
    gate: &Gate,
) -> Result<Fetched, Failure> {
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name().and_then(|name| name.to_str()))
    else {
        return Err(Failure::Download);
    };
    let files = Files::new(dir, name);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&files.lock)
        .map_err(|_| Failure::Download)?;
    let deadline = deadline.min(Instant::now() + LOCK_WAIT);
    while lock.try_lock().is_err() {
        if Instant::now() >= deadline || gate.closed() {
            return Err(Failure::Download);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    recover(&files);
    // Another session's agent may have put it in meanwhile.
    if client::build_of(exe).is_ok_and(|hash| hash == sum) {
        return Ok(Fetched::Present);
    }
    if Instant::now() >= deadline || !gate.enter() {
        return Err(Failure::Download);
    }
    deploy::sweep(&files);
    let written = write_part(&files.part, binary);
    let swapped = written.and_then(|()| swap(&files));
    if swapped.is_err() {
        let _ = std::fs::remove_file(&files.part);
    }
    swapped.map(|()| Fetched::Installed)
}

fn write_part(part: &Path, binary: &[u8]) -> Result<(), Failure> {
    let _ = std::fs::remove_file(part);
    let mut file = std::fs::File::create(part).map_err(|_| Failure::Download)?;
    file.write_all(binary)
        .and_then(|()| file.sync_all())
        .map_err(|_| Failure::Download)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(part, std::fs::Permissions::from_mode(0o755))
            .map_err(|_| Failure::Download)?;
    }
    Ok(())
}

/// A swap cut between its two renames (the process was killed) leaves no
/// `current` but its `old`: that goes back, under the deploy lock and only
/// when nothing else holds it. Blocking. Done at every ⬆️ Обновить
/// ([`fetch`], [`crate::agent`]): a missing `cctg` cannot start anything,
/// but the running workers of other sessions can put it back.
pub fn recover_in(exe: &Path) {
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name().and_then(|name| name.to_str()))
    else {
        return;
    };
    let files = Files::new(dir, name);
    if files.current.exists() || !files.old.exists() {
        return;
    }
    let Ok(lock) = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&files.lock)
    else {
        return;
    };
    if lock.try_lock().is_ok() {
        recover(&files);
    }
}

/// [`recover_in`] with the lock held.
fn recover(files: &Files) {
    if !files.current.exists() && files.old.exists() {
        let back = deploy::rename(&files.old, &files.current).is_ok();
        info!(
            back,
            "the binary was missing after a cut swap; the previous one put back"
        );
    }
}

async fn recover_blocking(exe: &Path) {
    let exe = exe.to_owned();
    let _ = tokio::task::spawn_blocking(move || recover_in(&exe)).await;
}

/// Windows cannot replace a running exe but can rename it: the old file
/// goes to `cctg.old.exe` ([`deploy::swap_in`], which puts it back when
/// the new one does not go in). Elsewhere a rename replaces it at once.
fn swap(files: &Files) -> Result<(), Failure> {
    if cfg!(windows) {
        deploy::swap_in(files).map_err(|_| Failure::Download)
    } else {
        deploy::rename(&files.part, &files.current).map_err(|_| Failure::Download)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;

    #[test]
    fn tags_targets_and_asset_names_follow_the_release() {
        for good in ["v0.1.3", "v1.0.0-rc.1", "0.2_x"] {
            assert!(is_tag(good), "{good}");
        }
        for bad in ["", ".", "..", "../x", "v1/2", "v 1", &"v".repeat(65)] {
            assert!(!is_tag(bad), "{bad}");
        }
        let target = target().expect("the test platforms have release builds");
        let name = asset_name("v0.1.3", target);
        assert!(name.starts_with("cctg-v0.1.3-"));
        assert_eq!(name.ends_with(".exe"), cfg!(windows));
    }

    #[test]
    fn a_checksum_line_is_found_by_its_file_name() {
        let a = "ab".repeat(32);
        let b = "CD".repeat(32);
        let sums = format!(
            "{a}  cctg-v1-x86_64-unknown-linux-musl\n{b} *cctg-v1-x86_64-pc-windows-msvc.exe\nzz  cctg-v1-bad\n"
        );
        assert_eq!(
            expected_sum(&sums, "cctg-v1-x86_64-unknown-linux-musl"),
            Some(a)
        );
        assert_eq!(
            expected_sum(&sums, "cctg-v1-x86_64-pc-windows-msvc.exe"),
            Some(b.to_ascii_lowercase())
        );
        assert_eq!(expected_sum(&sums, "cctg-v1-bad"), None, "not a sha256");
        assert_eq!(expected_sum(&sums, "cctg-v1"), None);
        assert_eq!(expected_sum("", "x"), None);
    }

    #[test]
    fn only_https_or_this_machine_is_asked() {
        assert!(local_http("http://127.0.0.1:8080/r"));
        assert!(local_http("http://localhost/r"));
        assert!(!local_http("http://127.0.0.1.example.com/r"));
        assert!(!local_http("http://example.com/r"));
        assert!(!local_http("https://127.0.0.1/r"));
        // User info is not the host.
        assert!(!local_http("http://localhost:80@evil.example/r"));
        assert!(!local_http("http://127.0.0.1@evil.example/r"));
        assert!(!local_http("http://user@127.0.0.1/r"));
        assert!(!local_http("not a url"));
    }

    #[test]
    fn redirects_from_https_never_go_down_to_http() {
        assert!(follows(true, 0, "https"));
        assert!(!follows(true, 0, "http"), "https -> http is refused");
        assert!(
            follows(false, 0, "http"),
            "a loopback base may redirect on http"
        );
        assert!(!follows(true, 10, "https"), "at most 10 hops");
    }

    /// One HTTP answer of raw bytes on loopback; the connection closes
    /// after it.
    async fn answer_once(raw: Vec<u8>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://127.0.0.1:{}/f",
            listener.local_addr().unwrap().port()
        );
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => return,
                    Ok(read) => request.extend_from_slice(&buffer[..read]),
                }
            }
            let _ = stream.write_all(&raw).await;
        });
        url
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    #[tokio::test]
    async fn a_body_cut_short_or_over_the_cap_is_a_failed_download() {
        let whole =
            answer_once(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".to_vec()).await;
        assert_eq!(get(&client(), &whole, 16).await, Ok(b"hello".to_vec()));
        // The connection drops in the middle of the binary.
        let cut = answer_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\nMZ half"
                .to_vec(),
        )
        .await;
        assert_eq!(
            get(&client(), &cut, MAX_BINARY).await,
            Err(Failure::Download)
        );
        // No Content-Length: the running count stops it.
        let chunks: &[u8] = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n10\r\n0123456789abcdef\r\n10\r\n0123456789abcdef\r\n0\r\n\r\n";
        let fits = answer_once(chunks.to_vec()).await;
        assert_eq!(
            get(&client(), &fits, 32).await.map(|body| body.len()),
            Ok(32)
        );
        let chunked = answer_once(chunks.to_vec()).await;
        assert_eq!(get(&client(), &chunked, 20).await, Err(Failure::Download));
        let told = answer_once(b"HTTP/1.1 200 OK\r\nContent-Length: 21\r\n\r\n".to_vec()).await;
        assert_eq!(get(&client(), &told, 20).await, Err(Failure::Download));
        let missing =
            answer_once(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec()).await;
        assert_eq!(get(&client(), &missing, 20).await, Err(Failure::Missing));
    }

    #[tokio::test]
    async fn a_plain_http_base_elsewhere_is_refused_without_a_request() {
        let dir = TempDir::new("download-http");
        let exe = dir.path().join("cctg.exe");
        std::fs::write(&exe, "old").unwrap();
        assert_eq!(
            fetch("http://example.invalid/r", "v1", &exe, &Arc::default()).await,
            Err(Failure::Download)
        );
        assert_eq!(std::fs::read(&exe).unwrap(), b"old");
    }

    #[test]
    fn the_swap_replaces_the_file_and_leaves_nothing_behind() {
        let dir = TempDir::new("download-install");
        let name = format!("cctg{}", std::env::consts::EXE_SUFFIX);
        let exe = dir.path().join(&name);
        std::fs::write(&exe, "old").unwrap();
        let sum = sha256_hex(b"new");
        let later = Instant::now() + TIMEOUT;
        let gate = Gate::default();
        assert_eq!(
            install(&exe, b"new", &sum, later, &gate),
            Ok(Fetched::Installed)
        );
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        let files = Files::new(dir.path(), &name);
        assert!(!files.part.exists());
        assert_eq!(files.old.exists(), cfg!(windows), "the old exe set aside");
        // Already there: nothing written.
        assert_eq!(
            install(&exe, b"new", &sum, later, &Gate::default()),
            Ok(Fetched::Present)
        );
        // Once the swap began, the exit waits for it.
        assert!(!gate.close());
    }

    #[test]
    fn no_swap_begins_after_the_deadline_or_the_exit() {
        let dir = TempDir::new("download-late");
        let name = format!("cctg{}", std::env::consts::EXE_SUFFIX);
        let exe = dir.path().join(&name);
        std::fs::write(&exe, "old").unwrap();
        let sum = sha256_hex(b"new");
        let files = Files::new(dir.path(), &name);
        let untouched = || {
            assert_eq!(std::fs::read(&exe).unwrap(), b"old");
            assert!(!files.part.exists() && !files.old.exists());
        };
        let past = Instant::now();
        assert_eq!(
            install(&exe, b"new", &sum, past, &Gate::default()),
            Err(Failure::Download)
        );
        untouched();
        let closed = Gate::default();
        assert!(closed.close(), "nothing began: the exit need not wait");
        let later = Instant::now() + TIMEOUT;
        assert_eq!(
            install(&exe, b"new", &sum, later, &closed),
            Err(Failure::Download)
        );
        untouched();
        // The deploy lock is held elsewhere: the wait ends with the gate.
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&files.lock)
            .unwrap();
        lock.try_lock().unwrap();
        let started = Instant::now();
        assert_eq!(
            install(&exe, b"new", &sum, later, &closed),
            Err(Failure::Download)
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        untouched();
    }

    #[test]
    fn a_cut_swap_gets_its_previous_binary_back() {
        let dir = TempDir::new("download-recover");
        let name = format!("cctg{}", std::env::consts::EXE_SUFFIX);
        let exe = dir.path().join(&name);
        let files = Files::new(dir.path(), &name);
        std::fs::write(&files.old, "previous").unwrap();
        recover_in(&exe);
        assert_eq!(std::fs::read(&exe).unwrap(), b"previous");
        assert!(!files.old.exists());
        // With a current file, `old` is left alone.
        std::fs::write(&files.old, "older").unwrap();
        recover_in(&exe);
        assert_eq!(std::fs::read(&exe).unwrap(), b"previous");
        assert_eq!(std::fs::read(&files.old).unwrap(), b"older");
    }
}
