//! Windows DPAPI protection for the host identity secrets.
//!
//! The M0 freeze (remote-control-m0-freeze.md § 1) requires host private
//! keys to live in an OS credential store, not in a world-readable file.
//! On Windows that store is DPAPI: `CryptProtectData` seals a blob to the
//! current user, and only the same user on the same machine can open it.
//!
//! Failure modes this module refuses (see identity.rs for the migration
//! order they protect):
//!
//! 1. A sealed blob that fails to open must be an error, never a reason to
//!    generate a fresh identity — a silent new key would silently end every
//!    paired device's trust.
//! 2. A seal failure must leave the caller's plaintext secret untouched.

use anyhow::bail;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{HLOCAL, LocalFree};
#[cfg(windows)]
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};

/// Seal `secret` to the current user. The blob is opaque: it carries no
/// key material of its own and is useless on another machine or user.
#[cfg(windows)]
pub fn protect(secret: &[u8]) -> anyhow::Result<Vec<u8>> {
    // The input blob borrows the caller's bytes; the output blob is owned
    // by crypt32 and released with LocalFree after the copy.
    let input = CRYPT_INTEGER_BLOB {
        cbData: secret.len() as u32,
        pbData: secret.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // CRYPTPROTECT_UI_FORBIDDEN: this runs inside a GUI app and inside the
    // host daemon; a prompt would hang both. No prompt struct, no entropy,
    // no description — the blob is scoped to this user on this machine.
    let sealed = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if sealed == 0 {
        bail!(
            "CryptProtectData failed: {}",
            std::io::Error::last_os_error()
        );
    }
    let blob = take_blob(&mut output);
    if blob.is_empty() {
        bail!("CryptProtectData returned an empty blob");
    }
    Ok(blob)
}

/// Open a blob this module sealed. A failure means the blob belongs to
/// another user or machine, or it was tampered with; the caller must treat
/// the identity as unreadable, never as absent.
#[cfg(windows)]
pub fn unprotect(blob: &[u8]) -> anyhow::Result<Vec<u8>> {
    let input = CRYPT_INTEGER_BLOB {
        cbData: blob.len() as u32,
        pbData: blob.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let opened = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if opened == 0 {
        bail!(
            "CryptUnprotectData failed: {}",
            std::io::Error::last_os_error()
        );
    }
    let secret = take_blob(&mut output);
    if secret.is_empty() {
        bail!("CryptUnprotectData returned an empty secret");
    }
    Ok(secret)
}

/// Copy a crypt32-owned blob out and release the original. The pointer is
/// always freed: a leak here would grow with every migration.
#[cfg(windows)]
fn take_blob(blob: &mut CRYPT_INTEGER_BLOB) -> Vec<u8> {
    let bytes = if blob.pbData.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize).to_vec() }
    };
    if !blob.pbData.is_null() {
        unsafe { LocalFree(blob.pbData as HLOCAL) };
    }
    blob.pbData = std::ptr::null_mut();
    blob.cbData = 0;
    bytes
}

/// Non-Windows callers never reach the sealed path: the caller falls back
/// to plaintext files with 0600 permissions (see identity.rs), exactly as
/// before this module existed.
#[cfg(not(windows))]
pub fn protect(_secret: &[u8]) -> anyhow::Result<Vec<u8>> {
    bail!("encrypted identity storage is only available on Windows")
}

#[cfg(not(windows))]
pub fn unprotect(_blob: &[u8]) -> anyhow::Result<Vec<u8>> {
    bail!("encrypted identity storage is only available on Windows")
}
