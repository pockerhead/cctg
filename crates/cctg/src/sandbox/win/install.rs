//! `cctg sandbox-install [--slots N]`, its hidden `--elevated-step`, and
//! `cctg sandbox-uninstall`. One UAC prompt: the non-elevated part generates
//! slot passwords and writes a request, the elevated part creates the group,
//! the accounts, the LSA denials and the ambient write-denies and writes the
//! install mark. Elevated FFI is in [`super::user`]; this module orchestrates.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TokenElevation};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_WOW64_64KEY, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE,
    REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW,
};

use super::util::{lookup_account_sid, wide, win_ok};
use super::{DEFAULT_SLOTS, GROUP, MARK_KEY_LOCAL, MAX_SLOTS, SETUP_VERSION, win_dir};

/// What the non-elevated part asks the elevated part to do.
#[derive(Debug, Serialize, Deserialize)]
struct Request {
    #[serde(default)]
    uninstall: bool,
    #[serde(default)]
    slots: u32,
    #[serde(default)]
    recreate: Vec<u32>,
    #[serde(default)]
    owner_sid: String,
    #[serde(default)]
    win_dir: String,
}

/// `cctg sandbox-install`.
pub fn install(slots: Option<u32>, elevated_step: Option<PathBuf>) -> i32 {
    if let Some(request) = elevated_step {
        return elevated(&request);
    }
    match install_non_elevated(slots) {
        Ok(()) => 0,
        Err(msg) => {
            eprintln!("cctg sandbox-install: {msg}");
            1
        }
    }
}

/// `cctg sandbox-uninstall`.
pub fn uninstall(elevated_step: Option<PathBuf>) -> i32 {
    if let Some(request) = elevated_step {
        return elevated(&request);
    }
    match uninstall_non_elevated() {
        Ok(()) => 0,
        Err(msg) => {
            eprintln!("cctg sandbox-uninstall: {msg}");
            1
        }
    }
}

fn home() -> Result<PathBuf, String> {
    super::super::home_dir_of(&|n| std::env::var(n).ok()).ok_or_else(|| "no home directory".into())
}

fn install_non_elevated(slots: Option<u32>) -> Result<(), String> {
    let home = home()?;
    let win = win_dir(&home);
    let me = super::current_user_sid().map_err(|_| "cannot resolve your SID")?;
    let existing = super::read_mark();
    if let Some(mark) = &existing {
        if !mark.owner_sid.eq_ignore_ascii_case(&me) {
            return Err("the sandbox on this computer belongs to another Windows user".into());
        }
        if mark
            .slot_sids
            .iter()
            .any(|(_, sid)| sid.eq_ignore_ascii_case(&me))
        {
            return Err("run this from your own account, not a sandbox account".into());
        }
    }
    // The slot count never shrinks.
    let current = existing.as_ref().map(|m| m.slots).unwrap_or(0);
    let want = slots.unwrap_or(current.max(DEFAULT_SLOTS));
    if want < current {
        return Err(format!(
            "the sandbox already has {current} slots; --slots cannot shrink it"
        ));
    }
    if want == 0 || want > MAX_SLOTS {
        return Err(format!("--slots must be between 1 and {MAX_SLOTS}"));
    }
    // Which slots to (re)create: all on the first install, else retired and
    // newly-added numbers.
    let recreate: Vec<u32> = match &existing {
        None => (1..=want).collect(),
        Some(_) => {
            let retired = super::slots::folders(&win)
                .map_err(|_| "slots.json unreadable")?
                .into_iter()
                .filter(|(_, _, active)| !active)
                .map(|(k, _, _)| k);
            let added = (current + 1)..=want;
            let mut set: Vec<u32> = retired.chain(added).collect();
            set.sort_unstable();
            set.dedup();
            set
        }
    };
    super::super::create_private_dir(&win).map_err(|_| "cannot make ~/.cctg/sandbox/win")?;
    for k in &recreate {
        let password = super::cred::generate().map_err(|_| "cannot generate a password")?;
        super::cred::write_pending(&win, *k, &password).map_err(|_| "cannot write a credential")?;
    }
    let request = Request {
        uninstall: false,
        slots: want,
        recreate: recreate.clone(),
        owner_sid: me,
        win_dir: win.to_string_lossy().to_string(),
    };
    let request_path = write_request(&win, &request)?;
    let code = run_elevated(&request_path)?;
    let log = win.join("install.log");
    let ok_now = code == 0 && super::read_mark().is_some_and(|m| m.version == SETUP_VERSION);
    if ok_now {
        for k in &recreate {
            let new = super::cred::cred_file(&win, *k, true);
            let final_ = super::cred::cred_file(&win, *k, false);
            let _ = std::fs::rename(&new, &final_);
        }
        // Retired slots are recreated with new SIDs; forget their folder link.
        if let Ok(folders) = super::slots::folders(&win) {
            for (_, folder, active) in folders {
                if !active {
                    let _ = super::slots::forget(&win, &folder);
                }
            }
        }
        if let Some(group_sid) = super::read_mark().map(|m| m.group_sid) {
            let _ = super::acl::deny_group_read(&win, &group_sid);
        }
        print_log(&log);
        let _ = std::fs::remove_file(&request_path);
        println!(
            "Сэндбокс установлен: группа {GROUP}, {want} скрытых учёток. Теперь в папке внутри \
             профиля выполните cctg sandbox on."
        );
        Ok(())
    } else {
        for k in &recreate {
            let _ = std::fs::remove_file(super::cred::cred_file(&win, *k, true));
        }
        let _ = std::fs::remove_file(&request_path);
        print_log(&log);
        if code == ECANCELLED {
            return Err("установка отменена".into());
        }
        Err("установка не удалась (см. вывод выше)".into())
    }
}

fn uninstall_non_elevated() -> Result<(), String> {
    let home = home()?;
    let win = win_dir(&home);
    let mark = super::read_mark().ok_or("the sandbox is not installed")?;
    let me = super::current_user_sid().map_err(|_| "cannot resolve your SID")?;
    if !mark.owner_sid.eq_ignore_ascii_case(&me) {
        return Err("the sandbox belongs to another Windows user".into());
    }
    // Revoke the folder ACEs and read-dir grants we can, then the accounts.
    if let Ok(folders) = super::slots::folders(&win) {
        for (k, folder, _active) in folders {
            if let Some(sid) = mark.slot_sid(k) {
                super::acl::revoke_folder(Path::new(&folder), sid);
            }
        }
    }
    if let Ok(dirs) = super::slots::read_dirs(&win) {
        for dir in dirs {
            let _ = super::acl::revoke_tree_read(Path::new(&dir), &mark.group_sid);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        let _ = super::acl::revoke_file(&exe, &mark.group_sid);
    }
    let request = Request {
        uninstall: true,
        slots: mark.slots,
        recreate: Vec::new(),
        owner_sid: me,
        win_dir: win.to_string_lossy().to_string(),
    };
    let request_path = write_request(&win, &request)?;
    let code = run_elevated(&request_path)?;
    let _ = std::fs::remove_file(&request_path);
    if code != 0 {
        return Err("удаление не удалось (см. вывод выше)".into());
    }
    // The whole win directory goes; folder marks stay (fail closed until the
    // user runs cctg sandbox off).
    let _ = std::fs::remove_dir_all(&win);
    println!("Сэндбокс удалён. Метки папок остались; снимите их через cctg sandbox off.");
    Ok(())
}

/// The elevated half: reads the request and does the privileged steps.
fn elevated(request_path: &Path) -> i32 {
    let request: Request = match std::fs::read(request_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    {
        Some(request) => request,
        None => {
            eprintln!("cctg sandbox-install: bad request");
            return 1;
        }
    };
    if !is_elevated() {
        eprintln!("cctg sandbox-install: the elevated step needs administrator rights");
        return 1;
    }
    let win = PathBuf::from(&request.win_dir);
    if !win.is_absolute() {
        eprintln!("cctg sandbox-install: bad request");
        return 1;
    }
    let mut log = String::new();
    let result = if request.uninstall {
        elevated_uninstall(&mut log)
    } else {
        elevated_install(&request, &mut log)
    };
    let _ = std::fs::write(win.join("install.log"), log.as_bytes());
    match result {
        Ok(()) => 0,
        Err(msg) => {
            eprintln!("cctg sandbox-install: {msg}");
            1
        }
    }
}

fn elevated_install(request: &Request, log: &mut String) -> Result<(), String> {
    if request.slots == 0 || request.slots > MAX_SLOTS {
        return Err("bad slot count".into());
    }
    if request
        .recreate
        .iter()
        .any(|k| *k == 0 || *k > request.slots)
    {
        return Err("bad recreate set".into());
    }
    let win = PathBuf::from(&request.win_dir);
    super::user::ensure_group().map_err(|e| e.to_string())?;
    let group_sid = lookup_account_sid(GROUP).ok_or("resolve group SID")?;
    let mut slot_sids: Vec<(u32, String)> = Vec::new();
    // Keep the SIDs of the slots not being recreated.
    if let Some(mark) = super::read_mark() {
        for (k, sid) in mark.slot_sids {
            if !request.recreate.contains(&k) && k <= request.slots {
                slot_sids.push((k, sid));
            }
        }
    }
    for k in &request.recreate {
        let password =
            super::cred::load_pending(&win, *k).map_err(|_| "read pending credential")?;
        let sid = super::user::recreate(*k, &password).map_err(|e| e.to_string())?;
        log.push_str(&format!("recreated slot {k}\n"));
        slot_sids.push((*k, sid));
    }
    slot_sids.sort_by_key(|(k, _)| *k);
    super::user::lsa_deny(&group_sid).map_err(|e| e.to_string())?;
    // Ambient write-denies (best-effort per path).
    for target in ambient_deny_targets() {
        if target.is_dir() {
            let _ = super::acl::deny_group_write(&target, &group_sid);
            log.push_str(&format!("ambient deny {}\n", target.display()));
        }
    }
    write_mark(request.slots, &request.owner_sid, &group_sid, &slot_sids)
        .map_err(|e| e.to_string())?;
    log.push_str("install mark written\n");
    Ok(())
}

fn elevated_uninstall(log: &mut String) -> Result<(), String> {
    let mark = super::read_mark().ok_or("no install mark")?;
    if let Some(group_sid) = lookup_account_sid(GROUP) {
        for target in ambient_deny_targets() {
            if target.is_dir() {
                let _ = super::acl::undeny_group_write(&target, &group_sid);
            }
        }
    }
    super::user::remove_all(&mark).map_err(|e| e.to_string())?;
    delete_mark().map_err(|e| e.to_string())?;
    log.push_str("accounts, group and mark removed\n");
    Ok(())
}

/// The world-writable system directories to deny the group write (srt list).
fn ambient_deny_targets() -> Vec<PathBuf> {
    let var = |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.into());
    let program_data = var("ProgramData", r"C:\ProgramData");
    let public = var("PUBLIC", r"C:\Users\Public");
    let system_root = var("SystemRoot", r"C:\Windows");
    let sr = |tail: &str| PathBuf::from(format!(r"{system_root}\{tail}"));
    vec![
        PathBuf::from(program_data),
        PathBuf::from(public),
        sr("Temp"),
        sr("Tasks"),
        sr("tracing"),
        sr(r"Registration\CRMLog"),
        sr(r"System32\FxsTmp"),
        sr(r"System32\com\dmp"),
        sr(r"System32\spool\PRINTERS"),
        sr(r"System32\spool\drivers\color"),
        sr(r"SysWOW64\FxsTmp"),
        sr(r"SysWOW64\com\dmp"),
        sr(r"SysWOW64\Tasks"),
    ]
}

const ECANCELLED: i32 = 1223; // ERROR_CANCELLED

/// Whether this process's token is elevated.
fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: current-process pseudo-handle; TOKEN_QUERY = 8.
    if unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut token) } == 0 {
        return false;
    }
    let token = super::util::OwnedHandle(token);
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut len = 0u32;
    // SAFETY: elevation buffer is the right size.
    let r = unsafe {
        GetTokenInformation(
            token.raw(),
            TokenElevation,
            &mut elevation as *mut _ as *mut c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };
    r != 0 && elevation.TokenIsElevated != 0
}

/// Runs the elevated step: in-process when already elevated (CI), else via a
/// hidden UAC (`runas`). Returns the exit code (or `ECANCELLED`).
fn run_elevated(request_path: &Path) -> Result<i32, String> {
    if is_elevated() {
        // Re-dispatch in this process.
        let exe = std::env::current_exe().map_err(|_| "no executable")?;
        let status = std::process::Command::new(exe)
            .arg("sandbox-install")
            .arg("--elevated-step")
            .arg(request_path)
            .status()
            .map_err(|_| "cannot run the elevated step")?;
        return Ok(status.code().unwrap_or(1));
    }
    shell_execute_runas(request_path)
}

/// `ShellExecuteExW(runas)` of `cctg sandbox-install --elevated-step <path>`,
/// hidden, waited on.
fn shell_execute_runas(request_path: &Path) -> Result<i32, String> {
    use windows_sys::Win32::Foundation::ERROR_CANCELLED;
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, INFINITE, WaitForSingleObject,
    };
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let exe = std::env::current_exe().map_err(|_| "no executable")?;
    let verb = wide("runas");
    let file = wide(&exe.to_string_lossy());
    let params = wide(&format!(
        "sandbox-install --elevated-step \"{}\"",
        request_path.to_string_lossy()
    ));
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE;
    // SAFETY: info is a valid SHELLEXECUTEINFOW.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let err = super::util::last_error();
        return if err == ERROR_CANCELLED {
            Ok(ECANCELLED)
        } else {
            Err("could not launch the elevated step".into())
        };
    }
    if info.hProcess.is_null() {
        return Err("the elevated step did not start".into());
    }
    // SAFETY: hProcess is valid.
    unsafe {
        let _ = WaitForSingleObject(info.hProcess, INFINITE);
    }
    let mut code = 1u32;
    // SAFETY: hProcess is valid.
    unsafe {
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
    }
    if code == WAIT_OBJECT_0 {
        // unreachable normally; keep the code as-is
    }
    Ok(code as i32)
}

fn write_request(win: &Path, request: &Request) -> Result<PathBuf, String> {
    let path = win.join("install-request.json");
    let bytes = serde_json::to_vec(request).map_err(|_| "encode request")?;
    std::fs::write(&path, &bytes).map_err(|_| "write request")?;
    Ok(path)
}

fn print_log(log: &Path) {
    if let Ok(text) = std::fs::read_to_string(log) {
        for line in text.lines() {
            println!("  {line}");
        }
    }
}

// ─── registry writes (elevated) ─────────────────────────────────────

fn write_mark(
    slots: u32,
    owner_sid: &str,
    group_sid: &str,
    slot_sids: &[(u32, String)],
) -> anyhow::Result<()> {
    let key = create_mark_key()?;
    reg_dword(key, "SetupVersion", SETUP_VERSION)?;
    reg_dword(key, "Slots", slots)?;
    reg_sz(key, "OwnerSid", owner_sid)?;
    reg_sz(key, "GroupSid", group_sid)?;
    for (k, sid) in slot_sids {
        reg_sz(key, &format!("Slot{k}Sid"), sid)?;
    }
    // SAFETY: key came from RegCreateKeyExW.
    unsafe {
        let _ = RegCloseKey(key);
    }
    Ok(())
}

fn create_mark_key() -> anyhow::Result<HKEY> {
    let sub = wide(MARK_KEY_LOCAL);
    let mut hkey: HKEY = std::ptr::null_mut();
    // SAFETY: valid subkey; null SA inherits the HKLM\SOFTWARE default DACL.
    win_ok(
        unsafe {
            RegCreateKeyExW(
                HKEY_LOCAL_MACHINE,
                sub.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE | KEY_WOW64_64KEY,
                std::ptr::null(),
                &mut hkey,
                std::ptr::null_mut(),
            )
        },
        "RegCreateKeyExW",
    )
    .map(|()| hkey)
}

fn reg_dword(key: HKEY, name: &str, value: u32) -> anyhow::Result<()> {
    let w = wide(name);
    let data = value.to_ne_bytes();
    // SAFETY: valid key/value; 4-byte DWORD.
    win_ok(
        unsafe { RegSetValueExW(key, w.as_ptr(), 0, REG_DWORD, data.as_ptr(), 4) },
        "RegSetValueExW",
    )
}

fn reg_sz(key: HKEY, name: &str, value: &str) -> anyhow::Result<()> {
    let w = wide(name);
    let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = data.len() * 2;
    // SAFETY: valid key/value; `data` is `bytes` bytes.
    win_ok(
        unsafe {
            RegSetValueExW(
                key,
                w.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr() as *const u8,
                bytes as u32,
            )
        },
        "RegSetValueExW",
    )
}

fn delete_mark() -> anyhow::Result<()> {
    let sub = wide(MARK_KEY_LOCAL);
    // SAFETY: valid subkey.
    let code = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, sub.as_ptr()) };
    // ERROR_FILE_NOT_FOUND (2) is fine.
    if code == 0 || code == 2 {
        Ok(())
    } else {
        anyhow::bail!("RegDeleteTreeW code={code}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambient_targets_are_absolute() {
        for t in ambient_deny_targets() {
            assert_eq!(t.to_string_lossy().as_bytes().get(1), Some(&b':'));
        }
    }

    #[test]
    fn a_request_round_trips() {
        let tmp = crate::hub::testdir::TempDir::new("install-req");
        let win = tmp.path().to_path_buf();
        let request = Request {
            uninstall: false,
            slots: 2,
            recreate: vec![1, 2],
            owner_sid: "S-1-5-21-1".into(),
            win_dir: win.to_string_lossy().to_string(),
        };
        let path = write_request(&win, &request).unwrap();
        let back: Request = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(back.slots, 2);
        assert_eq!(back.recreate, vec![1, 2]);
    }
}
