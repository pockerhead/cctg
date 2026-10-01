//! The broker's per-command private desktop, and the station/BNO grants the
//! two-hop runner+child need to attach it. Form of `srtwin/src_winsta.rs`
//! (Apache-2.0), adapted to raw `windows-sys`. The desktop keeps the sandbox
//! off the interactive `WinSta0\Default`, so a co-located human's keystrokes
//! are not capturable (`WH_KEYBOARD_LL` is per-desktop; the job's UI limits do
//! not gate low-level hooks).
//!
//! All grants are keyed on the slot USER SID, never a logon SID (seclogon
//! stamps the broker's logon SID into the runner's token). They persist for
//! the session; there is no revoke (concurrent brokers share the station).

use std::ffi::c_void;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    NtOpenDirectoryObject, NtQuerySecurityObject, NtSetSecurityObject,
};
use windows_sys::Win32::Foundation::{HANDLE, OBJ_CASE_INSENSITIVE, UNICODE_STRING};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    ACL, ACL_SIZE_INFORMATION, AclSizeInformation, AddAccessAllowedAce, AddAce,
    DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetLengthSid, GetSecurityDescriptorDacl,
    GetUserObjectSecurity, InitializeAcl, InitializeSecurityDescriptor, PSECURITY_DESCRIPTOR,
    SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SetSecurityDescriptorDacl, SetUserObjectSecurity,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::StationsAndDesktops::{
    CloseDesktop, CreateDesktopW, GetProcessWindowStation, GetThreadDesktop,
    GetUserObjectInformationW, HDESK, UOI_NAME,
};

use super::util::{LocalPsid, OwnedHandle, from_wide, ok, sid_bytes, wide};

const DESK_ALL_ACCESS: u32 = 0x000F_0000 | 0x0000_01FF;
/// `WINSTA_ALL_ACCESS | READ_CONTROL`, without `WRITE_DAC|WRITE_OWNER|DELETE`.
const WINSTA_GRANT: u32 = 0x0002_037F;
/// `DIRECTORY_QUERY | TRAVERSE | CREATE_OBJECT | CREATE_SUBDIRECTORY`.
const BNO_GRANT: u32 = 0x0000_000F;
const SECURITY_DESCRIPTOR_REVISION: u32 = 1;
const READ_CONTROL: u32 = 0x0002_0000;
const WRITE_DAC: u32 = 0x0004_0000;

/// The broker's per-command desktop; its handle is held until the runner
/// exits so the kernel object survives the whole chain.
pub struct IsolatedDesk {
    desktop: HDESK,
    /// `<winsta>\<desk>` for `STARTUPINFOW.lpDesktop` (NUL-terminated).
    path: Vec<u16>,
}

impl IsolatedDesk {
    /// Creates a fresh desktop on the current window station with an explicit
    /// `[real user, slot, SYSTEM] : GENERIC_ALL` DACL.
    pub fn new(user_sid: &str, slot_sid: &str) -> anyhow::Result<Self> {
        let station = current_winsta_name()?;
        let mut r = [0u8; 4];
        aws_lc_rs::rand::fill(&mut r).map_err(|_| anyhow::anyhow!("rng"))?;
        let name = format!(
            "cctg-sb-{:08x}{:08x}",
            std::process::id(),
            u32::from_le_bytes(r)
        );
        let name_w = wide(&name);
        let sddl = format!("D:(A;;GA;;;{user_sid})(A;;GA;;;{slot_sid})(A;;GA;;;SY)");
        let sddl_w = wide(&sddl);
        let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: valid SDDL; psd receives a LocalAlloc SD.
        ok(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl_w.as_ptr(),
                    SDDL_REVISION_1,
                    &mut psd,
                    std::ptr::null_mut(),
                )
            },
            "ConvertStringSecurityDescriptorToSecurityDescriptorW",
        )?;
        let _sd = super::util::OwnedSd(psd);
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: psd,
            bInheritHandle: 0,
        };
        // SAFETY: valid name and SA; no device/devmode.
        let desktop = unsafe {
            CreateDesktopW(
                name_w.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                DESK_ALL_ACCESS,
                &sa,
            )
        };
        if desktop.is_null() {
            anyhow::bail!("CreateDesktopW (os error {})", super::util::last_error());
        }
        let desk_name = object_name(desktop as HANDLE).unwrap_or(name);
        let path = wide(&format!("{station}\\{desk_name}"));
        Ok(Self { desktop, path })
    }

    /// Pointer to the `lpDesktop` buffer (kept alive by `self`).
    pub fn path_ptr(&mut self) -> *mut u16 {
        self.path.as_mut_ptr()
    }
}

impl Drop for IsolatedDesk {
    fn drop(&mut self) {
        // SAFETY: desktop came from CreateDesktopW.
        unsafe {
            let _ = CloseDesktop(self.desktop);
        }
    }
}

/// The current window station's name.
pub fn current_winsta_name() -> anyhow::Result<String> {
    // SAFETY: returns the process station handle.
    let ws = unsafe { GetProcessWindowStation() };
    if ws.is_null() {
        anyhow::bail!("GetProcessWindowStation");
    }
    object_name(ws as HANDLE)
}

/// Whether this thread is on the interactive `Default` desktop (fail closed:
/// any read failure reads as `true`).
pub fn on_default_desktop() -> bool {
    // SAFETY: current-thread desktop handle.
    let d = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
    if d.is_null() {
        return true;
    }
    object_name(d as HANDLE)
        .map(|n| n.eq_ignore_ascii_case("Default"))
        .unwrap_or(true)
}

use windows_sys::Win32::System::Threading::GetCurrentThreadId;

/// Reads a user object's `UOI_NAME`.
fn object_name(h: HANDLE) -> anyhow::Result<String> {
    let mut needed = 0u32;
    // SAFETY: sizing call.
    unsafe {
        let _ = GetUserObjectInformationW(h, UOI_NAME, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        anyhow::bail!("GetUserObjectInformationW sizing 0");
    }
    let mut buf = vec![0u8; needed as usize];
    // SAFETY: buf is `needed` bytes.
    ok(
        unsafe {
            GetUserObjectInformationW(
                h,
                UOI_NAME,
                buf.as_mut_ptr() as *mut c_void,
                needed,
                &mut needed,
            )
        },
        "GetUserObjectInformationW",
    )?;
    let wide: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_ne_bytes([c[0], c[1]]))
        .collect();
    Ok(from_wide(wide.as_ptr()))
}

/// Adds an attach-level ACE for the slot SID to the broker's window station.
pub fn grant_sandbox_on_winsta(slot_sid: &str) -> anyhow::Result<()> {
    // SAFETY: returns the process station handle.
    let ws = unsafe { GetProcessWindowStation() };
    if ws.is_null() {
        anyhow::bail!("GetProcessWindowStation");
    }
    let h = ws as HANDLE;
    let want = sid_bytes(slot_sid)?;
    let si = DACL_SECURITY_INFORMATION;
    // Read current SD.
    let mut needed = 0u32;
    // SAFETY: sizing call.
    unsafe {
        let _ = GetUserObjectSecurity(h, &si, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        anyhow::bail!("GetUserObjectSecurity sizing 0");
    }
    let mut sd = vec![0u8; needed as usize];
    // SAFETY: sd is `needed` bytes.
    ok(
        unsafe {
            GetUserObjectSecurity(
                h,
                &si,
                sd.as_mut_ptr() as PSECURITY_DESCRIPTOR,
                needed,
                &mut needed,
            )
        },
        "GetUserObjectSecurity",
    )?;
    let (new_acl, _sd) = append_ace(
        sd.as_mut_ptr() as PSECURITY_DESCRIPTOR,
        slot_sid,
        WINSTA_GRANT,
        &want,
    )?;
    // SAFETY: new SD is valid.
    ok(
        unsafe { SetUserObjectSecurity(h, &si, _sd.psd()) },
        "SetUserObjectSecurity",
    )?;
    drop(new_acl);
    Ok(())
}

/// Adds create rights for the slot SID to the session `BaseNamedObjects`.
pub fn grant_sandbox_on_session_bno(slot_sid: &str) -> anyhow::Result<()> {
    let bno = open_session_bno()?;
    let want = sid_bytes(slot_sid)?;
    let si = DACL_SECURITY_INFORMATION;
    let mut needed = 0u32;
    // SAFETY: sizing call.
    unsafe {
        let _ = NtQuerySecurityObject(bno.raw(), si, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        anyhow::bail!("NtQuerySecurityObject sizing 0");
    }
    let mut sd = vec![0u8; needed as usize];
    // SAFETY: sd is `needed` bytes.
    let status = unsafe {
        NtQuerySecurityObject(
            bno.raw(),
            si,
            sd.as_mut_ptr() as PSECURITY_DESCRIPTOR,
            needed,
            &mut needed,
        )
    };
    if status != 0 {
        anyhow::bail!("NtQuerySecurityObject status=0x{status:08x}");
    }
    let (new_acl, new_sd) = append_ace(
        sd.as_mut_ptr() as PSECURITY_DESCRIPTOR,
        slot_sid,
        BNO_GRANT,
        &want,
    )?;
    // SAFETY: new SD is valid.
    let status = unsafe { NtSetSecurityObject(bno.raw(), si, new_sd.psd()) };
    if status != 0 {
        anyhow::bail!("NtSetSecurityObject status=0x{status:08x}");
    }
    drop(new_acl);
    Ok(())
}

/// Opens `\Sessions\<ts>\BaseNamedObjects` for a DACL rewrite.
fn open_session_bno() -> anyhow::Result<OwnedHandle> {
    let mut ts = 0u32;
    // SAFETY: valid pid; ts receives the session.
    ok(
        unsafe { ProcessIdToSessionId(std::process::id(), &mut ts) },
        "ProcessIdToSessionId",
    )?;
    let path = format!("\\Sessions\\{ts}\\BaseNamedObjects");
    let mut buf: Vec<u16> = path.encode_utf16().collect();
    let bytes = (buf.len() * 2) as u16;
    let name = UNICODE_STRING {
        Length: bytes,
        MaximumLength: bytes,
        Buffer: buf.as_mut_ptr(),
    };
    let oa = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: std::ptr::null_mut(),
        ObjectName: &name,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut h: HANDLE = std::ptr::null_mut();
    // SAFETY: valid oa; h receives the directory handle.
    let status = unsafe { NtOpenDirectoryObject(&mut h, READ_CONTROL | WRITE_DAC, &oa) };
    if status != 0 {
        anyhow::bail!("NtOpenDirectoryObject status=0x{status:08x}");
    }
    Ok(OwnedHandle(h))
}

/// A self-relative SD's DACL with one ALLOW ACE for `sid` (mask, no inherit)
/// appended, dropping any existing ACE of `sid`, kept in a fresh absolute SD.
fn append_ace(
    old_sd: PSECURITY_DESCRIPTOR,
    sid_str: &str,
    mask: u32,
    drop_sid: &[u8],
) -> anyhow::Result<(Vec<u8>, AbsoluteSd)> {
    let mut present = 0i32;
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut defaulted = 0i32;
    // SAFETY: old_sd is a valid SD.
    ok(
        unsafe { GetSecurityDescriptorDacl(old_sd, &mut present, &mut dacl, &mut defaulted) },
        "GetSecurityDescriptorDacl",
    )?;
    // Collect kept ACEs (all except drop_sid), then append one ALLOW.
    let mut kept: Vec<(*const c_void, u16)> = Vec::new();
    if !dacl.is_null() {
        let mut info = ACL_SIZE_INFORMATION::default();
        // SAFETY: dacl valid.
        ok(
            unsafe {
                GetAclInformation(
                    dacl,
                    &mut info as *mut _ as *mut c_void,
                    std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                )
            },
            "GetAclInformation",
        )?;
        for i in 0..info.AceCount {
            let mut ace: *mut c_void = std::ptr::null_mut();
            // SAFETY: dacl valid; i < AceCount.
            if unsafe { GetAce(dacl, i, &mut ace) } == 0 || ace.is_null() {
                continue;
            }
            // SAFETY: ace has an ACE header + AceSize bytes.
            let size = unsafe {
                *(ace as *const u8).add(2) as u16 | ((*(ace as *const u8).add(3) as u16) << 8)
            };
            let body = unsafe { std::slice::from_raw_parts(ace as *const u8, size as usize) };
            let is_drop = body.get(8..8 + drop_sid.len()) == Some(drop_sid);
            if !is_drop {
                kept.push((ace as *const c_void, size));
            }
        }
    }
    let sid = LocalPsid::from_string(sid_str)?;
    // SAFETY: sid valid.
    let sid_len = unsafe { GetLengthSid(sid.as_psid()) } as usize;
    let kept_bytes: usize = kept.iter().map(|(_, s)| *s as usize).sum();
    let mut total = std::mem::size_of::<ACL>() + kept_bytes + 8 + sid_len;
    total = (total + 3) & !3;
    let mut acl_buf = vec![0u8; total];
    let acl = acl_buf.as_mut_ptr() as *mut ACL;
    // SAFETY: acl_buf is `total` bytes; ACL_REVISION = 2.
    ok(
        unsafe { InitializeAcl(acl, total as u32, 2) },
        "InitializeAcl",
    )?;
    for (ptr, size) in &kept {
        // SAFETY: acl fits; ptr/size describe a valid ACE.
        ok(
            unsafe { AddAce(acl, 2, u32::MAX, *ptr, *size as u32) },
            "AddAce",
        )?;
    }
    // SAFETY: acl fits; sid valid.
    ok(
        unsafe { AddAccessAllowedAce(acl, 2, mask, sid.as_psid()) },
        "AddAccessAllowedAce",
    )?;
    let abs = AbsoluteSd::new(acl_buf.as_ptr() as *const ACL)?;
    Ok((acl_buf, abs))
}

/// A minimal absolute security descriptor with a DACL (kept alive together
/// with its ACL buffer by the caller).
pub struct AbsoluteSd {
    sd: Box<SECURITY_DESCRIPTOR>,
}

impl AbsoluteSd {
    fn new(dacl: *const ACL) -> anyhow::Result<Self> {
        let mut sd: Box<SECURITY_DESCRIPTOR> = Box::new(unsafe { std::mem::zeroed() });
        let psd = &mut *sd as *mut _ as PSECURITY_DESCRIPTOR;
        // SAFETY: psd points at a SECURITY_DESCRIPTOR.
        ok(
            unsafe { InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION) },
            "InitializeSecurityDescriptor",
        )?;
        // SAFETY: psd valid; dacl points at a valid ACL kept by the caller.
        ok(
            unsafe { SetSecurityDescriptorDacl(psd, 1, dacl, 0) },
            "SetSecurityDescriptorDacl",
        )?;
        Ok(Self { sd })
    }

    fn psd(&self) -> PSECURITY_DESCRIPTOR {
        &*self.sd as *const _ as PSECURITY_DESCRIPTOR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_station_name_reads() {
        assert!(current_winsta_name().is_ok());
    }
}
