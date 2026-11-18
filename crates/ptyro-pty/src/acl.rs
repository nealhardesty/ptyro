//! "Only its owner can read it" (WIN7): the mode bits, one question.
//!
//! [`readable_by_others`] says who besides the owner may read a file; [`make_private`] makes it
//! the owner's alone (`chmod 600`).

use std::io;
use std::path::Path;

/// Who besides its owner can read or write `path`: the mode (`644`) when group or others can.
/// `None` when it is private, or can't be told.
pub fn readable_by_others(path: &Path) -> Option<String> {
    let mode = ptyro_core::os::mode(&std::fs::metadata(path).ok()?)? & 0o777;
    (mode & 0o066 != 0).then(|| format!("{mode:03o}"))
}

/// Who besides its owner can change `path` (or, for a folder, what's in it): the mode when group
/// or others can write.
pub fn writable_by_others(path: &Path) -> Option<String> {
    let mode = ptyro_core::os::mode(&std::fs::metadata(path).ok()?)? & 0o777;
    (mode & 0o022 != 0).then(|| format!("{mode:03o}"))
}

/// Make `path` readable by its owner only: `chmod 600` (a folder: 700).
pub fn make_private(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        ptyro_core::os::private_dir(path)
    } else {
        ptyro_core::os::make_private(path)
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
