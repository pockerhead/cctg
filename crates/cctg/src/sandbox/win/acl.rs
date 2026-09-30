//! The folder's DACL: an additive model of explicit ACEs for one slot
//! account's SID. `grant_folder` gives the slot modify on the tree (and denies
//! write to files with several hard links, which share one descriptor with
//! links outside the folder); `stamp_protected` denies write to `.git`,
//! `.claude`, shell/IDE configs and the like; `revoke_folder` drops the slot's
//! ACEs. Never a `PROTECTED` rewrite of a user file: we keep the object's own
//! ACEs and inheritance.
//!
//! Model and the no-follow open by intent: `srtwin/src_acl.rs` (Apache-2.0),
//! adapted from the `windows` crate to raw `windows-sys` and to one SID per
//! folder.

use std::ffi::c_void;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, GetSecurityInfo, SE_FILE_OBJECT, SetNamedSecurityInfoW, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, AddAccessAllowedAceEx,
    AddAccessDeniedAceEx, AddAce, DACL_SECURITY_INFORMATION, GetAce, GetAclInformation,
    GetLengthSid, GetSecurityDescriptorControl, InitializeAcl, OBJECT_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SE_DACL_PROTECTED, UNPROTECTED_DACL_SECURITY_INFORMATION,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, DELETE, FILE_APPEND_DATA,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA,
    GetFileInformationByHandle, OPEN_EXISTING, WRITE_DAC, WRITE_OWNER,
};

use super::util::{
    LocalPsid, OwnedHandle, OwnedSd, last_error, ok, same_sid, sid_bytes, verbatim_wide, win_ok,
};

// ─── access masks ───────────────────────────────────────────────────

const READ_CONTROL: u32 = 0x0002_0000;
const FILE_DELETE_CHILD: u32 = 0x0000_0040;

/// Modify minus `FILE_DELETE_CHILD` (srt): the slot can read/write/create and
/// delete its own, but a denied file inside stays undeletable via the parent.
const MODIFY_NO_FDC: u32 =
    (FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE) & !FILE_DELETE_CHILD;
/// Read + execute.
const READ_EXEC: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
/// Everything that changes a file's bytes, metadata, name or security.
const DENY_WRITE: u32 = FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | FILE_WRITE_EA
    | FILE_WRITE_ATTRIBUTES
    | DELETE
    | WRITE_DAC
    | WRITE_OWNER;
/// Replace or relink a directory/file (rename over, re-own, re-ACL).
const DENY_REPLACE: u32 = DELETE | WRITE_DAC | WRITE_OWNER;
/// [`DENY_WRITE`] without `DELETE`: for a shared hard-link body.
const DENY_LINK_WRITE: u32 = DENY_WRITE & !DELETE;

/// `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE`, for the group ACE on `cctg.exe`.
pub const GROUP_READ_EXEC: u32 = READ_EXEC;
/// `FILE_GENERIC_READ`, for the slot ACE on the gitconfig copy.
pub const FILE_READ: u32 = FILE_GENERIC_READ;

// ─── inheritance flags ──────────────────────────────────────────────

const OBJECT_INHERIT: u8 = 0x1;
const CONTAINER_INHERIT: u8 = 0x2;
const INHERIT_ONLY: u8 = 0x8;
const INHERITED_ACE: u8 = 0x10;
const OICI: u8 = OBJECT_INHERIT | CONTAINER_INHERIT;
const OICI_IO: u8 = OICI | INHERIT_ONLY;
const NO_INHERIT: u8 = 0;

const ACCESS_ALLOWED: u8 = 0;
const ACCESS_DENIED: u8 = 1;

const SID_OWNER_RIGHTS: &str = "S-1-3-4";
/// Well-known broad principals that expose a folder to other accounts.
const BROAD_SIDS: &[&str] = &[
    "S-1-1-0",      // Everyone
    "S-1-5-11",     // Authenticated Users
    "S-1-5-32-545", // BUILTIN\Users
    "S-1-5-4",      // Interactive
    "S-1-2-0",      // Local
    "S-1-2-1",      // Console Logon
    "S-1-5-32-546", // Guests
    "S-1-5-7",      // Anonymous
];

// ─── one ACE to add ─────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Ace {
    deny: bool,
    mask: u32,
    flags: u8,
}

impl Ace {
    fn allow(mask: u32, flags: u8) -> Self {
        Self {
            deny: false,
            mask,
            flags,
        }
    }
    fn deny(mask: u32, flags: u8) -> Self {
        Self {
            deny: true,
            mask,
            flags,
        }
    }
}

/// One trustee's ACEs to add.
struct Grant<'a> {
    sid: &'a LocalPsid,
    aces: &'a [Ace],
}

// ─── ACL rebuild ────────────────────────────────────────────────────

/// A new ACL: each grant's ACEs (in order), then every existing explicit ACE
/// except inherited ones and those whose trustee is in `drop_sids`.
fn rebuild(old: *const ACL, grants: &[Grant<'_>], drop_sids: &[&[u8]]) -> anyhow::Result<Vec<u8>> {
    let kept = kept_aces(old, |hdr, body| {
        hdr.AceFlags & INHERITED_ACE == 0 && !drop_sids.iter().any(|s| ace_sid_is(body, s))
    })?;
    let kept_bytes: usize = kept.iter().map(|(_, size)| *size as usize).sum();
    let mut total = std::mem::size_of::<ACL>() + kept_bytes;
    for g in grants {
        // SAFETY: g.sid is a valid SID.
        let sid_len = unsafe { GetLengthSid(g.sid.as_psid()) } as usize;
        total += g.aces.len() * (8 + sid_len);
    }
    total = (total + 3) & !3;
    let mut buf = vec![0u8; total];
    let acl = buf.as_mut_ptr() as *mut ACL;
    // SAFETY: buf is `total` bytes, DWORD-aligned; ACL_REVISION = 2.
    ok(
        unsafe { InitializeAcl(acl, total as u32, 2) },
        "InitializeAcl",
    )?;
    for g in grants {
        for ace in g.aces {
            let flags = ace.flags as u32;
            let r = if ace.deny {
                // SAFETY: acl fits; sid valid.
                unsafe { AddAccessDeniedAceEx(acl, 2, flags, ace.mask, g.sid.as_psid()) }
            } else {
                // SAFETY: acl fits; sid valid.
                unsafe { AddAccessAllowedAceEx(acl, 2, flags, ace.mask, g.sid.as_psid()) }
            };
            ok(r, "AddAce(new)")?;
        }
    }
    for (ptr, size) in &kept {
        // SAFETY: acl fits; ptr/size describe a valid ACE in `old`.
        ok(
            unsafe { AddAce(acl, 2, u32::MAX, *ptr, *size as u32) },
            "AddAce(kept)",
        )?;
    }
    Ok(buf)
}

/// `(ptr, size)` of each ACE of `acl` that `keep` accepts. Pointers point into
/// `acl`'s buffer; used at once (inside [`rebuild`] / the presence checks).
fn kept_aces(
    acl: *const ACL,
    mut keep: impl FnMut(&ACE_HEADER, &[u8]) -> bool,
) -> anyhow::Result<Vec<(*const c_void, u16)>> {
    if acl.is_null() {
        return Ok(Vec::new());
    }
    let mut info = ACL_SIZE_INFORMATION::default();
    // SAFETY: acl is valid; info is the right size.
    ok(
        unsafe {
            GetAclInformation(
                acl,
                &mut info as *mut _ as *mut c_void,
                std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        },
        "GetAclInformation",
    )?;
    let mut kept = Vec::new();
    for i in 0..info.AceCount {
        let mut ace: *mut c_void = std::ptr::null_mut();
        // SAFETY: acl valid; i < AceCount.
        if unsafe { GetAce(acl, i, &mut ace) } == 0 || ace.is_null() {
            continue;
        }
        // SAFETY: ace points at an ACE_HEADER + AceSize bytes.
        let hdr = unsafe { &*(ace as *const ACE_HEADER) };
        let body = unsafe { std::slice::from_raw_parts(ace as *const u8, hdr.AceSize as usize) };
        if keep(hdr, body) {
            kept.push((ace as *const c_void, hdr.AceSize));
        }
    }
    Ok(kept)
}

/// Whether an ACE body's trustee SID equals `sid_bytes` (SID starts at byte 8).
fn ace_sid_is(body: &[u8], sid_bytes: &[u8]) -> bool {
    !sid_bytes.is_empty() && body.get(8..8 + sid_bytes.len()) == Some(sid_bytes)
}

/// Whether the security descriptor's DACL is `SE_DACL_PROTECTED`.
fn sd_protected(psd: PSECURITY_DESCRIPTOR) -> bool {
    let mut control: u16 = 0;
    let mut rev: u32 = 0;
    // SAFETY: psd is a valid SD.
    if unsafe { GetSecurityDescriptorControl(psd, &mut control, &mut rev) } == 0 {
        return false;
    }
    control & SE_DACL_PROTECTED != 0
}

fn protection(protected: bool) -> OBJECT_SECURITY_INFORMATION {
    if protected {
        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION
    } else {
        DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION
    }
}

/// Converge a file/dir's DACL by path (with propagation to existing children),
/// keeping its protection state.
fn apply_named(path: &Path, grants: &[Grant<'_>], drop_sids: &[&[u8]]) -> anyhow::Result<()> {
    let w = verbatim_wide(path);
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `w` valid; psd receives a LocalAlloc SD; dacl points into it.
    let code = unsafe {
        GetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut psd,
        )
    };
    win_ok(code, "GetNamedSecurityInfoW")?;
    let _sd = OwnedSd(psd);
    let new = rebuild(dacl, grants, drop_sids)?;
    // SAFETY: new is a valid ACL buffer.
    let code = unsafe {
        SetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            protection(sd_protected(psd)),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new.as_ptr() as *const ACL,
            std::ptr::null(),
        )
    };
    win_ok(code, "SetNamedSecurityInfoW")
}

/// Converge a file/dir's DACL on an open handle: no propagation, keeps
/// protection.
fn apply_handle(h: HANDLE, grants: &[Grant<'_>], drop_sids: &[&[u8]]) -> anyhow::Result<()> {
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: h valid; psd receives a LocalAlloc SD.
    let code = unsafe {
        GetSecurityInfo(
            h,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut psd,
        )
    };
    win_ok(code, "GetSecurityInfo")?;
    let _sd = OwnedSd(psd);
    let new = rebuild(dacl, grants, drop_sids)?;
    // SAFETY: new is a valid ACL buffer.
    let code = unsafe {
        SetSecurityInfo(
            h,
            SE_FILE_OBJECT,
            protection(sd_protected(psd)),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new.as_ptr() as *const ACL,
            std::ptr::null(),
        )
    };
    win_ok(code, "SetSecurityInfo")
}

/// Applies `aces` for one SID on one object by handle (no reparse follow).
fn stamp_object(
    path: &Path,
    sid_str: &str,
    aces: &[Ace],
    reject_reparse: bool,
) -> anyhow::Result<()> {
    let sid = LocalPsid::from_string(sid_str)?;
    let bytes = sid.as_bytes();
    let h = open_no_follow(path, reject_reparse)?;
    apply_handle(h.raw(), &[Grant { sid: &sid, aces }], &[&bytes])
}

/// Opens `path` for a DACL rewrite without following reparse points. When
/// `reject_reparse`, errors if the object itself is a reparse point.
fn open_no_follow(path: &Path, reject_reparse: bool) -> anyhow::Result<OwnedHandle> {
    let w = verbatim_wide(path);
    // SAFETY: `w` is a valid path.
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            READ_CONTROL | WRITE_DAC,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE || h.is_null() {
        anyhow::bail!("CreateFileW (os error {})", last_error());
    }
    let owned = OwnedHandle(h);
    if reject_reparse {
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: owned is a valid open handle.
        ok(
            unsafe { GetFileInformationByHandle(owned.raw(), &mut info) },
            "GetFileInformationByHandle",
        )?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            anyhow::bail!("reparse point");
        }
    }
    Ok(owned)
}

// ─── public operations ──────────────────────────────────────────────

/// Grants the slot SID modify on the whole tree (propagated to existing
/// children), plus an Owner Rights read ACE so the slot cannot lock the user
/// out of files it creates. Then denies write to every hard-linked file (a
/// shared body reaches outside). Returns how many were denied. Idempotent: if
/// the root already carries the slot ACE, nothing propagates. `progress` gets
/// the running file count.
pub fn grant_folder(
    root: &Path,
    slot_sid: &str,
    mut progress: impl FnMut(usize),
) -> anyhow::Result<usize> {
    open_no_follow(root, true)?;
    if !has_ace(root, slot_sid) {
        let slot = LocalPsid::from_string(slot_sid)?;
        let slot_bytes = slot.as_bytes();
        let owner = LocalPsid::from_string(SID_OWNER_RIGHTS)?;
        let owner_bytes = owner.as_bytes();
        apply_named(
            root,
            &[
                Grant {
                    sid: &slot,
                    aces: &[Ace::allow(MODIFY_NO_FDC, OICI)],
                },
                Grant {
                    sid: &owner,
                    aces: &[Ace::allow(READ_CONTROL, OICI)],
                },
            ],
            &[&slot_bytes, &owner_bytes],
        )?;
    }
    let mut count = 0usize;
    let mut seen = 0usize;
    walk(root, &mut |path, kind| {
        seen += 1;
        if seen.is_multiple_of(500) {
            progress(seen);
        }
        if kind == Kind::File
            && hard_linked(path)
            && stamp_if_needed(path, slot_sid, &[Ace::deny(DENY_LINK_WRITE, NO_INHERIT)]).is_ok()
        {
            count += 1;
        }
    });
    Ok(count)
}

/// Stamps deny ACEs on the folder's protected names (`.git`, `.claude`, shell
/// and IDE configs, `.cctg`, …), like Claude Code's sandbox. `create` also
/// creates `.claude`, `.git/hooks`, `.cctg/inbox` as the real user so a
/// command cannot plant an executable one. Idempotent: an object that already
/// has an explicit deny of this SID is left alone.
pub fn stamp_protected(root: &Path, slot_sid: &str, create: bool) -> anyhow::Result<()> {
    let dir_deny = |mask: u32| vec![Ace::deny(mask, NO_INHERIT), Ace::deny(mask, OICI_IO)];
    let file_deny = |mask: u32| vec![Ace::deny(mask, NO_INHERIT)];

    let git = root.join(".git");
    match std::fs::symlink_metadata(&git) {
        Ok(meta) if meta.is_dir() => {
            stamp_if_needed(&git, slot_sid, &file_deny(DENY_REPLACE))?;
            let config = git.join("config");
            if config.exists() {
                stamp_if_needed(&config, slot_sid, &file_deny(DENY_WRITE))?;
            }
            let hooks = git.join("hooks");
            if create && !hooks.exists() {
                let _ = std::fs::create_dir_all(&hooks);
            }
            if hooks.exists() {
                stamp_if_needed(&hooks, slot_sid, &dir_deny(DENY_WRITE))?;
            }
        }
        Ok(_) => stamp_if_needed(&git, slot_sid, &file_deny(DENY_WRITE))?,
        Err(_) => {}
    }

    for name in [".claude", ".vscode", ".idea"] {
        let dir = root.join(name);
        if create && name == ".claude" && !dir.exists() {
            let _ = std::fs::create_dir_all(&dir);
        }
        if dir.exists() {
            stamp_if_needed(&dir, slot_sid, &dir_deny(DENY_WRITE))?;
        }
    }

    for name in super::super::paths::PROTECTED_TOP {
        let path = root.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => {
                stamp_if_needed(&path, slot_sid, &file_deny(DENY_REPLACE))?
            }
            Ok(_) => stamp_if_needed(&path, slot_sid, &file_deny(DENY_WRITE))?,
            Err(_) => {}
        }
    }

    let cctg = root.join(".cctg");
    if cctg.exists() {
        stamp_if_needed(&cctg, slot_sid, &file_deny(DENY_REPLACE))?;
    }
    let inbox = cctg.join("inbox");
    if create && cctg.exists() && !inbox.exists() {
        let _ = std::fs::create_dir_all(&inbox);
    }
    if inbox.exists() {
        stamp_if_needed(&inbox, slot_sid, &dir_deny(DENY_WRITE))?;
    }
    Ok(())
}

/// Stamps `aces` on `path` unless it already carries an explicit deny of
/// `slot_sid` (or, for a grant, any ACE). Cheap idempotency.
fn stamp_if_needed(path: &Path, slot_sid: &str, aces: &[Ace]) -> anyhow::Result<()> {
    let already = if aces.iter().all(|a| a.deny) {
        has_deny(path, slot_sid)
    } else {
        has_ace(path, slot_sid)
    };
    if already {
        return Ok(());
    }
    stamp_object(path, slot_sid, aces, false)
}

/// The protected names above (and `.git` as a whole) whose owner is the slot
/// SID: created by a command in the sandbox.
pub fn slot_owned_protected(root: &Path, slot_sid: &str) -> Vec<PathBuf> {
    let mut candidates = vec![
        root.join(".git"),
        root.join(".claude"),
        root.join(".vscode"),
        root.join(".idea"),
        root.join(".cctg"),
    ];
    for name in super::super::paths::PROTECTED_TOP {
        candidates.push(root.join(name));
    }
    candidates
        .into_iter()
        .filter(|path| path.exists() && owner_is(path, slot_sid))
        .collect()
}

/// Removes the slot SID's ACEs from the whole tree (walk, then the root so
/// propagation drops the inherited copies). Per-object errors are counted.
pub fn revoke_folder(root: &Path, slot_sid: &str) -> usize {
    let slot = match LocalPsid::from_string(slot_sid) {
        Ok(sid) => sid,
        Err(_) => return 1,
    };
    let slot_bytes = slot.as_bytes();
    let mut errors = 0usize;
    walk(root, &mut |path, _kind| {
        if has_ace(path, slot_sid) {
            match open_no_follow(path, false) {
                Ok(h) => {
                    if apply_handle(h.raw(), &[], &[&slot_bytes]).is_err() {
                        errors += 1;
                    }
                }
                Err(_) => errors += 1,
            }
        }
    });
    let owner_bytes = sid_bytes(SID_OWNER_RIGHTS).unwrap_or_default();
    if apply_named(root, &[], &[&slot_bytes, &owner_bytes]).is_err() {
        errors += 1;
    }
    errors
}

/// Grants `sid` `mask` on one file (no propagation, no reparse follow).
pub fn grant_file(path: &Path, sid: &str, mask: u32) -> anyhow::Result<()> {
    stamp_object(path, sid, &[Ace::allow(mask, NO_INHERIT)], true)
}

/// Removes `sid`'s ACEs from one file.
pub fn revoke_file(path: &Path, sid: &str) -> anyhow::Result<()> {
    let psid = LocalPsid::from_string(sid)?;
    let bytes = psid.as_bytes();
    let h = open_no_follow(path, false)?;
    apply_handle(h.raw(), &[], &[&bytes])
}

/// Grants a group read+execute on a program directory, propagated.
pub fn grant_tree_read(dir: &Path, group_sid: &str) -> anyhow::Result<()> {
    let sid = LocalPsid::from_string(group_sid)?;
    let bytes = sid.as_bytes();
    apply_named(
        dir,
        &[Grant {
            sid: &sid,
            aces: &[Ace::allow(READ_EXEC, OICI)],
        }],
        &[&bytes],
    )
}

/// Removes a group's read ACE from a program directory, propagated.
pub fn revoke_tree_read(dir: &Path, group_sid: &str) -> anyhow::Result<()> {
    let sid = LocalPsid::from_string(group_sid)?;
    let bytes = sid.as_bytes();
    apply_named(dir, &[], &[&bytes])
}

/// Denies the group read on a directory tree (the credential store), propagated.
pub fn deny_group_read(dir: &Path, group_sid: &str) -> anyhow::Result<()> {
    let sid = LocalPsid::from_string(group_sid)?;
    let bytes = sid.as_bytes();
    apply_named(
        dir,
        &[Grant {
            sid: &sid,
            aces: &[Ace::deny(FILE_GENERIC_READ, OICI)],
        }],
        &[&bytes],
    )
}

/// Denies the group write on one object (ambient world-writable dirs), no
/// reparse follow. Best-effort: an absent object is skipped by the caller.
pub fn deny_group_write(path: &Path, group_sid: &str) -> anyhow::Result<()> {
    let sid = LocalPsid::from_string(group_sid)?;
    let bytes = sid.as_bytes();
    let h = open_no_follow(path, false)?;
    apply_handle(
        h.raw(),
        &[Grant {
            sid: &sid,
            aces: &[
                Ace::deny(DENY_WRITE, NO_INHERIT),
                Ace::deny(DENY_WRITE, OICI_IO),
            ],
        }],
        &[&bytes],
    )
}

/// Removes the group's ambient write-deny from one object.
pub fn undeny_group_write(path: &Path, group_sid: &str) -> anyhow::Result<()> {
    revoke_file(path, group_sid)
}

/// Whether `path`'s DACL carries any ACE for `sid`.
pub fn has_ace(path: &Path, sid: &str) -> bool {
    ace_present(path, sid, |_| true)
}

/// Whether the object carries an explicit DENY ACE for `sid`.
fn has_deny(path: &Path, sid: &str) -> bool {
    ace_present(path, sid, |hdr| hdr.AceType == ACCESS_DENIED)
}

fn ace_present(path: &Path, sid: &str, want: impl Fn(&ACE_HEADER) -> bool) -> bool {
    let bytes = match sid_bytes(sid) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    with_dacl(path, |dacl| {
        kept_aces(dacl, |hdr, body| want(hdr) && ace_sid_is(body, &bytes))
            .map(|kept| !kept.is_empty())
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

/// Reads `path`'s DACL and runs `f` on the ACL pointer.
fn with_dacl<T>(path: &Path, f: impl FnOnce(*const ACL) -> T) -> Option<T> {
    let w = verbatim_wide(path);
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `w` valid; psd receives a LocalAlloc SD; dacl points into it.
    let code = unsafe {
        GetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut psd,
        )
    };
    if code != 0 {
        return None;
    }
    let _sd = OwnedSd(psd);
    Some(f(dacl))
}

/// Whether `path`'s owner is `sid`.
pub fn owner_is(path: &Path, sid: &str) -> bool {
    let want = match sid_bytes(sid) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    let w = verbatim_wide(path);
    let mut owner: PSID = std::ptr::null_mut();
    let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `w` valid; psd receives a LocalAlloc SD; owner points into it.
    let code = unsafe {
        GetNamedSecurityInfoW(
            w.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut psd,
        )
    };
    if code != 0 || owner.is_null() {
        return false;
    }
    let _sd = OwnedSd(psd);
    // SAFETY: owner is a valid SID within the SD.
    let len = unsafe { GetLengthSid(owner) } as usize;
    let have = unsafe { std::slice::from_raw_parts(owner as *const u8, len) };
    same_sid(have, &want)
}

/// An ancestor of `folder` up to `home` (inclusive) that a broad principal (or
/// the `cctg-sandbox` group / a slot) may read or write.
pub fn shared_ancestor(folder: &Path, home: &Path) -> Option<PathBuf> {
    let mut broad: Vec<Vec<u8>> = BROAD_SIDS
        .iter()
        .filter_map(|s| sid_bytes(s).ok())
        .collect();
    if let Some(mark) = super::read_mark() {
        broad.extend(sid_bytes(&mark.group_sid).ok());
        for (_, sid) in &mark.slot_sids {
            broad.extend(sid_bytes(sid).ok());
        }
    }
    let within = super::super::paths::within;
    let mut current = folder.parent()?;
    loop {
        // Only ancestors within the profile matter.
        if !within(home, current) {
            break;
        }
        if with_dacl(current, |dacl| dacl_grants_any(dacl, &broad)).unwrap_or(false) {
            return Some(current.to_path_buf());
        }
        if within(current, home) {
            break; // reached the profile root
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    None
}

/// Whether the DACL has an ALLOW ACE granting read or write to any of `sids`.
fn dacl_grants_any(dacl: *const ACL, sids: &[Vec<u8>]) -> bool {
    const RW: u32 =
        FILE_GENERIC_READ | FILE_GENERIC_WRITE | 0x1000_0000 | 0x8000_0000 | 0x4000_0000;
    kept_aces(dacl, |hdr, body| {
        if hdr.AceType != ACCESS_ALLOWED || body.len() < 8 {
            return false;
        }
        let mask = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
        mask & RW != 0 && sids.iter().any(|s| ace_sid_is(body, s))
    })
    .map(|kept| !kept.is_empty())
    .unwrap_or(false)
}

/// Removes `path` without following reparse points (a slot-created bare-repo
/// name after a command).
pub fn remove_no_follow(path: &Path) {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
            if meta.is_dir() {
                let _ = std::fs::remove_dir(path);
            } else {
                let _ = std::fs::remove_file(path);
            }
        }
        Ok(meta) if meta.is_dir() => {
            let _ = std::fs::remove_dir_all(path);
        }
        Ok(_) => {
            let _ = std::fs::remove_file(path);
        }
        Err(_) => {}
    }
}

// ─── tree walk ──────────────────────────────────────────────────────

#[derive(PartialEq, Eq, Clone, Copy)]
enum Kind {
    File,
    Dir,
    Reparse,
}

/// Visits every descendant of `root` (not `root` itself), never descending
/// into a reparse point.
fn walk(root: &Path, f: &mut dyn FnMut(&Path, Kind)) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(_) => continue,
        };
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            f(&path, Kind::Reparse);
        } else if meta.is_dir() {
            f(&path, Kind::Dir);
            walk(&path, f);
        } else {
            f(&path, Kind::File);
        }
    }
}

/// Whether the regular file at `path` has more than one hard link.
fn hard_linked(path: &Path) -> bool {
    let h = match open_no_follow(path, false) {
        Ok(h) => h,
        Err(_) => return false,
    };
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: h is a valid handle.
    if unsafe { GetFileInformationByHandle(h.raw(), &mut info) } == 0 {
        return false;
    }
    info.nNumberOfLinks > 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::testdir::TempDir;

    /// `S-1-5-32-546` (Guests): a well-known SID that is never the test
    /// runner, so its ACEs can be written without provisioning an account.
    const FAKE_SLOT: &str = "S-1-5-32-546";

    fn canon(p: &Path) -> PathBuf {
        crate::sandbox::paths::canonical(p).unwrap()
    }

    #[test]
    fn grant_then_revoke_leaves_no_slot_ace() {
        let tmp = TempDir::new("acl-grant");
        let root = canon(tmp.path()).join("proj");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("a.rs"), b"x").unwrap();
        assert!(!has_ace(&root, FAKE_SLOT));
        let n = grant_folder(&root, FAKE_SLOT, |_| {}).unwrap();
        assert_eq!(n, 0, "no hard links");
        assert!(has_ace(&root, FAKE_SLOT), "root carries the slot ACE");
        assert!(has_ace(&root.join("src").join("a.rs"), FAKE_SLOT));
        let errors = revoke_folder(&root, FAKE_SLOT);
        assert_eq!(errors, 0, "revoke clean");
        assert!(!has_ace(&root, FAKE_SLOT), "slot ACE gone from root");
        assert!(!has_ace(&root.join("src").join("a.rs"), FAKE_SLOT));
    }

    #[test]
    fn stamp_protects_git_and_claude() {
        let tmp = TempDir::new("acl-stamp");
        let root = canon(tmp.path()).join("proj");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git").join("config"), b"[core]").unwrap();
        std::fs::write(root.join(".mcp.json"), b"{}").unwrap();
        grant_folder(&root, FAKE_SLOT, |_| {}).unwrap();
        stamp_protected(&root, FAKE_SLOT, true).unwrap();
        assert!(has_deny(&root.join(".git"), FAKE_SLOT));
        assert!(has_deny(&root.join(".git").join("config"), FAKE_SLOT));
        assert!(
            root.join(".git").join("hooks").is_dir(),
            ".git/hooks created"
        );
        assert!(has_deny(&root.join(".git").join("hooks"), FAKE_SLOT));
        assert!(root.join(".claude").is_dir(), ".claude created");
        assert!(has_deny(&root.join(".claude"), FAKE_SLOT));
        assert!(has_deny(&root.join(".mcp.json"), FAKE_SLOT));
        stamp_protected(&root, FAKE_SLOT, false).unwrap();
        revoke_folder(&root, FAKE_SLOT);
        assert!(!has_deny(&root.join(".git"), FAKE_SLOT));
    }

    #[test]
    fn a_reparse_root_is_rejected() {
        let tmp = TempDir::new("acl-reparse");
        let real = canon(tmp.path()).join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&real)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if made {
            assert!(grant_folder(&link, FAKE_SLOT, |_| {}).is_err());
        }
    }

    #[test]
    fn a_hard_linked_file_is_write_denied() {
        let tmp = TempDir::new("acl-hardlink");
        let base = canon(tmp.path());
        let root = base.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        let inside = root.join("shared");
        std::fs::write(&inside, b"data").unwrap();
        let outside = base.join("outside");
        if std::fs::hard_link(&inside, &outside).is_ok() {
            let n = grant_folder(&root, FAKE_SLOT, |_| {}).unwrap();
            assert_eq!(n, 1, "the shared body is denied");
            assert!(has_deny(&inside, FAKE_SLOT));
            revoke_folder(&root, FAKE_SLOT);
        }
    }

    #[test]
    fn shared_ancestor_finds_a_broad_ace() {
        let tmp = TempDir::new("acl-shared");
        let home = canon(tmp.path()).join("home");
        let mid = home.join("mid");
        let folder = mid.join("proj");
        std::fs::create_dir_all(&folder).unwrap();
        assert_eq!(shared_ancestor(&folder, &home), None, "clean tree");
        stamp_object(&mid, "S-1-5-32-546", &[Ace::allow(READ_EXEC, OICI)], false).unwrap();
        assert_eq!(
            shared_ancestor(&folder, &home).as_deref(),
            Some(mid.as_path())
        );
        revoke_file(&mid, "S-1-5-32-546").unwrap();
    }
}
