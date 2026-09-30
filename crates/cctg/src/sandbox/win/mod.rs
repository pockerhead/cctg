//! The native Windows folder sandbox (TASK-089, part C of TASK-087).
//!
//! Claude Code has no OS command sandbox on native Windows, so cctg draws the
//! boundary itself. A marked folder's profile points
//! `CLAUDE_CODE_SHELL_PREFIX` at the shim `~/.cctg/bin/cctg-sandbox-exec`,
//! which runs `cctg sandbox-exec "$1"`. The broker ([`exec`]) passes its own
//! two calls straight through ([`super::prefix::own_call`]) and runs every
//! other command under a per-folder hidden local account, on a private
//! desktop, in a kill-on-close job, with a restricted token. Isolation rests
//! on one account per marked folder (a "slot"): `cctg sandbox on` grants that
//! slot's SID a modify ACE on the folder tree, `sandbox off` retires it, and
//! the next `sandbox-install` recreates the account with a fresh SID.
//!
//! One UAC prompt at `cctg sandbox-install`; nothing after that needs admin.
//! The whole tree is `#[cfg(windows)]`.

use std::path::{Path, PathBuf};

use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY, REG_DWORD, REG_SZ, RegCloseKey,
    RegOpenKeyExW, RegQueryValueExW,
};

use self::util::{from_wide, wide};

pub mod acl;
pub mod cred;
pub mod desktop;
pub mod exec;
pub mod install;
pub mod job;
pub mod runner;
pub mod slots;
pub mod token;
pub mod user;
pub mod util;

/// The local group that holds every sandbox slot account.
pub const GROUP: &str = "cctg-sandbox";
/// Default number of slot accounts (one per marked folder).
pub const DEFAULT_SLOTS: u32 = 8;
/// The most slots `--slots` accepts.
pub const MAX_SLOTS: u32 = 32;
/// The install schema. The broker refuses a folder whose mark is a different
/// version and asks for `cctg sandbox-install`.
pub const SETUP_VERSION: u32 = super::SETUP_VERSION;
/// The broker's exit code when it did not run the command (a refusal before
/// launch). Distinct from any command's own code.
pub const BROKER_FAILED: i32 = super::BROKER_FAILED;

/// The registry key holding the install mark (Admins write, Users read; no
/// secrets). `HKLM\SOFTWARE\cctg\sandbox`.
pub(crate) const MARK_KEY_LOCAL: &str = r"SOFTWARE\cctg\sandbox";

/// The env var (profile and broker) naming the mark root of the folder.
pub const MARK_VAR: &str = "CCTG_SANDBOX_MARK";
/// The env var naming the Git Bash `bash.exe` the broker launches.
pub const BASH_VAR: &str = "CCTG_SANDBOX_BASH";
/// The env var the broker sets on the child holding the command line to run.
pub const COMMAND_VAR: &str = "CCTG_SANDBOX_COMMAND";

/// Slot `k`'s account name (`cctg-sandbox-1`, …).
pub fn slot_user(k: u32) -> String {
    format!("{GROUP}-{k}")
}

/// `<home>/.cctg/bin/cctg-sandbox-exec`, where `install.sh` writes the shim
/// next to `cctg.exe`. Computed from home, not `current_exe`, which may be a
/// link in `cctg-workers/` or a test binary.
pub fn shim_path(home: &Path) -> PathBuf {
    super::win_shim_path(home)
}

/// `<home>/.cctg/sandbox/win`: slot map, lock, credential blobs, the install
/// request and log. Under the real user's profile ACL.
pub fn win_dir(home: &Path) -> PathBuf {
    super::sandbox_home(home).join("win")
}

/// The install mark read from `HKLM\SOFTWARE\cctg\sandbox`.
#[derive(Debug, Clone)]
pub struct Mark {
    pub version: u32,
    pub slots: u32,
    pub owner_sid: String,
    pub group_sid: String,
    /// Slot number -> that slot account's SID.
    pub slot_sids: Vec<(u32, String)>,
}

impl Mark {
    /// The SID recorded for slot `k`.
    pub fn slot_sid(&self, k: u32) -> Option<&str> {
        self.slot_sids
            .iter()
            .find(|(slot, _)| *slot == k)
            .map(|(_, sid)| sid.as_str())
    }
}

/// Reads the install mark, or `None` when the sandbox is not installed (the
/// key or a required value is absent). A malformed value reads as absent.
pub fn read_mark() -> Option<Mark> {
    let key = RegKey::open_read(MARK_KEY_LOCAL)?;
    let version = key.dword("SetupVersion")?;
    let slots = key.dword("Slots")?;
    let owner_sid = key.sz("OwnerSid")?;
    let group_sid = key.sz("GroupSid")?;
    let slot_sids = (1..=slots)
        .filter_map(|k| key.sz(&format!("Slot{k}Sid")).map(|sid| (k, sid)))
        .collect();
    Some(Mark {
        version,
        slots,
        owner_sid,
        group_sid,
        slot_sids,
    })
}

/// A read-only `HKLM` key handle, closed on drop. 64-bit view always.
pub(crate) struct RegKey(HKEY);

impl RegKey {
    fn open_read(subkey: &str) -> Option<Self> {
        let w = wide(subkey);
        let mut hkey: HKEY = std::ptr::null_mut();
        // SAFETY: valid key and NUL-terminated subkey; hkey receives the
        // opened handle on ERROR_SUCCESS.
        let code = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                w.as_ptr(),
                0,
                KEY_READ | KEY_WOW64_64KEY,
                &mut hkey,
            )
        };
        (code == 0 && !hkey.is_null()).then_some(Self(hkey))
    }

    fn dword(&self, name: &str) -> Option<u32> {
        let w = wide(name);
        let mut kind = 0u32;
        let mut data = [0u8; 4];
        let mut len = 4u32;
        // SAFETY: the buffer is 4 bytes; RegQueryValueExW writes at most `len`.
        let code = unsafe {
            RegQueryValueExW(
                self.0,
                w.as_ptr(),
                std::ptr::null(),
                &mut kind,
                data.as_mut_ptr(),
                &mut len,
            )
        };
        (code == 0 && kind == REG_DWORD && len == 4).then(|| u32::from_ne_bytes(data))
    }

    fn sz(&self, name: &str) -> Option<String> {
        let w = wide(name);
        let mut kind = 0u32;
        let mut len = 0u32;
        // Size query.
        // SAFETY: a null data pointer with a length-out asks for the size.
        let code = unsafe {
            RegQueryValueExW(
                self.0,
                w.as_ptr(),
                std::ptr::null(),
                &mut kind,
                std::ptr::null_mut(),
                &mut len,
            )
        };
        if code != 0 || kind != REG_SZ || len == 0 || !len.is_multiple_of(2) {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        // SAFETY: `buf` is `len` bytes; RegQueryValueExW writes at most `len`.
        let code = unsafe {
            RegQueryValueExW(
                self.0,
                w.as_ptr(),
                std::ptr::null(),
                &mut kind,
                buf.as_mut_ptr(),
                &mut len,
            )
        };
        if code != 0 {
            return None;
        }
        let wide: Vec<u16> = buf
            .chunks_exact(2)
            .map(|c| u16::from_ne_bytes([c[0], c[1]]))
            .collect();
        Some(from_wide(wide.as_ptr()))
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: the handle came from RegOpenKeyExW and drop runs once.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

/// The SID (SDDL string) of the current process's user.
pub fn current_user_sid() -> anyhow::Result<String> {
    use std::ffi::c_void;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_USER, TokenUser};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // TOKEN_QUERY = 8.
    let mut token: windows_sys::Win32::Foundation::HANDLE = std::ptr::null_mut();
    // SAFETY: current-process pseudo-handle; token receives the opened handle.
    util::ok(
        unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut token) },
        "OpenProcessToken",
    )?;
    let token = util::OwnedHandle(token);
    let mut len = 0u32;
    // SAFETY: sizing call; a null buffer with length 0 returns the size.
    unsafe {
        let _ = GetTokenInformation(token.raw(), TokenUser, std::ptr::null_mut(), 0, &mut len);
    }
    if len == 0 {
        anyhow::bail!("GetTokenInformation(TokenUser) sizing returned 0");
    }
    let mut buf = vec![0u8; len as usize];
    // SAFETY: `buf` is `len` bytes; GetTokenInformation writes at most `len`.
    util::ok(
        unsafe {
            GetTokenInformation(
                token.raw(),
                TokenUser,
                buf.as_mut_ptr() as *mut c_void,
                len,
                &mut len,
            )
        },
        "GetTokenInformation(TokenUser)",
    )?;
    // SAFETY: `buf` holds a TOKEN_USER whose User.Sid points into it.
    let sid = unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid };
    util::sid_to_string(sid).ok_or_else(|| anyhow::anyhow!("ConvertSidToStringSidW(user)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_names_and_paths() {
        assert_eq!(slot_user(1), "cctg-sandbox-1");
        assert_eq!(slot_user(8), "cctg-sandbox-8");
        let home = Path::new(r"C:\Users\u");
        assert_eq!(
            shim_path(home),
            home.join(".cctg").join("bin").join("cctg-sandbox-exec")
        );
        assert_eq!(
            win_dir(home),
            home.join(".cctg").join("sandbox").join("win")
        );
    }

    #[test]
    fn the_current_user_sid_resolves() {
        let sid = current_user_sid().expect("user sid");
        assert!(sid.starts_with("S-1-"), "{sid}");
    }
}
