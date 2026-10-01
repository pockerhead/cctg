//! Slot account lifecycle for the elevated install step: create the group and
//! the slot users, add them to `BUILTIN\Users` and the group, hide them from
//! the logon screen, stamp LSA logon denials on the group, and remove it all
//! on uninstall. Form of `srtwin/src_user.rs` (Apache-2.0), adapted to raw
//! `windows-sys` and one account per slot. Every function here needs
//! elevation.

use windows_sys::Win32::NetworkManagement::NetManagement::{
    LOCALGROUP_INFO_1, LOCALGROUP_MEMBERS_INFO_0, NERR_GroupExists, NERR_UserNotFound,
    NetLocalGroupAdd, NetLocalGroupAddMembers, NetLocalGroupDel, NetUserAdd, NetUserDel,
    UF_DONT_EXPIRE_PASSWD, UF_SCRIPT, USER_INFO_1, USER_PRIV_USER,
};
use windows_sys::Win32::Security::Authentication::Identity::{
    LSA_OBJECT_ATTRIBUTES, LSA_UNICODE_STRING, LsaAddAccountRights, LsaClose, LsaOpenPolicy,
    LsaRemoveAccountRights, POLICY_CREATE_ACCOUNT, POLICY_LOOKUP_NAMES,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_DWORD, REG_OPTION_NON_VOLATILE,
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW,
};
use windows_sys::Win32::UI::Shell::DeleteProfileW;

use super::util::{LocalPsid, lookup_account_name, lookup_account_sid, wide};
use super::{Mark, slot_user};

const SID_BUILTIN_USERS: &str = "S-1-5-32-545";
const WINLOGON_USERLIST: &str =
    r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon\SpecialAccounts\UserList";
/// Logon rights denied to the group (everything but interactive, which
/// `CreateProcessWithLogonW` needs and the account holds via `BUILTIN\Users`).
const LOGON_DENIALS: [&str; 4] = [
    "SeDenyNetworkLogonRight",
    "SeDenyBatchLogonRight",
    "SeDenyServiceLogonRight",
    "SeDenyRemoteInteractiveLogonRight",
];

/// Creates the sandbox group (idempotent).
pub fn ensure_group() -> anyhow::Result<()> {
    let mut name = wide(super::GROUP);
    let mut comment = wide("cctg folder sandbox accounts");
    let info = LOCALGROUP_INFO_1 {
        lgrpi1_name: name.as_mut_ptr(),
        lgrpi1_comment: comment.as_mut_ptr(),
    };
    // SAFETY: info is a valid LOCALGROUP_INFO_1 (level 1).
    let rc = unsafe {
        NetLocalGroupAdd(
            std::ptr::null(),
            1,
            &info as *const _ as *const u8,
            std::ptr::null_mut(),
        )
    };
    if rc == 0 || rc == NERR_GroupExists {
        Ok(())
    } else {
        anyhow::bail!("NetLocalGroupAdd({}) rc={rc}", super::GROUP)
    }
}

/// (Re)creates slot `k`'s account with `password` and returns its SID. Any
/// existing account by that name (and its profile) is removed first.
pub fn recreate(k: u32, password: &super::cred::Password) -> anyhow::Result<String> {
    let name = slot_user(k);
    if let Some(sid) = lookup_account_sid(&name) {
        let sid_w = wide(&sid);
        // SAFETY: valid SID string; best-effort profile delete.
        unsafe {
            let _ = DeleteProfileW(sid_w.as_ptr(), std::ptr::null(), std::ptr::null());
        }
        delete_user(&name)?;
    }
    let mut name_w = wide(&name);
    let mut pw_w = password.to_wide();
    let mut comment = wide("cctg folder sandbox account");
    let info = USER_INFO_1 {
        usri1_name: name_w.as_mut_ptr(),
        usri1_password: pw_w.as_mut_ptr(),
        usri1_password_age: 0,
        usri1_priv: USER_PRIV_USER,
        usri1_home_dir: std::ptr::null_mut(),
        usri1_comment: comment.as_mut_ptr(),
        usri1_flags: UF_SCRIPT | UF_DONT_EXPIRE_PASSWD,
        usri1_script_path: std::ptr::null_mut(),
    };
    // SAFETY: info is a valid USER_INFO_1 (level 1).
    let rc = unsafe {
        NetUserAdd(
            std::ptr::null(),
            1,
            &info as *const _ as *const u8,
            std::ptr::null_mut(),
        )
    };
    pw_w.fill(0);
    if rc != 0 {
        anyhow::bail!("NetUserAdd({name}) rc={rc}");
    }
    let sid = lookup_account_sid(&name).ok_or_else(|| anyhow::anyhow!("resolve SID of {name}"))?;
    let psid = LocalPsid::from_string(&sid)?;
    if let Some(users) = lookup_account_name(SID_BUILTIN_USERS) {
        add_member(&users, &psid)?;
    }
    add_member(super::GROUP, &psid)?;
    hide_from_logon(&name, true)?;
    Ok(sid)
}

/// Adds `member` (by SID) to a local group (idempotent-ish: an "already a
/// member" error is treated as success).
fn add_member(group: &str, member: &LocalPsid) -> anyhow::Result<()> {
    let group_w = wide(group);
    let info = LOCALGROUP_MEMBERS_INFO_0 {
        lgrmi0_sid: member.as_psid(),
    };
    // SAFETY: level-0 member info with a valid SID; one entry.
    let rc = unsafe {
        NetLocalGroupAddMembers(
            std::ptr::null(),
            group_w.as_ptr(),
            0,
            &info as *const _ as *const u8,
            1,
        )
    };
    // ERROR_MEMBER_IN_ALIAS (1378): already a member.
    if rc == 0 || rc == 1378 {
        Ok(())
    } else {
        anyhow::bail!("NetLocalGroupAddMembers({group}) rc={rc}")
    }
}

fn delete_user(name: &str) -> anyhow::Result<()> {
    let w = wide(name);
    // SAFETY: valid name.
    let rc = unsafe { NetUserDel(std::ptr::null(), w.as_ptr()) };
    if rc == 0 || rc == NERR_UserNotFound {
        Ok(())
    } else {
        anyhow::bail!("NetUserDel({name}) rc={rc}")
    }
}

/// Writes (or clears) the Winlogon UserList value that hides the account from
/// the logon screen.
fn hide_from_logon(user: &str, hide: bool) -> anyhow::Result<()> {
    let sub = wide(WINLOGON_USERLIST);
    let mut hkey: HKEY = std::ptr::null_mut();
    // SAFETY: valid subkey; hkey receives the created/opened key.
    let code = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            sub.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE | KEY_WOW64_64KEY,
            std::ptr::null(),
            &mut hkey,
            std::ptr::null_mut(),
        )
    };
    if code != 0 {
        // Cosmetic only; do not fail the install for it.
        return Ok(());
    }
    let val = wide(user);
    if hide {
        let data = 0u32.to_ne_bytes();
        // SAFETY: valid key/value; 4-byte DWORD data.
        unsafe {
            let _ = RegSetValueExW(hkey, val.as_ptr(), 0, REG_DWORD, data.as_ptr(), 4);
        }
    } else {
        // SAFETY: valid key/value.
        unsafe {
            let _ = RegDeleteValueW(hkey, val.as_ptr());
        }
    }
    // SAFETY: hkey came from RegCreateKeyExW.
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    Ok(())
}

fn lsa_string(s: &str) -> (Vec<u16>, u16) {
    let buf: Vec<u16> = s.encode_utf16().collect();
    let bytes = (buf.len() * 2) as u16;
    (buf, bytes)
}

fn with_lsa<F>(f: F) -> anyhow::Result<()>
where
    F: FnOnce(isize, &[LSA_UNICODE_STRING]) -> i32,
{
    let attrs: LSA_OBJECT_ATTRIBUTES = unsafe { std::mem::zeroed() };
    let mut policy: isize = 0;
    // SAFETY: valid attrs; policy receives the handle.
    let status = unsafe {
        LsaOpenPolicy(
            std::ptr::null(),
            &attrs,
            (POLICY_CREATE_ACCOUNT | POLICY_LOOKUP_NAMES) as u32,
            &mut policy,
        )
    };
    if status != 0 {
        anyhow::bail!("LsaOpenPolicy status=0x{status:08x}");
    }
    let bufs: Vec<(Vec<u16>, u16)> = LOGON_DENIALS.iter().map(|r| lsa_string(r)).collect();
    let rights: Vec<LSA_UNICODE_STRING> = bufs
        .iter()
        .map(|(b, bytes)| LSA_UNICODE_STRING {
            Length: *bytes,
            MaximumLength: *bytes,
            Buffer: b.as_ptr() as *mut u16,
        })
        .collect();
    let status = f(policy, &rights);
    // SAFETY: policy came from LsaOpenPolicy.
    unsafe {
        let _ = LsaClose(policy);
    }
    if status != 0 {
        anyhow::bail!("LSA account-rights status=0x{status:08x}");
    }
    Ok(())
}

/// Stamps [`LOGON_DENIALS`] on the group SID (idempotent).
pub fn lsa_deny(group_sid: &str) -> anyhow::Result<()> {
    let psid = LocalPsid::from_string(group_sid)?;
    with_lsa(|policy, rights| unsafe {
        LsaAddAccountRights(policy, psid.as_psid(), rights.as_ptr(), rights.len() as u32)
    })
}

fn lsa_undeny(group_sid: &str) -> anyhow::Result<()> {
    let psid = LocalPsid::from_string(group_sid)?;
    with_lsa(|policy, rights| unsafe {
        LsaRemoveAccountRights(
            policy,
            psid.as_psid(),
            false,
            rights.as_ptr(),
            rights.len() as u32,
        )
    })
}

/// Removes every slot account, the group, the LSA denials, the profiles and
/// the logon-hide values. Best-effort; returns the first error, if any.
pub fn remove_all(mark: &Mark) -> anyhow::Result<()> {
    let _ = lsa_undeny(&mark.group_sid);
    for k in 1..=mark.slots {
        let name = slot_user(k);
        if let Some(sid) = lookup_account_sid(&name) {
            let sid_w = wide(&sid);
            // SAFETY: valid SID string.
            unsafe {
                let _ = DeleteProfileW(sid_w.as_ptr(), std::ptr::null(), std::ptr::null());
            }
        }
        let _ = delete_user(&name);
        let _ = hide_from_logon(&name, false);
    }
    let group_w = wide(super::GROUP);
    // SAFETY: valid group name.
    unsafe {
        let _ = NetLocalGroupDel(std::ptr::null(), group_w.as_ptr());
    }
    Ok(())
}
