//! Device enrollment (TASK-045): one-time join codes and per-device secrets.
//!
//! A join code is 16 Crockford base32 characters (80 random bits), shown as
//! `XXXX-XXXX-XXXX-XXXX`, good once and for [`CODE_TTL`]. `cctg hub code`
//! (another process, e.g. `docker compose exec hub cctg hub code`) and the
//! hub itself mint codes into `<state>/join/`: one file per code, named by
//! the sha256 of the code and holding only its expiry, so the state volume
//! never holds a usable code. The hub takes a code by removing its file:
//! of two exchanges of one code only one removal succeeds.
//!
//! A device secret is `cctgd_<id>_<64 hex>` (256 random bits); `<id>` (8
//! hex) is public and names the device. It goes where the shared secret
//! went (`CCTG_HUB_SECRET` of `device.env`, `hello`, `Authorization`), so
//! agents and hooks need no change. The hub keeps only its sha256 in
//! `<state>/devices.json`: a random 256-bit value needs no slow hash.
//!
//! [`Devices`] is what the listeners check secrets against: the shared
//! secret while it is on (`CCTG_SHARED_SECRET`, default on), and the
//! enrolled devices. A revoke takes effect at once: hooks are checked per
//! request, and every agent link of a device watches [`Devices::subscribe`].
//!
//! Nothing here logs or returns in an error a code or a secret.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::watch;
use tracing::warn;

use crate::wire::Secret;

/// How long a join code is good.
pub const CODE_TTL: Duration = Duration::from_secs(10 * 60);
/// Enrolled devices at most; a join beyond it is refused.
pub const MAX_DEVICES: usize = 32;
/// Unused, unexpired codes at most; minting one more fails.
pub const MAX_CODES: usize = 32;
pub const DEVICES_FILE: &str = "devices.json";
const DEVICES_TEMP: &str = "devices.json.tmp";
pub const CODES_DIR: &str = "join";
const VERSION: u32 = 1;
const SECRET_PREFIX: &str = "cctgd_";
const ID_LEN: usize = 8;
/// Characters of a code, without the dashes.
pub const CODE_LEN: usize = 16;
/// Crockford base32: no I, L, O, U.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// UTF-16-free: characters of a device name kept.
pub const NAME_LIMIT: usize = 32;
/// A stale temp file of a code another process was writing.
const STALE_TEMP: Duration = Duration::from_secs(60);

/// Who a secret belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Who {
    /// The shared secret of `CCTG_HUB_SECRET`.
    Shared,
    /// An enrolled device, by its id.
    Device(String),
}

/// One enrolled device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    /// 8 hex digits; public, in the device's secret too.
    pub id: String,
    pub name: String,
    /// Unix seconds.
    pub joined: u64,
    /// sha256 of the whole secret, hex.
    hash: String,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    #[serde(default)]
    devices: Vec<Device>,
}

/// A device as `/devices` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    pub id: String,
    pub name: String,
    pub joined: SystemTime,
    /// The last time its secret was taken since the hub started.
    pub seen: Option<SystemTime>,
}

/// What `/devices` shows about the shared secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedState {
    Off,
    /// On; the last time it was taken since the hub started.
    On(Option<SystemTime>),
}

/// A new device's secret. `Debug` never shows it.
pub struct Enrolled {
    pub id: String,
    pub name: String,
    pub secret: Secret,
}

impl std::fmt::Debug for Enrolled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Enrolled")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum JoinError {
    /// Unknown, used or expired: one answer for all three.
    #[error("the join code is not valid")]
    Refused,
    #[error("the hub has {MAX_DEVICES} devices already")]
    Full,
    #[error("cannot save {DEVICES_FILE} ({0:?})")]
    Io(io::ErrorKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MintError {
    #[error("{MAX_CODES} join codes are waiting to be used already; wait until they expire")]
    Full,
    /// No `join/` in the state directory: no hub has started with it.
    #[error("no hub has started with this state directory")]
    NoHub,
    #[error("cannot write the join code into the hub state directory ({0:?})")]
    Io(io::ErrorKind),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoadError {
    #[error("cannot read {DEVICES_FILE} ({0:?})")]
    Read(io::ErrorKind),
    /// No serde text: it would quote the file.
    #[error("{DEVICES_FILE} is not a valid device list; fix or move it away")]
    Invalid,
    #[error("{DEVICES_FILE} has version {0}, this hub reads version {VERSION}")]
    Version(u32),
    #[error("cannot create the {CODES_DIR} directory ({0:?})")]
    Codes(io::ErrorKind),
}

/// A revoke that took effect; `saved`: also in `devices.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revoked {
    pub name: String,
    pub saved: bool,
}

struct Inner {
    devices: Vec<Device>,
    seen: HashMap<String, SystemTime>,
    shared_seen: Option<SystemTime>,
}

struct Shared {
    /// `None`: in memory only (tests, a plain shared-secret setup).
    state_dir: Option<PathBuf>,
    shared: Option<Secret>,
    inner: Mutex<Inner>,
    changes: watch::Sender<u64>,
}

/// The secrets the hub takes. Cheap to clone.
#[derive(Clone)]
pub struct Devices(Arc<Shared>);

impl std::fmt::Debug for Devices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Devices(<secrets redacted>)")
    }
}

/// The shared secret alone: no device list, no join codes.
impl From<Secret> for Devices {
    fn from(secret: Secret) -> Self {
        Self::in_memory(Some(secret))
    }
}

impl Devices {
    fn in_memory(shared: Option<Secret>) -> Self {
        Self::with(None, shared, Vec::new())
    }

    fn with(state_dir: Option<PathBuf>, shared: Option<Secret>, devices: Vec<Device>) -> Self {
        let (changes, _) = watch::channel(0);
        Self(Arc::new(Shared {
            state_dir,
            shared,
            inner: Mutex::new(Inner {
                devices,
                seen: HashMap::new(),
                shared_seen: None,
            }),
            changes,
        }))
    }

    /// The devices of `state_dir`; `shared`: the shared secret, while it is
    /// on. A missing file is no devices; one that does not parse stops the
    /// hub (starting without it would lock every enrolled device out).
    /// Creates `<state_dir>/join/`, where [`mint_code`] finds this hub.
    pub fn open(state_dir: &Path, shared: Option<Secret>) -> Result<Self, LoadError> {
        std::fs::create_dir_all(state_dir.join(CODES_DIR))
            .map_err(|error| LoadError::Codes(error.kind()))?;
        let devices = match std::fs::read(state_dir.join(DEVICES_FILE)) {
            Ok(bytes) => {
                let stored: Stored =
                    serde_json::from_slice(&bytes).map_err(|_| LoadError::Invalid)?;
                if stored.version != VERSION {
                    return Err(LoadError::Version(stored.version));
                }
                let mut ids = std::collections::HashSet::new();
                let valid = stored.devices.iter().all(|device| {
                    is_id(&device.id)
                        && is_hex(&device.hash, 64)
                        && UNIX_EPOCH
                            .checked_add(Duration::from_secs(device.joined))
                            .is_some()
                        && ids.insert(&device.id)
                });
                if !valid {
                    return Err(LoadError::Invalid);
                }
                stored.devices
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(LoadError::Read(error.kind())),
        };
        Ok(Self::with(Some(state_dir.to_owned()), shared, devices))
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whose secret `offered` is; `None`: nobody's. Constant time over the
    /// secret bytes; the device id in it is public.
    pub fn check(&self, offered: &[u8]) -> Option<Who> {
        let now = SystemTime::now();
        if let Some(shared) = &self.0.shared
            && shared.matches(offered)
        {
            self.lock().shared_seen = Some(now);
            return Some(Who::Shared);
        }
        let id = device_id(offered)?;
        let hash = sha256_hex(offered);
        let mut inner = self.lock();
        let known = inner
            .devices
            .iter()
            .find(|device| device.id == id)
            .is_some_and(|device| bool::from(device.hash.as_bytes().ct_eq(hash.as_bytes())));
        if !known {
            return None;
        }
        inner.seen.insert(id.to_owned(), now);
        Some(Who::Device(id.to_owned()))
    }

    /// Whether `who` still gets in.
    pub fn is_active(&self, who: &Who) -> bool {
        match who {
            Who::Shared => self.0.shared.is_some(),
            Who::Device(id) => self.lock().devices.iter().any(|device| &device.id == id),
        }
    }

    /// Changes on every revoke; a link rechecks [`Devices::is_active`] then.
    /// Subscribe before [`Devices::check`]: a revoke in between is seen.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.0.changes.subscribe()
    }

    /// Takes `code` and enrolls a device named after `name` (cleaned). A
    /// code is spent by the attempt that takes it, also when the save then
    /// fails. Blocking file I/O.
    pub fn join(&self, code: &str, name: &str) -> Result<Enrolled, JoinError> {
        let dir = self.0.state_dir.as_deref().ok_or(JoinError::Refused)?;
        let code = normalize_code(code).ok_or(JoinError::Refused)?;
        if !take_code(&dir.join(CODES_DIR), &code, SystemTime::now()) {
            return Err(JoinError::Refused);
        }
        self.enroll(name)
    }

    fn enroll(&self, name: &str) -> Result<Enrolled, JoinError> {
        let mut inner = self.lock();
        if inner.devices.len() >= MAX_DEVICES {
            return Err(JoinError::Full);
        }
        let id = loop {
            let id = hex(&random::<4>());
            if !inner.devices.iter().any(|device| device.id == id) {
                break id;
            }
        };
        let token = format!("{SECRET_PREFIX}{id}_{}", hex(&random::<32>()));
        let device = Device {
            id: id.clone(),
            name: clean_name(name),
            joined: unix(SystemTime::now()),
            hash: sha256_hex(token.as_bytes()),
        };
        let name = device.name.clone();
        inner.devices.push(device);
        if let Err(error) = self.save(&inner.devices) {
            inner.devices.pop();
            return Err(JoinError::Io(error.kind()));
        }
        let secret =
            Secret::parse(&token).expect("a device secret is visible ASCII, 79 characters");
        Ok(Enrolled { id, name, secret })
    }

    /// Removes device `id` at once; `None`: no such device. Blocking file
    /// I/O.
    pub fn revoke(&self, id: &str) -> Option<Revoked> {
        let mut inner = self.lock();
        let at = inner.devices.iter().position(|device| device.id == id)?;
        let device = inner.devices.remove(at);
        inner.seen.remove(id);
        let saved = match self.save(&inner.devices) {
            Ok(()) => true,
            Err(error) => {
                warn!(device = id, kind = ?error.kind(), "revoked device not saved; it comes back at the next start");
                false
            }
        };
        drop(inner);
        self.0.changes.send_modify(|count| *count += 1);
        Some(Revoked {
            name: device.name,
            saved,
        })
    }

    /// The devices, oldest first, and the shared secret's state.
    pub fn list(&self) -> (Vec<Listed>, SharedState) {
        let inner = self.lock();
        let devices = inner
            .devices
            .iter()
            .map(|device| Listed {
                id: device.id.clone(),
                name: device.name.clone(),
                // Checked at load; a hand-edited value never panics here.
                joined: UNIX_EPOCH
                    .checked_add(Duration::from_secs(device.joined))
                    .unwrap_or(UNIX_EPOCH),
                seen: inner.seen.get(&device.id).copied(),
            })
            .collect();
        let shared = match self.0.shared {
            Some(_) => SharedState::On(inner.shared_seen),
            None => SharedState::Off,
        };
        (devices, shared)
    }

    /// The name of device `id`.
    pub fn name(&self, id: &str) -> Option<String> {
        self.lock()
            .devices
            .iter()
            .find(|device| device.id == id)
            .map(|device| device.name.clone())
    }

    /// Temp file, fsync, rename; only the owner may read it.
    fn save(&self, devices: &[Device]) -> io::Result<()> {
        let Some(dir) = &self.0.state_dir else {
            return Ok(());
        };
        let bytes = serde_json::to_vec_pretty(&Stored {
            version: VERSION,
            devices: devices.to_vec(),
        })
        .expect("the device list always serializes");
        let temp = dir.join(DEVICES_TEMP);
        let mut file = private_file(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, dir.join(DEVICES_FILE))
    }
}

fn private_file(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// The id of a device secret, when `offered` has that form.
fn device_id(offered: &[u8]) -> Option<&str> {
    let rest = std::str::from_utf8(offered)
        .ok()?
        .strip_prefix(SECRET_PREFIX)?;
    let (id, secret) = rest.split_once('_')?;
    (is_id(id) && is_hex(secret, 64)).then_some(id)
}

/// Whether `secret` has the form of a device secret (`cctg doctor`, `cctg
/// join`); its id then.
pub fn secret_device_id(secret: &Secret) -> Option<&str> {
    device_id(secret.expose().as_bytes())
}

fn is_id(id: &str) -> bool {
    is_hex(id, ID_LEN)
}

fn is_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex(aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, bytes).as_ref())
}

/// `N` bytes from the system's secure generator.
fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    aws_lc_rs::rand::fill(&mut bytes).expect("the system random generator works");
    bytes
}

fn unix(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// A device name for `/devices`: whitespace becomes one space, control and
/// invisible formatting characters go, at most [`NAME_LIMIT`] characters;
/// `device` when nothing is left.
pub fn clean_name(raw: &str) -> String {
    let invisible = |c: char| {
        matches!(c, '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{FEFF}'
            | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}')
    };
    let kept: String = raw
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|&c| !c.is_control() && !invisible(c))
        .collect();
    let name: String = kept
        .split(' ')
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(NAME_LIMIT)
        .collect();
    if name.trim().is_empty() {
        "device".to_owned()
    } else {
        name.trim().to_owned()
    }
}

// ------------------------------------------------------------ join codes

/// A fresh code as it is shown: `XXXX-XXXX-XXXX-XXXX`.
fn new_code() -> String {
    let bytes = random::<10>();
    let mut bits = 0u128;
    for byte in bytes {
        bits = (bits << 8) | u128::from(byte);
    }
    let chars: Vec<u8> = (0..CODE_LEN)
        .map(|index| ALPHABET[((bits >> (75 - 5 * index)) & 31) as usize])
        .collect();
    chars
        .chunks(4)
        .map(|group| std::str::from_utf8(group).expect("the alphabet is ASCII"))
        .collect::<Vec<_>>()
        .join("-")
}

/// A code as typed: any case, dashes and spaces anywhere, `O` for `0` and
/// `I`/`L` for `1` (Crockford). `None`: not a code.
pub fn normalize_code(typed: &str) -> Option<String> {
    let mut code = String::with_capacity(CODE_LEN);
    for c in typed.chars() {
        let c = match c.to_ascii_uppercase() {
            '-' | ' ' => continue,
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        };
        if !c.is_ascii() || !ALPHABET.contains(&(c as u8)) {
            return None;
        }
        code.push(c);
    }
    (code.len() == CODE_LEN).then_some(code)
}

#[derive(Serialize, Deserialize)]
struct CodeFile {
    /// Unix seconds.
    expires: u64,
}

fn code_path(dir: &Path, normalized: &str) -> PathBuf {
    dir.join(format!("{}.json", sha256_hex(normalized.as_bytes())))
}

/// Mints a join code into `<state_dir>/join/`, good for [`CODE_TTL`] from
/// `now`. Expired codes are swept first. `join/` must be there already
/// ([`Devices::open`] of a hub makes it): a code minted anywhere else would
/// never be taken. Blocking file I/O.
pub fn mint_code(state_dir: &Path, now: SystemTime) -> Result<String, MintError> {
    let dir = state_dir.join(CODES_DIR);
    let io = |error: io::Error| MintError::Io(error.kind());
    if !dir.is_dir() {
        return Err(MintError::NoHub);
    }
    if sweep(&dir, now) >= MAX_CODES {
        return Err(MintError::Full);
    }
    let code = new_code();
    let normalized = normalize_code(&code).expect("a new code is a code");
    let path = code_path(&dir, &normalized);
    let temp = path.with_extension("tmp");
    let body = serde_json::to_vec(&CodeFile {
        expires: unix(now + CODE_TTL),
    })
    .expect("a code file always serializes");
    let mut file = private_file(&temp).map_err(io)?;
    file.write_all(&body).map_err(io)?;
    file.sync_all().map_err(io)?;
    drop(file);
    std::fs::rename(&temp, &path).map_err(io)?;
    Ok(code)
}

/// Removes expired and unreadable codes and stale temp files; the number
/// of codes left.
fn sweep(dir: &Path, now: SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut left = 0;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("json") => {
                if deadline(&path, now).is_some_and(|until| unix(now) < until) {
                    left += 1;
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
            Some("tmp") => {
                let stale = entry
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|at| now.duration_since(at).ok())
                    .is_some_and(|age| age > STALE_TEMP);
                if stale {
                    let _ = std::fs::remove_file(&path);
                }
            }
            _ => {}
        }
    }
    left
}

/// Until when a code file is good (Unix seconds): its `expires`, but at
/// most [`CODE_TTL`] after the file was written (its mtime); a deadline
/// more than [`CODE_TTL`] after `now` (a minting clock that ran ahead, a
/// hand-written file) is no deadline at all (0). `None`: unreadable.
fn deadline(path: &Path, now: SystemTime) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    let expires = serde_json::from_slice::<CodeFile>(&bytes).ok()?.expires;
    let written = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()?;
    let ttl = CODE_TTL.as_secs();
    let until = expires.min(unix(written).saturating_add(ttl));
    Some(if until > unix(now).saturating_add(ttl) {
        0
    } else {
        until
    })
}

/// Spends `normalized`: true when it was minted, unused and unexpired.
/// Takes run one at a time: on Windows two racing `remove_file` calls of
/// one file can both succeed (each deletes through its own handle), so the
/// removal alone would let one code enroll two devices. One hub takes
/// codes from its state directory. An expired code is removed too.
fn take_code(dir: &Path, normalized: &str, now: SystemTime) -> bool {
    static TAKING: Mutex<()> = Mutex::new(());
    let _one_at_a_time = TAKING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let path = code_path(dir, normalized);
    let Some(until) = deadline(&path, now) else {
        return false;
    };
    let removed = std::fs::remove_file(&path).is_ok();
    sweep(dir, now);
    removed && unix(now) < until
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;

    const SHARED: &str = "shared-secret-0123456789";

    fn shared() -> Secret {
        Secret::parse(SHARED).unwrap()
    }

    #[test]
    fn codes_are_crockford_groups_and_normalize() {
        let code = new_code();
        assert_eq!(code.len(), CODE_LEN + 3, "{code}");
        assert_eq!(code.matches('-').count(), 3);
        let normalized = normalize_code(&code).unwrap();
        assert_eq!(normalized, code.replace('-', ""));
        assert_ne!(new_code(), code, "fresh randomness");
        assert_eq!(
            normalize_code("abcd-efgh-jkmn-pqrs").as_deref(),
            Some("ABCDEFGHJKMNPQRS")
        );
        assert_eq!(
            normalize_code(" o0il 1234 5678 9abc ").as_deref(),
            Some("0011123456789ABC")
        );
        for bad in [
            "",
            "ABCD-EFGH-JKMN-PQR",
            "ABCD-EFGH-JKMN-PQRST",
            "ABCD-EFGH-JKMN-PQRU",
            "ABCD-EFGH-JKMN-PQRЖ",
        ] {
            assert_eq!(normalize_code(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_code_is_good_once_and_only_until_it_expires() {
        let dir = TempDir::new("devices-codes");
        let devices = Devices::open(dir.path(), None).unwrap();
        let code = mint_code(dir.path(), SystemTime::now()).unwrap();
        // The state directory holds no usable code.
        for entry in std::fs::read_dir(dir.path().join(CODES_DIR)).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(entry.path()).unwrap();
            let bare = code.replace('-', "");
            assert!(
                !name.contains(&bare) && !text.contains(&bare),
                "{name} {text}"
            );
        }
        let joined = devices.join(&code.to_lowercase(), "laptop").unwrap();
        assert_eq!(joined.name, "laptop");
        assert_eq!(
            devices.join(&code, "again").unwrap_err(),
            JoinError::Refused
        );

        let past = SystemTime::now() - CODE_TTL - Duration::from_secs(1);
        let old = mint_code(dir.path(), past).unwrap();
        assert_eq!(devices.join(&old, "late").unwrap_err(), JoinError::Refused);
        assert_eq!(
            devices.join("0000-0000-0000-0000", "x").unwrap_err(),
            JoinError::Refused
        );
        assert_eq!(
            devices.join("not a code", "x").unwrap_err(),
            JoinError::Refused
        );
        assert_eq!(devices.list().0.len(), 1);
        assert_eq!(
            std::fs::read_dir(dir.path().join(CODES_DIR))
                .unwrap()
                .count(),
            0,
            "used and expired codes are gone"
        );
    }

    #[test]
    fn minting_stops_at_the_cap_and_expired_codes_free_places() {
        let dir = TempDir::new("devices-mint-cap");
        Devices::open(dir.path(), None).unwrap();
        let past = SystemTime::now() - CODE_TTL - Duration::from_secs(1);
        for _ in 0..MAX_CODES {
            mint_code(dir.path(), past).unwrap();
        }
        for _ in 0..MAX_CODES {
            mint_code(dir.path(), SystemTime::now()).unwrap();
        }
        assert_eq!(
            mint_code(dir.path(), SystemTime::now()).unwrap_err(),
            MintError::Full
        );
    }

    #[test]
    fn a_code_is_minted_only_where_a_hub_keeps_its_state() {
        let dir = TempDir::new("devices-mint-nohub");
        assert_eq!(
            mint_code(dir.path(), SystemTime::now()).unwrap_err(),
            MintError::NoHub
        );
        assert_eq!(
            mint_code(&dir.path().join("missing"), SystemTime::now()).unwrap_err(),
            MintError::NoHub
        );
        assert!(!dir.path().join(CODES_DIR).exists(), "nothing created");
        Devices::open(dir.path(), None).unwrap();
        assert!(mint_code(dir.path(), SystemTime::now()).is_ok());
    }

    #[test]
    fn a_code_lives_at_most_ten_minutes_whatever_its_file_says() {
        let dir = TempDir::new("devices-code-cap");
        Devices::open(dir.path(), None).unwrap();
        let codes = dir.path().join(CODES_DIR);
        let forever = |code: &str| {
            let path = code_path(&codes, &normalize_code(code).unwrap());
            std::fs::write(&path, format!(r#"{{"expires":{}}}"#, u64::MAX)).unwrap();
            path
        };
        // Taken in time: good, its own expiry notwithstanding.
        let code = mint_code(dir.path(), SystemTime::now()).unwrap();
        forever(&code);
        let normalized = normalize_code(&code).unwrap();
        assert!(take_code(&codes, &normalized, SystemTime::now()));
        // Taken later than 10 minutes after it was written.
        let code = mint_code(dir.path(), SystemTime::now()).unwrap();
        forever(&code);
        let later = SystemTime::now() + CODE_TTL + Duration::from_secs(2);
        assert!(!take_code(&codes, &normalize_code(&code).unwrap(), later));
        // Written more than 10 minutes ago.
        let code = mint_code(dir.path(), SystemTime::now()).unwrap();
        let path = forever(&code);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::now() - CODE_TTL - Duration::from_secs(2))
            .unwrap();
        assert!(!take_code(
            &codes,
            &normalize_code(&code).unwrap(),
            SystemTime::now()
        ));
        // Minted by a clock that ran ahead.
        let ahead = SystemTime::now() + CODE_TTL * 3;
        let code = mint_code(dir.path(), ahead).unwrap();
        let path = code_path(&codes, &normalize_code(&code).unwrap());
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(ahead)
            .unwrap();
        assert!(!take_code(
            &codes,
            &normalize_code(&code).unwrap(),
            SystemTime::now()
        ));
    }

    #[test]
    fn of_two_racing_takes_of_one_code_one_wins() {
        let dir = TempDir::new("devices-code-race");
        Devices::open(dir.path(), None).unwrap();
        let codes = dir.path().join(CODES_DIR);
        for _ in 0..20 {
            let code = normalize_code(&mint_code(dir.path(), SystemTime::now()).unwrap()).unwrap();
            let start = std::sync::Barrier::new(2);
            let won = std::thread::scope(|scope| {
                let takes: Vec<_> = (0..2)
                    .map(|_| {
                        scope.spawn(|| {
                            start.wait();
                            take_code(&codes, &code, SystemTime::now())
                        })
                    })
                    .collect();
                takes
                    .into_iter()
                    .map(|take| take.join().unwrap())
                    .filter(|&won| won)
                    .count()
            });
            assert_eq!(won, 1);
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_device_list_and_codes_are_the_owners_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("devices-mode");
        let devices = Devices::open(dir.path(), None).unwrap();
        let code = mint_code(dir.path(), SystemTime::now()).unwrap();
        for entry in std::fs::read_dir(dir.path().join(CODES_DIR)).unwrap() {
            let mode = entry.unwrap().metadata().unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        devices.join(&code, "box").unwrap();
        let mode = std::fs::metadata(dir.path().join(DEVICES_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_code_of_an_in_memory_book_is_always_refused() {
        let devices = Devices::from(shared());
        assert_eq!(
            devices.join("0000-0000-0000-0000", "x").unwrap_err(),
            JoinError::Refused
        );
    }

    #[test]
    fn a_device_secret_gets_in_and_only_its_hash_is_kept() {
        let dir = TempDir::new("devices-enroll");
        let devices = Devices::open(dir.path(), Some(shared())).unwrap();
        let code = mint_code(dir.path(), SystemTime::now()).unwrap();
        let joined = devices.join(&code, " my\tlaptop\u{202E} ").unwrap();
        let secret = joined.secret.expose().to_owned();
        assert!(
            secret.starts_with("cctgd_") && secret.len() == 79,
            "{secret}"
        );
        assert_eq!(secret_device_id(&joined.secret), Some(joined.id.as_str()));
        assert_eq!(joined.name, "my laptop");
        assert!(!format!("{joined:?}").contains(&secret));
        assert_eq!(
            devices.check(secret.as_bytes()),
            Some(Who::Device(joined.id.clone()))
        );
        assert_eq!(devices.check(SHARED.as_bytes()), Some(Who::Shared));
        // A wrong secret of the right shape and id.
        let last = if secret.ends_with('0') { '1' } else { '0' };
        let wrong = format!("{}{last}", &secret[..secret.len() - 1]);
        assert_eq!(devices.check(wrong.as_bytes()), None);
        assert_eq!(devices.check(b"cctgd_nothexxx_00"), None);

        let file = std::fs::read_to_string(dir.path().join(DEVICES_FILE)).unwrap();
        assert!(
            !file.contains(&secret) && !file.contains(&secret[15..]),
            "{file}"
        );
        assert!(file.contains(&sha256_hex(secret.as_bytes())), "{file}");

        // A restarted hub knows the device; the shared secret can be off.
        let reopened = Devices::open(dir.path(), None).unwrap();
        assert_eq!(
            reopened.check(secret.as_bytes()),
            Some(Who::Device(joined.id.clone()))
        );
        assert_eq!(reopened.check(SHARED.as_bytes()), None);
        assert!(!reopened.is_active(&Who::Shared));
        let (listed, state) = reopened.list();
        assert_eq!(state, SharedState::Off);
        assert_eq!(listed[0].name, "my laptop");
        assert!(listed[0].seen.is_some());
    }

    #[tokio::test]
    async fn a_revoke_shuts_the_device_out_at_once_and_is_announced() {
        let dir = TempDir::new("devices-revoke");
        let devices = Devices::open(dir.path(), None).unwrap();
        let code = mint_code(dir.path(), SystemTime::now()).unwrap();
        let joined = devices.join(&code, "old box").unwrap();
        let who = devices.check(joined.secret.expose().as_bytes()).unwrap();
        let mut changes = devices.subscribe();
        assert!(devices.is_active(&who));
        assert_eq!(
            devices.revoke(&joined.id),
            Some(Revoked {
                name: "old box".into(),
                saved: true
            })
        );
        assert!(changes.has_changed().unwrap());
        changes.mark_unchanged();
        assert!(!devices.is_active(&who));
        assert_eq!(devices.check(joined.secret.expose().as_bytes()), None);
        assert_eq!(devices.revoke(&joined.id), None);
        assert!(!changes.has_changed().unwrap());
        let reopened = Devices::open(dir.path(), None).unwrap();
        assert!(reopened.list().0.is_empty());
    }

    #[test]
    fn a_full_book_refuses_and_a_bad_file_stops_the_start() {
        let dir = TempDir::new("devices-full");
        let devices = Devices::open(dir.path(), None).unwrap();
        for index in 0..MAX_DEVICES {
            devices.enroll(&format!("d{index}")).unwrap();
        }
        let code = mint_code(dir.path(), SystemTime::now()).unwrap();
        assert_eq!(
            devices.join(&code, "one more").unwrap_err(),
            JoinError::Full
        );

        std::fs::write(dir.path().join(DEVICES_FILE), "{ not json").unwrap();
        assert_eq!(
            Devices::open(dir.path(), None).err(),
            Some(LoadError::Invalid)
        );
        std::fs::write(
            dir.path().join(DEVICES_FILE),
            r#"{"version":9,"devices":[]}"#,
        )
        .unwrap();
        assert_eq!(
            Devices::open(dir.path(), None).err(),
            Some(LoadError::Version(9))
        );
        let twice = format!(
            r#"{{"version":1,"devices":[{0},{0}]}}"#,
            r#"{"id":"0123abcd","name":"x","joined":1,"hash":"0000000000000000000000000000000000000000000000000000000000000000"}"#
        );
        std::fs::write(dir.path().join(DEVICES_FILE), twice).unwrap();
        assert_eq!(
            Devices::open(dir.path(), None).err(),
            Some(LoadError::Invalid)
        );
        // A join time no clock can hold: refused at load, no panic later.
        let far = format!(
            r#"{{"version":1,"devices":[{{"id":"0123abcd","name":"x","joined":{},"hash":"{}"}}]}}"#,
            u64::MAX,
            "0".repeat(64)
        );
        std::fs::write(dir.path().join(DEVICES_FILE), far).unwrap();
        assert_eq!(
            Devices::open(dir.path(), None).err(),
            Some(LoadError::Invalid)
        );
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  "), "device");
        assert_eq!(clean_name("a\u{0}b\nc"), "ab c");
        assert_eq!(clean_name(&"x".repeat(40)).chars().count(), NAME_LIMIT);
        assert_eq!(clean_name("Мой ноут"), "Мой ноут");
    }
}
