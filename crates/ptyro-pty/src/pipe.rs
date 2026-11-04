//! The daemon's named pipe on Windows (WIN5; SPEC §17): what the Unix socket's mode and
//! `peer_cred` do, done with access lists and process owners.
//!
//! - Every instance the daemon creates admits only this user's SID (a DACL with nothing else in
//!   it) and refuses clients on other machines.
//! - Pipe names are one namespace for the whole computer, so another user could create ours
//!   first. A client therefore checks the process at the other end runs as itself before it says
//!   anything ([`server_is_ours`]), as a Unix client checks the socket's peer uid.
#![allow(unsafe_code)] // Win32 security calls; each block says what it relies on.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle;

use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

fn last_error(what: &str) -> io::Error {
    let e = io::Error::last_os_error();
    io::Error::new(e.kind(), format!("{what}: {e}"))
}

/// The string SID (`S-1-5-21-…`) of the user `process` runs as.
fn owner_sid(process: HANDLE) -> io::Result<String> {
    // SAFETY: the token is opened for query and closed below; the TOKEN_USER buffer is sized by
    // the first call and outlives the SID pointer read from it; the SID string is freed with
    // LocalFree, as ConvertSidToStringSidW requires.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return Err(last_error("OpenProcessToken"));
        }
        let mut len = 0u32;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
        CloseHandle(token);
        if ok == 0 {
            return Err(last_error("GetTokenInformation"));
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut wide: *mut u16 = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut wide) == 0 {
            return Err(last_error("ConvertSidToStringSid"));
        }
        let n = (0..).take_while(|&i| *wide.add(i) != 0).count();
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(wide, n));
        LocalFree(wide as HLOCAL);
        Ok(sid)
    }
}

/// This process's user, as a string SID.
pub fn user_sid() -> io::Result<String> {
    // SAFETY: the pseudo-handle for this process needs no closing.
    owner_sid(unsafe { GetCurrentProcess() })
}

/// An instance of the pipe `name` that only this user can open. `first`: fail if the name is
/// taken already (another daemon, or someone squatting it), as binding a socket in use does.
pub fn server(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    let sddl: Vec<u16> =
        format!("D:P(A;;GA;;;{})", user_sid()?).encode_utf16().chain(std::iter::once(0)).collect();
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: a NUL-ended SDDL string; on success `sd` is a self-relative descriptor that we free
    // with LocalFree once the pipe exists (CreateNamedPipe copies it).
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(last_error("ConvertStringSecurityDescriptorToSecurityDescriptor"));
        }
        let mut sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd,
            bInheritHandle: 0,
        };
        let pipe = ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(name, (&raw mut sa).cast::<c_void>());
        LocalFree(sd as HLOCAL);
        pipe
    }
}

/// The pid of the client connected to `server`.
pub fn client_pid(server: &NamedPipeServer) -> Option<u32> {
    let mut pid = 0u32;
    // SAFETY: a connected pipe handle we own; `pid` is written on success.
    let ok = unsafe { GetNamedPipeClientProcessId(server.as_raw_handle(), &mut pid) } != 0;
    (ok && pid != 0).then_some(pid)
}

/// Whether the process serving `client`'s pipe runs as this user.
pub fn server_is_ours(client: &NamedPipeClient) -> io::Result<bool> {
    handle_server_is_ours(client.as_raw_handle())
}

/// [`server_is_ours`] for a pipe opened another way (a `std::fs::File`, as the hook relay does).
pub fn file_server_is_ours(file: &std::fs::File) -> io::Result<bool> {
    handle_server_is_ours(file.as_raw_handle())
}

fn handle_server_is_ours(handle: std::os::windows::io::RawHandle) -> io::Result<bool> {
    let mut pid = 0u32;
    // SAFETY: a connected pipe handle we own; the server's process is opened for query only and
    // closed below.
    unsafe {
        if GetNamedPipeServerProcessId(handle, &mut pid) == 0 {
            return Err(last_error("GetNamedPipeServerProcessId"));
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Err(last_error("OpenProcess"));
        }
        let sid = owner_sid(process);
        CloseHandle(process);
        Ok(sid? == user_sid()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::windows::named_pipe::ClientOptions;

    #[test]
    fn this_user_has_a_sid() {
        assert!(user_sid().unwrap().starts_with("S-1-"));
    }

    #[tokio::test]
    async fn a_private_pipe_carries_bytes_and_knows_both_ends() {
        let name = format!(r"\\.\pipe\ptyro-test-{}", std::process::id());
        let server = server(&name, true).unwrap();
        // The name is ours now: a second "first" instance is refused.
        assert!(super::server(&name, true).is_err());
        let mut client = ClientOptions::new().open(&name).unwrap();
        server.connect().await.unwrap();
        assert!(server_is_ours(&client).unwrap());
        assert_eq!(client_pid(&server), Some(std::process::id()));
        let mut server = server;
        client.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        server.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
    }
}
