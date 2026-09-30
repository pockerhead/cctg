//! `cctg sandbox-runner`: the inside-the-logon half of the two-hop launch. The
//! broker `CreateProcessWithLogonW`s this under the slot account; it reads a
//! spec from stdin (4-byte LE length + JSON), self-protects, builds the
//! restricted token, and spawns Git Bash suspended under a job, a mitigation
//! policy and an explicit handle whitelist, then resumes and waits.
//!
//! Form of `srtwin/src_launch.rs` and `src_runner.rs` (Apache-2.0), adapted to
//! raw `windows-sys`. Only reachable when the broker launched it under the
//! slot account; validated by the CI e2e and the user's live check.

use std::ffi::c_void;

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_KERNEL_OBJECT,
    SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR,
};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_USER, TokenUser};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    CreateProcessAsUserW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
    GetCurrentProcess, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY, PROCESS_INFORMATION, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

use super::job::Job;
use super::token::{make_sandbox_token, open_self_token};
use super::util::{OwnedHandle, from_wide, ok, same_sid, sid_bytes, wide};

/// Mitigation-policy bits (winnt.h slots) that do not break Node/Python/mingw.
const MITIGATION: u64 = (1u64 << 32) | (1u64 << 48) | (1u64 << 52) | (1u64 << 56);
const HANDLE_FLAG_INHERIT: u32 = 1;

/// What the broker asks the runner to launch.
#[derive(Debug, Serialize, Deserialize)]
pub struct RunnerSpec {
    /// `argv[0]` = the program (Git Bash); `argv[1..]` its arguments.
    pub argv: Vec<String>,
    /// Overlaid on the slot profile env (overlay wins, case-insensitive).
    pub env_overlay: Vec<(String, String)>,
    /// The slot SID the runner's own token must carry (fail closed otherwise).
    pub slot_sid: String,
}

/// `<u32 LE len><JSON>` for stdin.
pub fn encode_spec(spec: &RunnerSpec) -> anyhow::Result<Vec<u8>> {
    let json = serde_json::to_vec(spec)?;
    let mut out = Vec::with_capacity(4 + json.len());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&json);
    Ok(out)
}

/// Reads exactly the spec from raw stdin (4-byte length + that many bytes), so
/// nothing after it is consumed (the child inherits the rest of stdin).
fn read_spec() -> anyhow::Result<RunnerSpec> {
    // SAFETY: standard input handle.
    let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let mut len_buf = [0u8; 4];
    read_exact(stdin, &mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > 4 * 1024 * 1024 {
        anyhow::bail!("spec too large");
    }
    let mut buf = vec![0u8; len];
    read_exact(stdin, &mut buf)?;
    Ok(serde_json::from_slice(&buf)?)
}

fn read_exact(h: HANDLE, buf: &mut [u8]) -> anyhow::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::ReadFile;
    let mut off = 0;
    while off < buf.len() {
        let mut read = 0u32;
        // SAFETY: buf[off..] is valid for the requested length.
        let r = unsafe {
            ReadFile(
                h,
                buf[off..].as_mut_ptr(),
                (buf.len() - off) as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if r == 0 || read == 0 {
            anyhow::bail!("stdin read");
        }
        off += read as usize;
    }
    Ok(())
}

/// The runner entry point.
pub fn run() -> i32 {
    match run_inner() {
        Ok(code) => code,
        Err(_) => {
            eprintln!("cctg sandbox: the sandboxed command could not start");
            super::BROKER_FAILED
        }
    }
}

fn run_inner() -> anyhow::Result<i32> {
    self_protect()?;
    let spec = read_spec()?;
    if spec.argv.is_empty() {
        anyhow::bail!("empty argv");
    }
    // The runner's own token must be the expected slot account.
    let self_token = open_self_token()?;
    let user = token_user_sid(self_token.raw())?;
    let want = sid_bytes(&spec.slot_sid)?;
    if !same_sid(&user, &want) {
        anyhow::bail!("wrong slot token");
    }
    // Not on the interactive desktop (fail closed).
    if super::desktop::on_default_desktop() {
        anyhow::bail!("on Default desktop");
    }
    let primary = make_sandbox_token(self_token.raw())?;
    // The child's environment is built from the slot user's own profile
    // (CreateEnvironmentBlock, no inheritance) plus the broker's whitelist
    // overlay. CreateProcessWithLogonW gives the runner the BROKER's
    // environment (NULL lpEnvironment inherits the caller's), so the runner's
    // own env is NOT used for the child — otherwise the broker's CCTG_* and
    // tokens would leak in. A denylist scrub is a second guard.
    let env = build_env_block(&spec.env_overlay, self_token.raw());
    let cmdline = build_cmdline(&spec.argv);
    let mut cmdline_w = wide(&cmdline);
    let app_w = wide(&spec.argv[0]);

    // Handle list + mitigation policy.
    let std_handles = inheritable_std_handles();
    let handles: Vec<HANDLE> = std_handles
        .iter()
        .copied()
        .filter(|h| !h.is_null())
        .collect();
    if handles.is_empty() {
        anyhow::bail!("no inheritable stdio");
    }
    let mut attrs = ProcThreadAttrs::new(2)?;
    let mitigation = MITIGATION;
    attrs.set(
        PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY as usize,
        &mitigation as *const u64 as *const c_void,
        std::mem::size_of::<u64>(),
    )?;
    attrs.set(
        PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
        handles.as_ptr() as *const c_void,
        std::mem::size_of_val(handles.as_slice()),
    )?;

    let mut six: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    six.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    six.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    six.StartupInfo.hStdInput = std_handles[0];
    six.StartupInfo.hStdOutput = std_handles[1];
    six.StartupInfo.hStdError = std_handles[2];
    six.lpAttributeList = attrs.list();

    let job = Job::new(false)?;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: primary is a valid token; app/cmdline valid; env is a double-NUL
    // UTF-16 block; six is a valid STARTUPINFOEXW with an attribute list.
    ok(
        unsafe {
            CreateProcessAsUserW(
                primary.raw(),
                app_w.as_ptr(),
                cmdline_w.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1, // must be TRUE for the handle-list attribute
                CREATE_SUSPENDED
                    | CREATE_UNICODE_ENVIRONMENT
                    | EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_NO_WINDOW
                    | CREATE_BREAKAWAY_FROM_JOB,
                env.as_ptr() as *const c_void,
                std::ptr::null(),
                &six.StartupInfo as *const STARTUPINFOW,
                &mut pi,
            )
        },
        "CreateProcessAsUserW",
    )?;
    let child = OwnedHandle(pi.hProcess);
    let thread = OwnedHandle(pi.hThread);
    // Assign then resume. If assign fails, terminate the suspended child.
    if let Err(error) = job.assign(child.raw()) {
        // SAFETY: child is valid.
        unsafe {
            let _ = windows_sys::Win32::System::Threading::TerminateProcess(child.raw(), 1);
        }
        return Err(error);
    }
    // SAFETY: thread is valid.
    if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
        anyhow::bail!("ResumeThread");
    }
    // SAFETY: child is valid.
    let rc = unsafe { WaitForSingleObject(child.raw(), INFINITE) };
    if rc != WAIT_OBJECT_0 {
        anyhow::bail!("WaitForSingleObject");
    }
    let mut code: u32 = 1;
    // SAFETY: child is valid.
    ok(
        unsafe { GetExitCodeProcess(child.raw(), &mut code) },
        "GetExitCodeProcess",
    )?;
    drop(attrs);
    Ok(code as i32)
}

/// The SID (bytes) of a token's user.
fn token_user_sid(token: HANDLE) -> anyhow::Result<Vec<u8>> {
    let mut len = 0u32;
    // SAFETY: sizing call.
    unsafe {
        let _ = GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
    }
    if len == 0 {
        anyhow::bail!("TokenUser sizing 0");
    }
    let mut buf = vec![0u8; len as usize];
    // SAFETY: buf is `len` bytes.
    ok(
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buf.as_mut_ptr() as *mut c_void,
                len,
                &mut len,
            )
        },
        "GetTokenInformation(TokenUser)",
    )?;
    // SAFETY: buf holds a TOKEN_USER whose User.Sid points into it.
    let sid = unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid };
    // SAFETY: sid is valid within buf.
    let sid_len = unsafe { windows_sys::Win32::Security::GetLengthSid(sid) } as usize;
    Ok(unsafe { std::slice::from_raw_parts(sid as *const u8, sid_len).to_vec() })
}

/// Rewrites this process's DACL so the child (same slot user) cannot open it:
/// `[SYSTEM, Admins, OwnerRights:READ_CONTROL]`, PROTECTED, no slot ACE.
fn self_protect() -> anyhow::Result<()> {
    let sddl = wide("D:P(A;;0x1fffff;;;SY)(A;;0x1fffff;;;BA)(A;;RC;;;S-1-3-4)");
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
    // SAFETY: psd is a valid SD.
    ok(
        unsafe { GetSecurityDescriptorDacl(psd, &mut present, &mut dacl, &mut defaulted) },
        "GetSecurityDescriptorDacl",
    )?;
    // SAFETY: current-process pseudo-handle; dacl valid.
    super::util::win_ok(
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
        "SetSecurityInfo(self DACL)",
    )
}

/// This process's std handles, marked inheritable, as `[in, out, err]`.
fn inheritable_std_handles() -> [HANDLE; 3] {
    use windows_sys::Win32::Foundation::SetHandleInformation;
    let mut out = [std::ptr::null_mut(); 3];
    for (i, which) in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
        .into_iter()
        .enumerate()
    {
        // SAFETY: standard handle query.
        let h = unsafe { GetStdHandle(which) };
        if h.is_null() || (h as isize) == -1 {
            continue;
        }
        // SAFETY: h is a valid handle.
        if unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } != 0 {
            out[i] = h;
        }
    }
    out
}

/// The command line for `CreateProcessAsUserW`: MSVCRT quoting (our target is
/// `bash.exe`, never `cmd.exe`).
fn build_cmdline(argv: &[String]) -> String {
    let mut s = quote_arg(&argv[0]);
    for a in &argv[1..] {
        s.push(' ');
        s.push_str(&quote_arg(a));
    }
    s
}

/// MSVCRT / `CommandLineToArgvW` quoting for one argument.
fn quote_arg(a: &str) -> String {
    if !a.is_empty() && !a.chars().any(|c| matches!(c, ' ' | '\t' | '"' | '\\')) {
        return a.to_string();
    }
    let mut out = String::with_capacity(a.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in a.chars() {
        match c {
            '\\' => {
                backslashes += 1;
                out.push('\\');
            }
            '"' => {
                for _ in 0..=backslashes {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                backslashes = 0;
                out.push(c);
            }
        }
    }
    for _ in 0..backslashes {
        out.push('\\');
    }
    out.push('"');
    out
}

/// A UTF-16 environment block for the child: the slot user's profile
/// environment (from `token`, no inheritance) with the broker's `overlay`
/// applied on top (overlay wins case-insensitively). A name the overlay
/// provides, or a sensitive one ([`sensitive`]), never survives from the base.
fn build_env_block(overlay: &[(String, String)], token: HANDLE) -> Vec<u16> {
    let mut entries = env_from_token(token);
    let overlay_keys: std::collections::HashSet<String> = overlay
        .iter()
        .map(|(k, _)| k.to_ascii_uppercase())
        .collect();
    entries.retain(|(k, _)| {
        let up = k.to_ascii_uppercase();
        !overlay_keys.contains(&up) && !sensitive(&up)
    });
    for (k, v) in overlay {
        entries.push((k.clone(), v.clone()));
    }
    entries.sort_by_key(|(k, _)| k.to_ascii_uppercase());
    let mut out: Vec<u16> = Vec::new();
    for (k, v) in entries {
        out.extend(k.encode_utf16());
        out.push(b'=' as u16);
        out.extend(v.encode_utf16());
        out.push(0);
    }
    out.push(0);
    out
}

/// A variable name (uppercased) that must never reach a sandboxed command: any
/// `CCTG_*` / `CLAUDE*` / `ANTHROPIC*`, a known token/secret name, or a
/// `*_TOKEN` / `*_SECRET` name. `CCTG_SANDBOX` and `CCTG_SANDBOX_COMMAND` are
/// re-added from the trusted overlay after this filter.
fn sensitive(up: &str) -> bool {
    up.starts_with("CCTG_")
        || up.starts_with("CLAUDE")
        || up.starts_with("ANTHROPIC")
        || up.ends_with("_TOKEN")
        || up.ends_with("_SECRET")
        || matches!(
            up,
            "GH_TOKEN" | "GITHUB_TOKEN" | "SSH_AUTH_SOCK" | "NPM_TOKEN" | "OPENAI_API_KEY"
        )
}

/// The profile environment of `token`'s user as `(name, value)` pairs,
/// via `CreateEnvironmentBlock` with NO inheritance (so the runner's own
/// inherited env is not used). Empty on failure (fail closed: the child then
/// gets only the overlay).
fn env_from_token(token: HANDLE) -> Vec<(String, String)> {
    use windows_sys::Win32::System::Environment::{
        CreateEnvironmentBlock, DestroyEnvironmentBlock,
    };
    let mut block: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: `token` is a valid token; binherit = FALSE (0).
    if unsafe { CreateEnvironmentBlock(&mut block, token, 0) } == 0 || block.is_null() {
        return Vec::new();
    }
    let pairs = parse_env_block(block as *const u16);
    // SAFETY: `block` came from CreateEnvironmentBlock.
    unsafe {
        let _ = DestroyEnvironmentBlock(block);
    }
    pairs
}

/// A double-NUL-terminated UTF-16 environment block as `(name, value)` pairs.
fn parse_env_block(ptr: *const u16) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    // SAFETY: `ptr` is a double-NUL-terminated UTF-16 environment block.
    unsafe {
        let mut p = ptr;
        while *p != 0 {
            let entry = from_wide(p);
            let mut len = 0;
            while *p.add(len) != 0 {
                len += 1;
            }
            p = p.add(len + 1);
            // `=C:=...` drive entries start with '='; keep only KEY=VALUE.
            if let Some(eq) = entry[1..].find('=') {
                let eq = eq + 1;
                pairs.push((entry[..eq].to_owned(), entry[eq + 1..].to_owned()));
            }
        }
    }
    pairs
}

/// RAII over a `PROC_THREAD_ATTRIBUTE_LIST`.
struct ProcThreadAttrs {
    storage: Vec<u8>,
}

impl ProcThreadAttrs {
    fn new(count: u32) -> anyhow::Result<Self> {
        let mut size = 0usize;
        // SAFETY: sizing call.
        unsafe {
            let _ = InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut size);
        }
        if size == 0 {
            anyhow::bail!("attr list size 0");
        }
        let mut storage = vec![0u8; size];
        // SAFETY: storage is `size` bytes.
        ok(
            unsafe {
                InitializeProcThreadAttributeList(
                    storage.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST,
                    count,
                    0,
                    &mut size,
                )
            },
            "InitializeProcThreadAttributeList",
        )?;
        Ok(Self { storage })
    }

    fn list(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST
    }

    fn set(&mut self, attribute: usize, value: *const c_void, size: usize) -> anyhow::Result<()> {
        // SAFETY: list is initialized; value is valid for `size` bytes and
        // outlives the CreateProcess call.
        ok(
            unsafe {
                UpdateProcThreadAttribute(
                    self.list(),
                    0,
                    attribute,
                    value,
                    size,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            },
            "UpdateProcThreadAttribute",
        )
    }
}

impl Drop for ProcThreadAttrs {
    fn drop(&mut self) {
        // SAFETY: list was initialized.
        unsafe {
            DeleteProcThreadAttributeList(self.storage.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_round_trips() {
        let spec = RunnerSpec {
            argv: vec!["bash.exe".into(), "-c".into()],
            env_overlay: vec![("PATH".into(), "C:/x".into())],
            slot_sid: "S-1-5-21-1-2-3-1001".into(),
        };
        let bytes = encode_spec(&spec).unwrap();
        assert_eq!(
            u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize,
            bytes.len() - 4
        );
        let back: RunnerSpec = serde_json::from_slice(&bytes[4..]).unwrap();
        assert_eq!(back.argv, spec.argv);
        assert_eq!(back.slot_sid, spec.slot_sid);
    }

    #[test]
    fn quoting_matches_msvcrt() {
        assert_eq!(quote_arg("foo"), "foo");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg(r#"a\"b"#), r#""a\\\"b""#);
        assert_eq!(quote_arg(r"a\"), r#""a\\""#);
    }

    #[test]
    fn the_child_env_carries_the_overlay_and_drops_sensitive_base_vars() {
        // Build against the test process's own token; the overlay is applied
        // and the slot profile base never keeps a secret.
        let token = open_self_token().expect("self token");
        let block = build_env_block(
            &[
                ("CCTG_SANDBOX".into(), "1".into()),
                ("PATH".into(), "C:/x".into()),
            ],
            token.raw(),
        );
        let text = String::from_utf16_lossy(&block);
        assert!(text.contains("CCTG_SANDBOX=1"), "overlay is present");
        assert!(text.contains("PATH=C:/x"), "overlay PATH wins");
        // No secret from the base survives (defence in depth beyond the clean
        // CreateEnvironmentBlock base).
        for leak in ["CCTG_HUB_SECRET", "GH_TOKEN", "ANTHROPIC_API_KEY"] {
            assert!(!text.contains(&format!("{leak}=")), "{leak} leaked: {text}");
        }
    }

    #[test]
    fn sensitive_names_are_scrubbed() {
        for yes in [
            "CCTG_HUB_SECRET",
            "CCTG_JOIN_CODE",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "SSH_AUTH_SOCK",
            "SOME_TOKEN",
            "APP_SECRET",
        ] {
            assert!(sensitive(yes), "{yes}");
        }
        for no in [
            "PATH",
            "HOME",
            "APPDATA",
            "SYSTEMROOT",
            "CARGO_HOME",
            "GIT_CONFIG_GLOBAL",
        ] {
            assert!(!sensitive(no), "{no}");
        }
    }
}
