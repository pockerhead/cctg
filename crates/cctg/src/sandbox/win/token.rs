//! The restricted token the runner launches the command under. Form of
//! `srtwin/src_token.rs` (Apache-2.0): `CreateRestrictedToken` with
//! `LUA_TOKEN`, `BUILTIN\Administrators` and every logon-session SID disabled
//! (deny-only), all privileges deleted except `SeChangeNotifyPrivilege`, and
//! NO restricting SIDs — those break Schannel (`SEC_E_NO_CREDENTIALS`), so
//! cargo, `curl.exe` and git-over-schannel would fail. Medium integrity, a
//! default DACL of `[SYSTEM, slot user] : GENERIC_ALL`, duplicated to a
//! primary token.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{HANDLE, LUID};
use windows_sys::Win32::Security::{
    ACL, AddAccessAllowedAce, AllocateAndInitializeSid, CreateRestrictedToken, DuplicateTokenEx,
    FreeSid, GetLengthSid, GetTokenInformation, InitializeAcl, LUA_TOKEN, LUID_AND_ATTRIBUTES,
    LookupPrivilegeValueW, PSID, SID_AND_ATTRIBUTES, SID_IDENTIFIER_AUTHORITY,
    SecurityImpersonation, SetTokenInformation, TOKEN_DEFAULT_DACL, TOKEN_GROUPS,
    TOKEN_INFORMATION_CLASS, TOKEN_MANDATORY_LABEL, TOKEN_PRIVILEGES, TOKEN_USER, TokenDefaultDacl,
    TokenGroups, TokenIntegrityLevel, TokenPrimary, TokenPrivileges, TokenUser,
};

use super::util::{LocalPsid, OwnedHandle, ok};

/// `SE_GROUP_LOGON_ID` (0xC0000000): both bits mark a logon-session SID.
const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;
/// `SE_GROUP_INTEGRITY`.
const SE_GROUP_INTEGRITY: u32 = 0x0000_0020;
/// Medium integrity RID.
const IL_MEDIUM: u32 = 0x2000;
const SID_ADMINS: &str = "S-1-5-32-544";
const SID_SYSTEM: &str = "S-1-5-18";
const GENERIC_ALL: u32 = 0x1000_0000;
const TOKEN_ALL: u32 = 0x000F_01FF; // TOKEN_ALL_ACCESS

/// Reads a token information class into a byte buffer.
fn info(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> anyhow::Result<Vec<u8>> {
    let mut len = 0u32;
    // SAFETY: sizing call.
    unsafe {
        let _ = GetTokenInformation(token, class, std::ptr::null_mut(), 0, &mut len);
    }
    if len == 0 {
        anyhow::bail!("GetTokenInformation sizing 0");
    }
    let mut buf = vec![0u8; len as usize];
    // SAFETY: buf is `len` bytes.
    ok(
        unsafe {
            GetTokenInformation(token, class, buf.as_mut_ptr() as *mut c_void, len, &mut len)
        },
        "GetTokenInformation",
    )?;
    Ok(buf)
}

/// Each logon-session SID in `token`, as owned byte buffers.
fn logon_sids(token: HANDLE) -> anyhow::Result<Vec<Vec<u8>>> {
    let buf = info(token, TokenGroups)?;
    // SAFETY: buf holds a TOKEN_GROUPS.
    let tg = unsafe { &*(buf.as_ptr() as *const TOKEN_GROUPS) };
    // SAFETY: Groups is GroupCount long past the struct header.
    let groups = unsafe { std::slice::from_raw_parts(tg.Groups.as_ptr(), tg.GroupCount as usize) };
    Ok(groups
        .iter()
        .filter(|g| g.Attributes & SE_GROUP_LOGON_ID == SE_GROUP_LOGON_ID)
        .map(|g| {
            // SAFETY: g.Sid is a valid SID.
            let len = unsafe { GetLengthSid(g.Sid) } as usize;
            unsafe { std::slice::from_raw_parts(g.Sid as *const u8, len).to_vec() }
        })
        .collect())
}

/// Privileges of `token` except those named in `keep`.
fn privileges_except(token: HANDLE, keep: &[&str]) -> anyhow::Result<Vec<LUID_AND_ATTRIBUTES>> {
    let keep_luids: Vec<LUID> = keep
        .iter()
        .filter_map(|name| {
            let w = super::util::wide(name);
            let mut luid = LUID {
                LowPart: 0,
                HighPart: 0,
            };
            // SAFETY: valid name; luid receives the value.
            (unsafe { LookupPrivilegeValueW(std::ptr::null(), w.as_ptr(), &mut luid) } != 0)
                .then_some(luid)
        })
        .collect();
    let buf = info(token, TokenPrivileges)?;
    // SAFETY: buf holds a TOKEN_PRIVILEGES.
    let tp = unsafe { &*(buf.as_ptr() as *const TOKEN_PRIVILEGES) };
    // SAFETY: Privileges is PrivilegeCount long.
    let privs =
        unsafe { std::slice::from_raw_parts(tp.Privileges.as_ptr(), tp.PrivilegeCount as usize) };
    Ok(privs
        .iter()
        .filter(|p| {
            !keep_luids
                .iter()
                .any(|k| k.LowPart == p.Luid.LowPart && k.HighPart == p.Luid.HighPart)
        })
        .map(|p| LUID_AND_ATTRIBUTES {
            Luid: p.Luid,
            Attributes: 0,
        })
        .collect())
}

/// Opens the current process's primary token with full access.
pub fn open_self_token() -> anyhow::Result<OwnedHandle> {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut h: HANDLE = std::ptr::null_mut();
    // SAFETY: current-process pseudo-handle; h receives the token.
    ok(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ALL, &mut h) },
        "OpenProcessToken",
    )?;
    Ok(OwnedHandle(h))
}

/// Builds the restricted primary token from `base` (the runner's own token).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn make_sandbox_token(base: HANDLE) -> anyhow::Result<OwnedHandle> {
    let admins = LocalPsid::from_string(SID_ADMINS)?;
    let logon = logon_sids(base)?;
    let disable: Vec<SID_AND_ATTRIBUTES> = std::iter::once(admins.as_psid())
        .chain(logon.iter().map(|s| s.as_ptr() as PSID))
        .map(|sid| SID_AND_ATTRIBUTES {
            Sid: sid,
            Attributes: 0,
        })
        .collect();
    let delete = privileges_except(base, &["SeChangeNotifyPrivilege"])?;
    let mut out: HANDLE = std::ptr::null_mut();
    // SAFETY: base is a valid token; disable/delete describe valid arrays;
    // no restricting SIDs.
    ok(
        unsafe {
            CreateRestrictedToken(
                base,
                LUA_TOKEN,
                disable.len() as u32,
                disable.as_ptr(),
                delete.len() as u32,
                if delete.is_empty() {
                    std::ptr::null()
                } else {
                    delete.as_ptr()
                },
                0,
                std::ptr::null(),
                &mut out,
            )
        },
        "CreateRestrictedToken",
    )?;
    let restricted = OwnedHandle(out);
    set_il(restricted.raw(), IL_MEDIUM)?;
    set_default_dacl(restricted.raw(), base)?;
    // Duplicate to a primary token.
    let mut primary: HANDLE = std::ptr::null_mut();
    // SAFETY: restricted is a valid token.
    ok(
        unsafe {
            DuplicateTokenEx(
                restricted.raw(),
                TOKEN_ALL,
                std::ptr::null(),
                SecurityImpersonation,
                TokenPrimary,
                &mut primary,
            )
        },
        "DuplicateTokenEx",
    )?;
    Ok(OwnedHandle(primary))
}

fn set_il(token: HANDLE, rid: u32) -> anyhow::Result<()> {
    let auth = SID_IDENTIFIER_AUTHORITY {
        Value: [0, 0, 0, 0, 0, 16],
    };
    let mut sid: PSID = std::ptr::null_mut();
    // SAFETY: valid authority; sid receives an allocated SID.
    ok(
        unsafe { AllocateAndInitializeSid(&auth, 1, rid, 0, 0, 0, 0, 0, 0, 0, &mut sid) },
        "AllocateAndInitializeSid",
    )?;
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: sid,
            Attributes: SE_GROUP_INTEGRITY,
        },
    };
    // SAFETY: sid is valid; label is the right size.
    let r = unsafe {
        SetTokenInformation(
            token,
            TokenIntegrityLevel,
            &label as *const _ as *const c_void,
            std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as u32 + GetLengthSid(sid),
        )
    };
    // SAFETY: sid came from AllocateAndInitializeSid.
    unsafe {
        FreeSid(sid);
    }
    ok(r, "SetTokenInformation(IL)")
}

/// Default DACL so objects the child creates are reachable by SYSTEM and the
/// slot user (`base`'s TokenUser). Keyed on the user SID, not the logon SID
/// (which is deny-only in the restricted token).
fn set_default_dacl(token: HANDLE, base: HANDLE) -> anyhow::Result<()> {
    let system = LocalPsid::from_string(SID_SYSTEM)?;
    let user_buf = info(base, TokenUser)?;
    // SAFETY: user_buf holds a TOKEN_USER.
    let user = unsafe { (*(user_buf.as_ptr() as *const TOKEN_USER)).User.Sid };
    let sids = [system.as_psid(), user];
    let mut total = std::mem::size_of::<ACL>();
    for s in &sids {
        // SAFETY: each s is a valid SID.
        total += 8 + unsafe { GetLengthSid(*s) } as usize;
    }
    total = (total + 3) & !3;
    let mut buf = vec![0u8; total];
    let acl = buf.as_mut_ptr() as *mut ACL;
    // SAFETY: buf is `total` bytes; ACL_REVISION = 2.
    ok(
        unsafe { InitializeAcl(acl, total as u32, 2) },
        "InitializeAcl",
    )?;
    for s in &sids {
        // SAFETY: acl fits; s is valid.
        ok(
            unsafe { AddAccessAllowedAce(acl, 2, GENERIC_ALL, *s) },
            "AddAccessAllowedAce(default DACL)",
        )?;
    }
    let dacl = TOKEN_DEFAULT_DACL { DefaultDacl: acl };
    // SAFETY: dacl points at a valid ACL.
    ok(
        unsafe {
            SetTokenInformation(
                token,
                TokenDefaultDacl,
                &dacl as *const _ as *const c_void,
                std::mem::size_of::<TOKEN_DEFAULT_DACL>() as u32,
            )
        },
        "SetTokenInformation(DefaultDacl)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restricted_token_builds() {
        let base = open_self_token().expect("self token");
        let token = make_sandbox_token(base.raw()).expect("restricted token");
        assert!(!token.raw().is_null());
    }

    #[test]
    fn change_notify_is_kept() {
        let base = open_self_token().expect("self token");
        let to_delete = privileges_except(base.raw(), &["SeChangeNotifyPrivilege"]).unwrap();
        let w = super::super::util::wide("SeChangeNotifyPrivilege");
        let mut keep = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        // SAFETY: valid name.
        unsafe {
            assert_ne!(
                LookupPrivilegeValueW(std::ptr::null(), w.as_ptr(), &mut keep),
                0
            );
        }
        assert!(
            !to_delete
                .iter()
                .any(|p| p.Luid.LowPart == keep.LowPart && p.Luid.HighPart == keep.HighPart)
        );
    }
}
