//! Small Win32 helpers shared by the sandbox modules: wide strings, `\\?\`
//! long paths, RAII over handles, SIDs and security descriptors, and the
//! last-error check the raw `windows-sys` calls need (they return `BOOL` /
//! `WIN32_ERROR`, never `Result`).
//!
//! Source of the RAII shape and the SID helpers by intent:
//! `srtwin/src_util.rs`, `srtwin/src_sid.rs` (Apache-2.0). Not copied
//! line by line; adapted from the `windows` crate to raw `windows-sys`.

use std::ffi::c_void;
use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, LocalFree, WIN32_ERROR};
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW};
use windows_sys::Win32::Security::{GetLengthSid, IsValidSid, PSID};
use windows_sys::core::{PCWSTR, PWSTR};

/// A UTF-16, NUL-terminated copy of `s` for a `PCWSTR`/`PWSTR` argument.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `path` as a NUL-terminated `\\?\` wide string, so every ACL and tree API
/// takes the long-path form. A plain drive path gains `\\?\`; a UNC path
/// gains `\\?\UNC\`; an already-verbatim path is left as is.
pub fn verbatim_wide(path: &Path) -> Vec<u16> {
    let text = path.to_string_lossy();
    let text = text.as_ref();
    let out = if text.starts_with(r"\\?\") || text.starts_with(r"\\.\") {
        text.to_owned()
    } else if let Some(unc) = text.strip_prefix(r"\\") {
        format!(r"\\?\UNC\{unc}")
    } else {
        format!(r"\\?\{text}")
    };
    wide(&out)
}

/// The last Win32 error code.
pub fn last_error() -> u32 {
    // SAFETY: GetLastError has no preconditions.
    unsafe { GetLastError() }
}

/// `Ok(())` when `ok` (a Win32 `BOOL`) is non-zero, else an error carrying the
/// call name and `GetLastError`. Never quotes any argument.
pub fn ok(ok: windows_sys::core::BOOL, what: &str) -> anyhow::Result<()> {
    if ok != 0 {
        Ok(())
    } else {
        Err(anyhow::anyhow!("{what} failed (os error {})", last_error()))
    }
}

/// `Ok(())` when a `WIN32_ERROR` is `ERROR_SUCCESS`.
pub fn win_ok(code: WIN32_ERROR, what: &str) -> anyhow::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(anyhow::anyhow!("{what} failed (os error {code})"))
    }
}

/// A `HANDLE` closed on drop.
pub struct OwnedHandle(pub HANDLE);

impl OwnedHandle {
    pub fn raw(&self) -> HANDLE {
        self.0
    }
    /// Take the handle without closing it.
    pub fn into_raw(self) -> HANDLE {
        let h = self.0;
        std::mem::forget(self);
        h
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own the handle and drop runs once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// A `PSID` from `ConvertStringSidToSidW`, freed with `LocalFree`.
pub struct LocalPsid(PSID);

impl LocalPsid {
    /// Parse an SDDL SID string (`S-1-5-…`) into an owned `PSID`.
    pub fn from_string(sid: &str) -> anyhow::Result<Self> {
        let w = wide(sid);
        let mut psid: PSID = std::ptr::null_mut();
        // SAFETY: `w` is a valid NUL-terminated string; `psid` receives a
        // LocalAlloc buffer on success.
        ok(
            unsafe { ConvertStringSidToSidW(w.as_ptr(), &mut psid) },
            "ConvertStringSidToSidW",
        )?;
        Ok(Self(psid))
    }

    pub fn as_psid(&self) -> PSID {
        self.0
    }

    /// The SID's self-relative bytes.
    pub fn as_bytes(&self) -> Vec<u8> {
        // SAFETY: `self.0` is a valid SID; GetLengthSid gives its size.
        unsafe {
            let len = GetLengthSid(self.0) as usize;
            std::slice::from_raw_parts(self.0 as *const u8, len).to_vec()
        }
    }
}

impl Drop for LocalPsid {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the PSID came from ConvertStringSidToSidW (LocalAlloc).
            unsafe {
                let _ = LocalFree(self.0);
            }
        }
    }
}

/// The SDDL string (`S-1-5-…`) of a `PSID`, or `None`.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn sid_to_string(psid: PSID) -> Option<String> {
    if psid.is_null() {
        return None;
    }
    let mut out: PWSTR = std::ptr::null_mut();
    // SAFETY: `psid` is a valid SID; on success `out` is a LocalAlloc string.
    if unsafe { ConvertSidToStringSidW(psid, &mut out) } == 0 || out.is_null() {
        return None;
    }
    let s = from_wide(out);
    // SAFETY: `out` came from ConvertSidToStringSidW (LocalAlloc).
    unsafe {
        let _ = LocalFree(out as *mut c_void);
    }
    Some(s)
}

/// The self-relative bytes of the SID named by `sid` (SDDL string).
pub fn sid_bytes(sid: &str) -> anyhow::Result<Vec<u8>> {
    Ok(LocalPsid::from_string(sid)?.as_bytes())
}

/// Whether two SID byte buffers name the same SID.
pub fn same_sid(a: &[u8], b: &[u8]) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    // SAFETY: both are non-empty SID byte buffers; IsValidSid guards the
    // EqualSid call, which reads only within each SID's own length.
    unsafe {
        let (pa, pb) = (a.as_ptr() as PSID, b.as_ptr() as PSID);
        if IsValidSid(pa) == 0 || IsValidSid(pb) == 0 {
            return false;
        }
        windows_sys::Win32::Security::EqualSid(pa, pb) != 0
    }
}

/// A NUL-terminated wide string as a Rust `String` (stops at the first NUL).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn from_wide(ptr: PCWSTR) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: `ptr` is NUL-terminated; we read up to the NUL.
    unsafe {
        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
    }
}

/// The SID (SDDL string) of a local account or group by name, or `None`.
pub fn lookup_account_sid(name: &str) -> Option<String> {
    use windows_sys::Win32::Security::LookupAccountNameW;
    let w = wide(name);
    let mut sid_len = 0u32;
    let mut dom_len = 0u32;
    let mut use_ = 0i32;
    // Sizing call.
    // SAFETY: null buffers with lengths 0 ask for the sizes.
    unsafe {
        let _ = LookupAccountNameW(
            std::ptr::null(),
            w.as_ptr(),
            std::ptr::null_mut(),
            &mut sid_len,
            std::ptr::null_mut(),
            &mut dom_len,
            &mut use_,
        );
    }
    if sid_len == 0 {
        return None;
    }
    let mut sid = vec![0u8; sid_len as usize];
    let mut dom = vec![0u16; dom_len.max(1) as usize];
    // SAFETY: sid/dom are sized as the sizing call asked.
    if unsafe {
        LookupAccountNameW(
            std::ptr::null(),
            w.as_ptr(),
            sid.as_mut_ptr() as PSID,
            &mut sid_len,
            dom.as_mut_ptr(),
            &mut dom_len,
            &mut use_,
        )
    } == 0
    {
        return None;
    }
    sid_to_string(sid.as_ptr() as PSID)
}

/// The account/group name of a SID (SDDL string), or `None` (e.g. the
/// localized `BUILTIN\Users` name).
pub fn lookup_account_name(sid: &str) -> Option<String> {
    use windows_sys::Win32::Security::LookupAccountSidW;
    let psid = LocalPsid::from_string(sid).ok()?;
    let mut name_len = 0u32;
    let mut dom_len = 0u32;
    let mut use_ = 0i32;
    // SAFETY: null buffers ask for the sizes.
    unsafe {
        let _ = LookupAccountSidW(
            std::ptr::null(),
            psid.as_psid(),
            std::ptr::null_mut(),
            &mut name_len,
            std::ptr::null_mut(),
            &mut dom_len,
            &mut use_,
        );
    }
    if name_len == 0 {
        return None;
    }
    let mut name = vec![0u16; name_len as usize];
    let mut dom = vec![0u16; dom_len.max(1) as usize];
    // SAFETY: name/dom are sized as asked.
    if unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            psid.as_psid(),
            name.as_mut_ptr(),
            &mut name_len,
            dom.as_mut_ptr(),
            &mut dom_len,
            &mut use_,
        )
    } == 0
    {
        return None;
    }
    Some(from_wide(name.as_ptr()))
}

/// A security descriptor buffer from `GetNamedSecurityInfoW` /
/// `GetSecurityInfo` (a single `LocalAlloc`), freed on drop.
pub struct OwnedSd(pub *mut c_void);

impl Drop for OwnedSd {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the buffer came from Get*SecurityInfo (LocalAlloc).
            unsafe {
                let _ = LocalFree(self.0);
            }
        }
    }
}
