//! The current user's registry (`HKEY_CURRENT_USER`), for what makes ptyro an app Windows knows
//! (WIN10): its AppUserModelID (toasts that say "ptyro" with its icon) and its `ptyro:` link
//! handler (a toast's click). Only `HKCU`: nothing here needs an administrator, and nothing
//! reaches another user.
#![allow(unsafe_code)] // Win32 calls; each block says what it relies on.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegSetValueExW,
};

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

fn check(code: u32, what: &str) -> io::Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::new(
            io::Error::from_raw_os_error(code as i32).kind(),
            format!("{what}: {}", io::Error::from_raw_os_error(code as i32)),
        ))
    }
}

/// Set the string value `name` (`""`: the key's default value) of `HKCU\<key>`, making the key
/// (and any missing parents) first.
pub fn set_string(key: &str, name: &str, value: &str) -> io::Result<()> {
    let k = wide(key);
    let mut h: HKEY = std::ptr::null_mut();
    // SAFETY: `k` is NUL-ended; `h` is written on success and closed below.
    let r = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            k.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut h,
            std::ptr::null_mut(),
        )
    };
    check(r, &format!("HKCU\\{key}"))?;
    let n = wide(name);
    let v = wide(value);
    // SAFETY: `h` is open; the data is `v`'s bytes, its NUL included, as REG_SZ wants.
    let r = unsafe {
        RegSetValueExW(
            h,
            if name.is_empty() { std::ptr::null() } else { n.as_ptr() },
            0,
            REG_SZ,
            v.as_ptr().cast(),
            (v.len() * 2) as u32,
        )
    };
    // SAFETY: `h` came from RegCreateKeyExW above.
    unsafe { RegCloseKey(h) };
    check(r, &format!("HKCU\\{key}\\{name}"))
}

/// The string value `name` (`""`: the default value) of `HKCU\<key>`; `None` when there's none.
pub fn get_string(key: &str, name: &str) -> Option<String> {
    let k = wide(key);
    let n = wide(name);
    let name_ptr = if name.is_empty() { std::ptr::null() } else { n.as_ptr() };
    let mut size = 0u32;
    // SAFETY: asking for the size only (no buffer).
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            k.as_ptr(),
            name_ptr,
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if r != ERROR_SUCCESS || size == 0 {
        return None;
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    // SAFETY: `buf` holds `size` bytes, which is what the call was told.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            k.as_ptr(),
            name_ptr,
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// Remove `HKCU\<key>` and everything under it; nothing there is fine.
pub fn delete_tree(key: &str) -> io::Result<()> {
    let k = wide(key);
    // SAFETY: `k` is NUL-ended; the key is opened and closed by the call itself.
    let r = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, k.as_ptr()) };
    if r == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    check(r, &format!("HKCU\\{key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Written, read back (Unicode too), the default value included, and removed with
    /// everything under it, in a key of its own under ptyro's test root.
    #[test]
    fn values_are_written_read_and_removed() {
        let root = format!("Software\\ptyro-test-{}", std::process::id());
        let key = format!("{root}\\shell\\open\\command");
        set_string(&key, "", "\"C:\\Program Files\\ptyro\\ptyrow.exe\" open-url \"%1\"").unwrap();
        set_string(&root, "DisplayName", "ptyro 漢字").unwrap();
        assert_eq!(
            get_string(&key, "").as_deref(),
            Some("\"C:\\Program Files\\ptyro\\ptyrow.exe\" open-url \"%1\"")
        );
        assert_eq!(get_string(&root, "DisplayName").as_deref(), Some("ptyro 漢字"));
        assert_eq!(get_string(&root, "Missing"), None);
        delete_tree(&root).unwrap();
        assert_eq!(get_string(&key, ""), None, "gone, with what was under it");
        delete_tree(&root).unwrap();
    }
}
