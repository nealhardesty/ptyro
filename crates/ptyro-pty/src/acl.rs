//! "Only its owner can read it" (WIN7): Unix's mode bits, Windows' access lists, one question.
//!
//! A file under the user's profile inherits an access list that admits the user, SYSTEM and
//! Administrators, which is private enough; one copied in from elsewhere, or under a folder
//! shared on purpose, may admit more. [`readable_by_others`] says who; [`make_private`] makes
//! the list exactly those three (on Unix: `chmod 600`).

use std::io;
use std::path::Path;

/// Who besides its owner can read or write `path`: the mode (`644`) on Unix when group or
/// others can; on Windows the names (`Everyone`, `BUILTIN\Users`) its access list lets read.
/// `None` when it is private, or can't be told.
pub fn readable_by_others(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        let mode = ptyro_core::os::mode(&std::fs::metadata(path).ok()?)? & 0o777;
        (mode & 0o066 != 0).then(|| format!("{mode:03o}"))
    }
    #[cfg(windows)]
    {
        let names = windows::readers(path).ok()?;
        (!names.is_empty()).then(|| names.join(", "))
    }
}

/// Who besides its owner can change `path` (or, for a folder, what's in it): the mode on Unix
/// when group or others can write; on Windows the names its access list lets write.
pub fn writable_by_others(path: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        let mode = ptyro_core::os::mode(&std::fs::metadata(path).ok()?)? & 0o777;
        (mode & 0o022 != 0).then(|| format!("{mode:03o}"))
    }
    #[cfg(windows)]
    {
        let names = windows::writers(path).ok()?;
        (!names.is_empty()).then(|| names.join(", "))
    }
}

/// Make `path` readable by its owner only: `chmod 600` on Unix (a folder: 700); on Windows an
/// access list of the user, SYSTEM and Administrators, no longer inherited (a folder's also
/// passed on to what's in it).
pub fn make_private(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        if path.is_dir() {
            ptyro_core::os::private_dir(path)
        } else {
            ptyro_core::os::make_private(path)
        }
    }
    #[cfg(windows)]
    {
        windows::make_private(path)
    }
}

#[cfg(windows)]
#[allow(unsafe_code)] // Win32 security calls; each block says what it relies on.
mod windows {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{HLOCAL, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        DACL_SECURITY_INFORMATION, GetAce, GetAclInformation, GetSecurityDescriptorDacl,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    };

    /// `ACCESS_ALLOWED_ACE_TYPE`.
    const ALLOWED: u8 = 0;
    /// What reading takes: the data, or a generic right that includes it.
    const READS: u32 = 0x0001 | 0x8000_0000 | 0x1000_0000; // FILE_READ_DATA, GENERIC_READ, GENERIC_ALL
    /// What changing takes: writing or appending data (in a folder: adding files or folders),
    /// deleting, changing the access list or the owner, or a generic right that includes one.
    const WRITES: u32 =
        0x0002 | 0x0004 | 0x0001_0000 | 0x0004_0000 | 0x0008_0000 | 0x4000_0000 | 0x1000_0000;

    fn wide(p: &Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }

    fn sid_string(sid: PSID) -> Option<String> {
        let mut w: *mut u16 = std::ptr::null_mut();
        // SAFETY: a valid SID from an ACE; the string is freed with LocalFree.
        unsafe {
            if ConvertSidToStringSidW(sid, &mut w) == 0 {
                return None;
            }
            let n = (0..).take_while(|&i| *w.add(i) != 0).count();
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(w, n));
            LocalFree(w as HLOCAL);
            Some(s)
        }
    }

    /// A SID as people know it: the well-known ones by name, the rest as their string.
    fn shown(sid: &str) -> String {
        match sid {
            "S-1-1-0" => "Everyone".into(),
            "S-1-5-11" => "Authenticated Users".into(),
            "S-1-5-32-545" => "Users".into(),
            "S-1-5-32-546" => "Guests".into(),
            "S-1-5-7" => "Anonymous".into(),
            other => other.to_owned(),
        }
    }

    /// Who besides this user, SYSTEM, Administrators and the owner's own rights may read it.
    pub fn readers(path: &Path) -> io::Result<Vec<String>> {
        others(path, READS)
    }

    /// The same, for changing it.
    pub fn writers(path: &Path) -> io::Result<Vec<String>> {
        others(path, WRITES)
    }

    fn others(path: &Path, rights: u32) -> io::Result<Vec<String>> {
        let me = crate::pipe::user_sid()?;
        let ok_sids = [me.as_str(), "S-1-5-18", "S-1-5-32-544", "S-1-3-4", "S-1-3-0"];
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: the descriptor comes back allocated (freed below), the DACL points into it;
        // each ACE is read through GetAce within the ACL's own count.
        unsafe {
            let w = wide(path);
            let r = GetNamedSecurityInfoW(
                w.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut sd,
            );
            if r != 0 {
                return Err(io::Error::from_raw_os_error(r as i32));
            }
            let out = if dacl.is_null() {
                // No list at all: everyone may do anything.
                vec!["Everyone".to_owned()]
            } else {
                let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
                GetAclInformation(
                    dacl,
                    (&raw mut info).cast(),
                    std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                );
                let mut names = Vec::new();
                for i in 0..info.AceCount {
                    let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
                    if GetAce(dacl, i, &mut ace) == 0 {
                        continue;
                    }
                    let header = &*(ace as *const ACE_HEADER);
                    if header.AceType != ALLOWED {
                        continue;
                    }
                    let allowed = &*(ace as *const ACCESS_ALLOWED_ACE);
                    if allowed.Mask & rights == 0 {
                        continue;
                    }
                    let sid = (&raw const allowed.SidStart) as PSID;
                    if let Some(s) = sid_string(sid)
                        && !ok_sids.contains(&s.as_str())
                        && !names.contains(&shown(&s))
                    {
                        names.push(shown(&s));
                    }
                }
                names
            };
            LocalFree(sd as HLOCAL);
            Ok(out)
        }
    }

    /// An access list of this user, SYSTEM and Administrators, full control, not inherited.
    pub fn make_private(path: &Path) -> io::Result<()> {
        let me = crate::pipe::user_sid()?;
        // A folder's entries are inherited by its files and folders (OI, CI).
        let inherit = if path.is_dir() { "OICI" } else { "" };
        let sddl: Vec<u16> =
            format!("D:P(A;{inherit};FA;;;{me})(A;{inherit};FA;;;SY)(A;{inherit};FA;;;BA)")
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
        // SAFETY: the descriptor made from the SDDL is freed below, after the DACL taken from it
        // is applied (SetNamedSecurityInfo copies it).
        unsafe {
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let (mut present, mut defaulted) = (0, 0);
            let mut dacl: *mut ACL = std::ptr::null_mut();
            let got = GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted);
            let r = if got == 0 {
                io::Error::last_os_error().raw_os_error().unwrap_or(1) as u32
            } else {
                let w = wide(path);
                SetNamedSecurityInfoW(
                    w.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    dacl,
                    std::ptr::null(),
                )
            };
            LocalFree(sd as HLOCAL);
            if r == 0 { Ok(()) } else { Err(io::Error::from_raw_os_error(r as i32)) }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_file_shared_with_everyone_is_said_so_and_made_private() {
            let dir = tempfile::tempdir().unwrap();
            let f = dir.path().join("config.toml");
            std::fs::write(&f, "x").unwrap();
            // Give Everyone read, as a file copied from a share might have.
            let me = crate::pipe::user_sid().unwrap();
            let sddl: Vec<u16> = format!("D:P(A;;FA;;;{me})(A;;FR;;;WD)")
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            unsafe {
                let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
                assert!(
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        sddl.as_ptr(),
                        SDDL_REVISION_1,
                        &mut sd,
                        std::ptr::null_mut()
                    ) != 0
                );
                let (mut p, mut d) = (0, 0);
                let mut dacl: *mut ACL = std::ptr::null_mut();
                GetSecurityDescriptorDacl(sd, &mut p, &mut dacl, &mut d);
                let w = wide(&f);
                assert_eq!(
                    SetNamedSecurityInfoW(
                        w.as_ptr(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        dacl,
                        std::ptr::null()
                    ),
                    0
                );
                LocalFree(sd as HLOCAL);
            }
            assert_eq!(super::super::readable_by_others(&f).as_deref(), Some("Everyone"));
            make_private(&f).unwrap();
            assert_eq!(super::super::readable_by_others(&f), None);
            assert_eq!(std::fs::read_to_string(&f).unwrap(), "x", "still ours to read");
            // A folder anyone may write to: said so, then private, with what's inside reachable.
            let d = dir.path().join("shell");
            std::fs::create_dir(&d).unwrap();
            let sddl: Vec<u16> = format!("D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;WD)")
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            unsafe {
                let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
                assert!(
                    ConvertStringSecurityDescriptorToSecurityDescriptorW(
                        sddl.as_ptr(),
                        SDDL_REVISION_1,
                        &mut sd,
                        std::ptr::null_mut()
                    ) != 0
                );
                let (mut p, mut dd) = (0, 0);
                let mut dacl: *mut ACL = std::ptr::null_mut();
                GetSecurityDescriptorDacl(sd, &mut p, &mut dacl, &mut dd);
                let w = wide(&d);
                assert_eq!(
                    SetNamedSecurityInfoW(
                        w.as_ptr(),
                        SE_FILE_OBJECT,
                        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        dacl,
                        std::ptr::null()
                    ),
                    0
                );
                LocalFree(sd as HLOCAL);
            }
            assert_eq!(super::super::writable_by_others(&d).as_deref(), Some("Everyone"));
            make_private(&d).unwrap();
            assert_eq!(super::super::writable_by_others(&d), None);
            std::fs::write(d.join("bash-init"), "x").unwrap();
            assert_eq!(super::super::readable_by_others(&d.join("bash-init")), None, "inherited");
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn a_file_others_can_read_says_its_mode_and_is_made_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("c.toml");
        std::fs::write(&f, "x").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(super::readable_by_others(&f).as_deref(), Some("644"));
        super::make_private(&f).unwrap();
        assert_eq!(super::readable_by_others(&f), None);
        // A folder others can write to, made private: 700.
        let d = dir.path().join("shared");
        std::fs::create_dir(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(super::writable_by_others(&d).as_deref(), Some("777"));
        super::make_private(&d).unwrap();
        assert_eq!(super::writable_by_others(&d), None);
        assert_eq!(std::fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
    }
}
