//! `cctg sandbox-exec <line>`: the broker the shell prefix calls. It passes
//! its own two calls (`cctg agent`, `cctg statusline`) straight through, and
//! runs every other command under the folder's slot account via a two-hop
//! `CreateProcessWithLogonW` → `cctg sandbox-runner` on a private desktop, in a
//! kill-on-close job.
//!
//! Form of `srtwin/src_logon.rs` (Apache-2.0), adapted to raw `windows-sys`.
//! stderr carries only fixed English strings (no path, password or command).
//! The launch path is validated by the CI e2e and the user's live check.

use std::path::{Path, PathBuf};
use std::process::Command;

use windows_sys::Win32::Foundation::{
    ERROR_LOGON_FAILURE, ERROR_NOT_SUPPORTED, HANDLE, INVALID_HANDLE_VALUE, SetHandleInformation,
    WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW, GetFileInformationByHandle, OPEN_EXISTING,
    ReadFile, WriteFile,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessWithLogonW,
    GetExitCodeProcess, INFINITE, LOGON_WITH_PROFILE, PROCESS_INFORMATION, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOW, WaitForSingleObject,
};

use super::job::Job;
use super::runner::{RunnerSpec, encode_spec};
use super::util::{OwnedHandle, last_error, ok, wide};
use super::{BROKER_FAILED, COMMAND_VAR, MARK_VAR, slot_user};

const HANDLE_FLAG_INHERIT: u32 = 1;
const DRIVE_REMOTE: u32 = 4;
const READ_CONTROL: u32 = 0x0002_0000;

/// The broker entry point: `line` is the prefix's `$1`.
pub fn run(line: &str) -> i32 {
    match run_inner(line) {
        Ok(code) => code,
        Err(Refusal(msg)) => {
            eprintln!("cctg sandbox: {msg}");
            BROKER_FAILED
        }
    }
}

/// A refusal before the command ran (broker exit code, fixed message).
struct Refusal(&'static str);

impl From<anyhow::Error> for Refusal {
    fn from(_: anyhow::Error) -> Self {
        Refusal("the sandbox could not start")
    }
}

fn run_inner(line: &str) -> Result<i32, Refusal> {
    let exe = std::env::current_exe().map_err(|_| Refusal("no executable"))?;

    // 1. Pass-through of our own agent / statusline.
    if let Some((program, own)) = super::super::prefix::own_call(line)
        && same_file(Path::new(&program), &exe)
    {
        let sub = match own {
            super::super::prefix::Own::Agent => "agent",
            super::super::prefix::Own::Statusline => "statusline",
        };
        let status = Command::new(&exe)
            .arg(sub)
            .env_remove("MSYS_NO_PATHCONV")
            .env_remove("MSYS2_ARG_CONV_EXCL")
            .status()
            .map_err(|_| Refusal("pass-through failed"))?;
        return Ok(status.code().unwrap_or(BROKER_FAILED));
    }

    // 3. The folder must be prepared.
    let mark = std::env::var(MARK_VAR)
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or(Refusal("this folder is not prepared for the sandbox"))?;
    let mark = super::super::paths::canonical(&mark)
        .filter(|m| covered(m))
        .ok_or(Refusal("this folder is not prepared for the sandbox"))?;
    let bash = std::env::var(super::BASH_VAR)
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .ok_or(Refusal("this folder is not prepared for the sandbox"))?;

    // 4. The install must be current, ours, with a live slot and ACE.
    let info = super::read_mark().ok_or(Refusal("run cctg sandbox on again"))?;
    if info.version != super::SETUP_VERSION {
        return Err(Refusal("run cctg sandbox-install again"));
    }
    let me = super::current_user_sid().map_err(|_| Refusal("run cctg sandbox on again"))?;
    if !info.owner_sid.eq_ignore_ascii_case(&me) {
        return Err(Refusal("the sandbox belongs to another Windows user"));
    }
    let win_dir = win_dir()?;
    let mark_str = mark.to_string_lossy().to_string();
    let k = super::slots::active_slot(&win_dir, &mark_str)
        .ok()
        .flatten()
        .ok_or(Refusal("run cctg sandbox on again"))?;
    let slot_sid = info
        .slot_sid(k)
        .ok_or(Refusal("run cctg sandbox on again"))?
        .to_owned();
    if !super::acl::has_ace(&mark, &slot_sid) {
        return Err(Refusal("run cctg sandbox on again"));
    }

    // 5. The runner reads this exe's image; keep the group ACE, and re-stamp
    // protected names the user may have created since `sandbox on`.
    if !super::acl::has_ace(&exe, &info.group_sid) {
        let _ = super::acl::grant_file(&exe, &info.group_sid, super::acl::GROUP_READ_EXEC);
    }
    let _ = super::acl::stamp_protected(&mark, &slot_sid, false);

    // 6. Slot password and broker self-protection.
    let password =
        super::cred::load(&win_dir, k).map_err(|_| Refusal("run cctg sandbox-install"))?;
    let _ = self_protect(&me);

    // 7-8. Build the spec and launch the runner under the slot account.
    let spec = RunnerSpec {
        argv: vec![
            bash.to_string_lossy().to_string(),
            "--noprofile".into(),
            "--norc".into(),
            "-c".into(),
            "eval \"$CCTG_SANDBOX_COMMAND\"".into(),
        ],
        env_overlay: env_overlay(line, &mark, &me, &info.group_sid),
        slot_sid: slot_sid.clone(),
    };
    let code = spawn_runner(&exe, k, &password, &me, &slot_sid, &spec)?;

    // 9. A bare-repo left in the mark root by the command is the slot's; drop
    // it (Claude Code's rule: such names would make the folder a bare repo
    // whose config the user's git would run).
    for name in ["HEAD", "objects", "refs"] {
        let path = mark.join(name);
        if path.exists() && super::acl::owner_is(&path, &slot_sid) {
            super::acl::remove_no_follow(&path);
        }
    }
    Ok(code)
}

/// The folder is covered by a mark on this device.
fn covered(folder: &Path) -> bool {
    let Some(home) = super::super::home_dir_of(&|n| std::env::var(n).ok()) else {
        return false;
    };
    super::super::marks::covered(&super::super::marks_file(&home), folder) == Ok(true)
}

fn win_dir() -> Result<PathBuf, Refusal> {
    let home = super::super::home_dir_of(&|n| std::env::var(n).ok()).ok_or(Refusal("no home"))?;
    Ok(super::win_dir(&home))
}

/// The child's environment overlay: the slot profile plus a whitelist, no
/// `CCTG_*` but `CCTG_SANDBOX`, no tokens.
fn env_overlay(line: &str, mark: &Path, user_sid: &str, _group_sid: &str) -> Vec<(String, String)> {
    let _ = user_sid;
    let mut out: Vec<(String, String)> = Vec::new();
    let get = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    for name in [
        "PATH",
        "TERM",
        "COLORTERM",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "MSYSTEM",
        "CLAUDECODE",
    ] {
        if let Some(v) = get(name) {
            out.push((name.to_owned(), v));
        }
    }
    let mark_str = mark.to_string_lossy().to_string();
    out.push((super::super::ACTIVE_VAR.to_owned(), "1".to_owned()));
    out.push((COMMAND_VAR.to_owned(), line.to_owned()));
    let project = get("CLAUDE_PROJECT_DIR")
        .map(PathBuf::from)
        .filter(|p| super::super::paths::within(mark, p))
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| mark_str.clone());
    out.push(("CLAUDE_PROJECT_DIR".to_owned(), project));
    // HOME stays the real user's (like unix); APPDATA/LOCALAPPDATA stay the
    // slot's (each folder its own).
    if let Some(home) = super::super::home_dir_of(&|n| std::env::var(n).ok()) {
        let home = home.to_string_lossy().to_string();
        for name in ["HOME", "USERPROFILE"] {
            out.push((name.to_owned(), home.clone()));
        }
        if let Some(v) = get("HOMEDRIVE") {
            out.push(("HOMEDRIVE".to_owned(), v));
        }
        if let Some(v) = get("HOMEPATH") {
            out.push(("HOMEPATH".to_owned(), v));
        }
    }
    let own = format!(r"{mark_str}\.cctg\sandbox");
    let tmp = format!(r"{own}\tmp");
    for name in ["TEMP", "TMP"] {
        out.push((name.to_owned(), tmp.clone()));
    }
    // Tool caches in the folder (Windows paths).
    for (name, value) in [
        ("CARGO_HOME", format!(r"{own}\cargo")),
        ("CARGO_TARGET_DIR", format!(r"{mark_str}\target")),
        ("XDG_CACHE_HOME", format!(r"{own}\cache")),
        ("XDG_DATA_HOME", format!(r"{own}\data")),
        ("XDG_STATE_HOME", format!(r"{own}\state")),
        ("XDG_CONFIG_HOME", format!(r"{own}\config")),
        ("npm_config_cache", format!(r"{own}\npm")),
        ("GOPATH", format!(r"{own}\go")),
    ] {
        out.push((name.to_owned(), value));
    }
    // git: the name-only copy, system config kept, credential helper off.
    if let Some(git) = get(super::super::profile::GITCONFIG_VAR) {
        out.push(("GIT_CONFIG_GLOBAL".to_owned(), git));
        out.push(("GIT_CONFIG_COUNT".to_owned(), "1".to_owned()));
        out.push((
            "GIT_CONFIG_KEY_0".to_owned(),
            "credential.helper".to_owned(),
        ));
        out.push(("GIT_CONFIG_VALUE_0".to_owned(), String::new()));
        out.push(("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()));
        out.push(("GCM_INTERACTIVE".to_owned(), "never".to_owned()));
    }
    out
}

/// Whether two paths name the same file (volume serial + file index).
fn same_file(a: &Path, b: &Path) -> bool {
    match (file_id(a), file_id(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

fn file_id(path: &Path) -> Option<(u32, u32, u32)> {
    let w = wide(&path.to_string_lossy());
    // SAFETY: `w` valid; open read-only.
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE || h.is_null() {
        return None;
    }
    let owned = OwnedHandle(h);
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: owned is valid.
    if unsafe { GetFileInformationByHandle(owned.raw(), &mut info) } == 0 {
        return None;
    }
    Some((
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ))
}

/// Broker self-protection: `[SYSTEM, Admins, real user, OwnerRights:RC]`,
/// PROTECTED, so the child cannot open the broker (with the password).
fn self_protect(user_sid: &str) -> anyhow::Result<()> {
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_KERNEL_OBJECT,
        SetSecurityInfo,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let sddl = wide(&format!(
        "D:P(A;;0x1fffff;;;SY)(A;;0x1fffff;;;BA)(A;;0x1fffff;;;{user_sid})(A;;RC;;;S-1-3-4)"
    ));
    let mut psd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: valid SDDL; psd receives a LocalAlloc SD.
    ok(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut psd,
                std::ptr::null_mut(),
            )
        },
        "ConvertStringSecurityDescriptorToSecurityDescriptorW",
    )?;
    let _sd = super::util::OwnedSd(psd);
    let mut present = 0i32;
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut defaulted = 0i32;
    // SAFETY: psd valid.
    ok(
        unsafe { GetSecurityDescriptorDacl(psd, &mut present, &mut dacl, &mut defaulted) },
        "GetSecurityDescriptorDacl",
    )?;
    super::util::win_ok(
        // SAFETY: current-process pseudo-handle; dacl valid.
        unsafe {
            SetSecurityInfo(
                GetCurrentProcess(),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        },
        "SetSecurityInfo(broker DACL)",
    )
}

/// One anonymous pipe; the broker's end is made non-inheritable.
struct Pipe {
    broker: OwnedHandle,
    runner: OwnedHandle,
}

fn make_pipe(runner_writes: bool) -> anyhow::Result<Pipe> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    // SAFETY: valid SA; read/write receive the handles.
    ok(
        unsafe { CreatePipe(&mut read, &mut write, &sa, 0) },
        "CreatePipe",
    )?;
    let (broker, runner) = if runner_writes {
        (OwnedHandle(read), OwnedHandle(write))
    } else {
        (OwnedHandle(write), OwnedHandle(read))
    };
    // SAFETY: broker end must not be inherited by the runner.
    ok(
        unsafe { SetHandleInformation(broker.raw(), HANDLE_FLAG_INHERIT, 0) },
        "SetHandleInformation",
    )?;
    Ok(Pipe { broker, runner })
}

/// Launches `cctg sandbox-runner` under the slot account, writes the spec, and
/// pumps stdio; returns the runner's exit code.
fn spawn_runner(
    exe: &Path,
    k: u32,
    password: &super::cred::Password,
    user_sid: &str,
    slot_sid: &str,
    spec: &RunnerSpec,
) -> Result<i32, Refusal> {
    let cwd = std::env::current_dir().map_err(|_| Refusal("no working directory"))?;
    if is_network_drive(&cwd) {
        return Err(Refusal(
            "the sandbox cannot start on a mapped or network drive",
        ));
    }
    let spec_bytes = encode_spec(spec).map_err(|_| Refusal("spec"))?;
    let stdin = make_pipe(false)?;
    let stdout = make_pipe(true)?;
    let stderr = make_pipe(true)?;
    let mut desk = super::desktop::IsolatedDesk::new(user_sid, slot_sid)?;
    let job = Job::new(true)?;

    let user_w = wide(&slot_user(k));
    let domain_w = wide(".");
    let mut pw_w = password.to_wide();
    let exe_w = wide(&exe.to_string_lossy());
    let mut cmdline_w = wide(&format!("\"{}\" sandbox-runner", exe.to_string_lossy()));
    let cwd_w = wide(&cwd.to_string_lossy());

    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdInput = stdin.runner.raw();
    si.hStdOutput = stdout.runner.raw();
    si.hStdError = stderr.runner.raw();
    si.lpDesktop = desk.path_ptr();
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: all pointers valid; env NULL uses the slot profile env.
    let r = unsafe {
        CreateProcessWithLogonW(
            user_w.as_ptr(),
            domain_w.as_ptr(),
            pw_w.as_ptr(),
            LOGON_WITH_PROFILE,
            exe_w.as_ptr(),
            cmdline_w.as_mut_ptr(),
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW | CREATE_SUSPENDED,
            std::ptr::null(),
            cwd_w.as_ptr(),
            &si,
            &mut pi,
        )
    };
    pw_w.fill(0);
    if r == 0 {
        let err = last_error();
        return Err(if err == ERROR_LOGON_FAILURE {
            Refusal("sandbox account logon failed (run cctg sandbox-install)")
        } else {
            Refusal("the sandbox could not start (is the Secondary Logon service disabled?)")
        });
    }
    let child = OwnedHandle(pi.hProcess);
    let thread = OwnedHandle(pi.hThread);
    // Station and BNO grants for the two-hop attach.
    let _ = super::desktop::grant_sandbox_on_winsta(slot_sid);
    let _ = super::desktop::grant_sandbox_on_session_bno(slot_sid);
    // Assign (ERROR_NOT_SUPPORTED tolerated), then resume.
    if let Err(error) = job.assign(child.raw()) {
        let not_supported = error
            .to_string()
            .contains(&format!("os error {ERROR_NOT_SUPPORTED}"));
        if !not_supported {
            // SAFETY: child is valid.
            unsafe {
                let _ = windows_sys::Win32::System::Threading::TerminateProcess(child.raw(), 1);
            }
            return Err(Refusal("the sandbox could not start"));
        }
    }
    // SAFETY: thread valid.
    if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
        return Err(Refusal("the sandbox could not start"));
    }
    // Close runner-side pipe ends so the pumps see EOF.
    drop(stdin.runner);
    drop(stdout.runner);
    drop(stderr.runner);

    // Write the spec, then pump broker stdin -> runner stdin.
    write_all(stdin.broker.raw(), &spec_bytes)
        .map_err(|_| Refusal("the sandbox could not start"))?;
    let stdin_broker = stdin.broker.into_raw() as isize;
    let t_in = std::thread::spawn(move || {
        // SAFETY: standard input handle.
        let src = unsafe { GetStdHandle(STD_INPUT_HANDLE_LOCAL) };
        pump(src, stdin_broker as HANDLE);
        // SAFETY: close the runner-stdin write end so the runner sees EOF.
        unsafe {
            let _ = windows_sys::Win32::Foundation::CloseHandle(stdin_broker as HANDLE);
        }
    });
    let out_broker = stdout.broker.into_raw() as isize;
    let err_broker = stderr.broker.into_raw() as isize;
    let t_out = std::thread::spawn(move || {
        // SAFETY: standard output handle.
        let dst = unsafe { GetStdHandle(STD_OUTPUT_HANDLE_LOCAL) };
        pump(out_broker as HANDLE, dst);
        unsafe {
            let _ = windows_sys::Win32::Foundation::CloseHandle(out_broker as HANDLE);
        }
    });
    let t_err = std::thread::spawn(move || {
        // SAFETY: standard error handle.
        let dst = unsafe { GetStdHandle(STD_ERROR_HANDLE_LOCAL) };
        pump(err_broker as HANDLE, dst);
        unsafe {
            let _ = windows_sys::Win32::Foundation::CloseHandle(err_broker as HANDLE);
        }
    });

    // SAFETY: child valid.
    let rc = unsafe { WaitForSingleObject(child.raw(), INFINITE) };
    if rc != WAIT_OBJECT_0 {
        return Err(Refusal("the sandbox could not start"));
    }
    let _ = t_out.join();
    let _ = t_err.join();
    let _ = t_in.join();
    let mut code: u32 = 1;
    // SAFETY: child valid.
    unsafe {
        let _ = GetExitCodeProcess(child.raw(), &mut code);
    }
    drop(desk);
    Ok(code as i32)
}

use windows_sys::Win32::System::Console::GetStdHandle;
use windows_sys::Win32::System::Console::{
    STD_ERROR_HANDLE as STD_ERROR_HANDLE_LOCAL, STD_INPUT_HANDLE as STD_INPUT_HANDLE_LOCAL,
    STD_OUTPUT_HANDLE as STD_OUTPUT_HANDLE_LOCAL,
};

/// Copies `src` to `dst` until EOF.
fn pump(src: HANDLE, dst: HANDLE) {
    let mut buf = [0u8; 8192];
    loop {
        let mut read = 0u32;
        // SAFETY: buf is valid.
        let r = unsafe {
            ReadFile(
                src,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if r == 0 || read == 0 {
            break;
        }
        let mut off = 0u32;
        while off < read {
            let mut wrote = 0u32;
            // SAFETY: buf[off..read] is valid.
            let w = unsafe {
                WriteFile(
                    dst,
                    buf[off as usize..read as usize].as_ptr(),
                    read - off,
                    &mut wrote,
                    std::ptr::null_mut(),
                )
            };
            if w == 0 || wrote == 0 {
                return;
            }
            off += wrote;
        }
    }
}

fn write_all(h: HANDLE, bytes: &[u8]) -> anyhow::Result<()> {
    let mut off = 0;
    while off < bytes.len() {
        let mut wrote = 0u32;
        // SAFETY: bytes[off..] valid.
        let w = unsafe {
            WriteFile(
                h,
                bytes[off..].as_ptr(),
                (bytes.len() - off) as u32,
                &mut wrote,
                std::ptr::null_mut(),
            )
        };
        if w == 0 || wrote == 0 {
            anyhow::bail!("write");
        }
        off += wrote as usize;
    }
    Ok(())
}

/// Whether `cwd`'s drive is a network/mapped drive.
fn is_network_drive(cwd: &Path) -> bool {
    let text = cwd.to_string_lossy();
    let bytes = text.as_bytes();
    let root = if bytes.len() >= 3 && bytes[1] == b':' {
        format!("{}:\\", (bytes[0] as char).to_ascii_uppercase())
    } else {
        return false;
    };
    let w = wide(&root);
    // SAFETY: valid root path.
    unsafe { GetDriveTypeW(w.as_ptr()) == DRIVE_REMOTE }
}
