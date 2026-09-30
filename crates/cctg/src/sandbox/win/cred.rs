//! Slot account passwords: generated, machine-scope DPAPI blobs in
//! `~/.cctg/sandbox/win/slot-<k>.cred` (the real user's profile ACL, plus a
//! DENY for the `cctg-sandbox` group that `install` adds to the directory).
//! The non-elevated install writes them; the elevated step reads them; a slot
//! account can never read its own (it is DENY'd on the directory).
//!
//! Password shape and the DPAPI calls by intent: `srtwin/src_user.rs`
//! (`gen_password`), `srtwin/src_dpapi.rs` (Apache-2.0). Adapted to
//! `aws_lc_rs::rand` and raw `windows-sys`.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_LOCAL_MACHINE, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
    CryptUnprotectData,
};

use super::util::ok;

/// A password held in memory, zeroed on drop.
pub struct Password(String);

impl Password {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A NUL-terminated UTF-16 copy for `CreateProcessWithLogonW`; the caller
    /// zeroes it after the call.
    pub fn to_wide(&self) -> Vec<u16> {
        self.0.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

impl Drop for Password {
    fn drop(&mut self) {
        // SAFETY: writing zeros over the bytes keeps the String valid UTF-8.
        unsafe { self.0.as_mut_vec() }.fill(0);
    }
}

/// The 85-symbol alphabet (srt): no `"`, `\`, space, `` ` `` or `&|<>^`, so the
/// password survives any quoting layer to `CreateProcessWithLogonW`.
const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ\
                       abcdefghijklmnopqrstuvwxyz\
                       0123456789!#$%()*+,-./:;=?@[]_{}~";
const LEN: usize = 32;

/// A fresh 32-character password drawn uniformly from [`ALPHA`] with the
/// system CSPRNG (rejection sampling; 85 does not divide 256). Each character
/// class is present (a tightened local policy needs at least one of each).
pub fn generate() -> anyhow::Result<Password> {
    const { assert!(ALPHA.len() == 85) };
    let bound = (u8::MAX - (u8::MAX % ALPHA.len() as u8)) as usize;
    let mut out = Vec::with_capacity(LEN);
    let mut buf = [0u8; 64];
    while out.len() < LEN {
        aws_lc_rs::rand::fill(&mut buf).map_err(|_| anyhow::anyhow!("rng"))?;
        for &b in &buf {
            if out.len() == LEN {
                break;
            }
            if (b as usize) < bound {
                out.push(ALPHA[b as usize % ALPHA.len()]);
            }
        }
    }
    // Force one of each class into fixed positions (rare; the draw above
    // almost always already has them).
    const CLASSES: [&[u8]; 4] = [
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        b"abcdefghijklmnopqrstuvwxyz",
        b"0123456789",
        b"!#$%()*+,-./:;=?@[]_{}~",
    ];
    if CLASSES.iter().any(|c| !out.iter().any(|b| c.contains(b))) {
        let mut extra = [0u8; 5];
        aws_lc_rs::rand::fill(&mut extra).map_err(|_| anyhow::anyhow!("rng"))?;
        let base = extra[0] as usize;
        for (k, class) in CLASSES.iter().enumerate() {
            out[(base + k) % LEN] = class[extra[1 + k] as usize % class.len()];
        }
    }
    Ok(Password(String::from_utf8(out).expect("ALPHA is ASCII")))
}

/// `slot-<k>.cred` in the win directory (before install commits, `.new`).
pub fn cred_file(win_dir: &Path, k: u32, pending: bool) -> PathBuf {
    let suffix = if pending { ".new" } else { "" };
    win_dir.join(format!("slot-{k}.cred{suffix}"))
}

/// Machine-scope DPAPI ciphertext of `data` (any local account on this
/// machine could decrypt it, so the file's ACL is the real protection).
pub fn protect(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: input points at `data`; out receives a LocalAlloc buffer.
    ok(
        unsafe {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_LOCAL_MACHINE | CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        },
        "CryptProtectData",
    )?;
    let blob = copy_and_free(&out);
    Ok(blob)
}

/// Reverse of [`protect`].
pub fn unprotect(blob: &[u8]) -> anyhow::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: blob.len() as u32,
        pbData: blob.as_ptr() as *mut u8,
    };
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: input points at `blob`; out receives a LocalAlloc buffer.
    ok(
        unsafe {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        },
        "CryptUnprotectData",
    )?;
    Ok(copy_and_free(&out))
}

/// Copy a DPAPI output blob into a `Vec` and `LocalFree` it.
fn copy_and_free(out: &CRYPT_INTEGER_BLOB) -> Vec<u8> {
    // SAFETY: `out.pbData` is a LocalAlloc buffer of `out.cbData` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    // SAFETY: the buffer came from Crypt*Data (LocalAlloc).
    unsafe {
        let _ = LocalFree(out.pbData as *mut c_void);
    }
    bytes
}

/// Reads and decrypts slot `k`'s committed credential.
pub fn load(win_dir: &Path, k: u32) -> anyhow::Result<Password> {
    load_from(&cred_file(win_dir, k, false), k)
}

/// Reads and decrypts slot `k`'s pending (`.new`) credential (elevated step).
pub fn load_pending(win_dir: &Path, k: u32) -> anyhow::Result<Password> {
    load_from(&cred_file(win_dir, k, true), k)
}

fn load_from(path: &Path, k: u32) -> anyhow::Result<Password> {
    let bytes = std::fs::read(path).map_err(|_| anyhow::anyhow!("read slot-{k}.cred"))?;
    let mut pw = unprotect(&bytes)?;
    let password = String::from_utf8(pw.clone()).map_err(|_| anyhow::anyhow!("password bytes"))?;
    pw.fill(0);
    Ok(Password(password))
}

/// Writes the DPAPI blob of `password` to the pending credential file.
pub fn write_pending(win_dir: &Path, k: u32, password: &Password) -> anyhow::Result<()> {
    let blob = protect(password.as_str().as_bytes())?;
    std::fs::write(cred_file(win_dir, k, true), &blob)
        .map_err(|_| anyhow::anyhow!("write slot-{k}.cred.new"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_shape() {
        let p = generate().expect("gen");
        assert_eq!(p.as_str().len(), LEN);
        assert!(p.as_str().is_ascii());
        for c in ['"', '\\', '`', ' ', '&', '|', '<', '>', '^'] {
            assert!(!p.as_str().contains(c), "{c}");
        }
        assert!(p.as_str().bytes().any(|b| b.is_ascii_uppercase()));
        assert!(p.as_str().bytes().any(|b| b.is_ascii_lowercase()));
        assert!(p.as_str().bytes().any(|b| b.is_ascii_digit()));
        assert!(p.as_str().bytes().any(|b| !b.is_ascii_alphanumeric()));
        assert_ne!(p.as_str(), generate().unwrap().as_str());
    }

    #[test]
    fn dpapi_round_trip() {
        let secret = b"cctg-sandbox slot password 0123456789";
        let blob = protect(secret).expect("protect");
        assert_ne!(blob.as_slice(), secret.as_slice());
        assert_eq!(unprotect(&blob).expect("unprotect"), secret);
    }
}
