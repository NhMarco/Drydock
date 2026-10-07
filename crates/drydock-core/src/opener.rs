//! Hands a link or URI (`https:`, `steam:`) to whatever this system opens it with.
//!
//! On Windows this asks the shell directly (`ShellExecuteW`), the way Explorer opens a link. The
//! `open` crate goes through a hidden PowerShell there and gives up as soon as that fails, which it
//! does for some link handlers (browser pickers set as the default browser, for one) and wherever
//! security software stops a program from starting PowerShell. Elsewhere `open` does the work.

use std::io;

/// Opens `uri` with the program the system has for it. The caller decides which schemes it allows.
#[cfg(windows)]
pub(crate) fn open_uri(uri: &str) -> io::Result<()> {
    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let uri: Vec<u16> = uri.encode_utf16().chain(Some(0)).collect();
    // A link handler may be a COM component (packaged apps register theirs that way), so the shell
    // wants COM set up, single-threaded, on the calling thread. A thread of its own leaves the
    // caller's COM state alone: the UI thread already has OLE for drag and drop.
    let code = std::thread::spawn(move || {
        let initialized = unsafe {
            CoInitializeEx(
                std::ptr::null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            )
        };
        // No verb: the handler's default action, which is "open" for a link.
        let code = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                std::ptr::null(),
                uri.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        } as isize;
        if initialized >= 0 {
            unsafe { CoUninitialize() };
        }
        code
    })
    .join()
    .map_err(|_| io::Error::other("the link could not be handed to Windows"))?;
    if code > 32 {
        Ok(())
    } else {
        Err(shell_execute_error(code))
    }
}

#[cfg(not(windows))]
pub(crate) fn open_uri(uri: &str) -> io::Result<()> {
    open::that(uri)
}

/// What a `ShellExecuteW` result of 32 or less means (its documented error values).
#[cfg_attr(not(windows), allow(dead_code))]
fn shell_execute_error(code: isize) -> io::Error {
    match code {
        0 | 8 => io::Error::new(io::ErrorKind::OutOfMemory, "Windows ran out of memory"),
        31 => io::Error::new(
            io::ErrorKind::NotFound,
            "no program is set up in Windows to open this kind of link",
        ),
        26..=30 | 32 => io::Error::other(format!(
            "the program Windows opens these links with did not start (error {code})"
        )),
        // The rest are Win32 error codes (file not found, access denied, …).
        code => io::Error::from_raw_os_error(i32::try_from(code).unwrap_or(0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_errors_read_as_reasons_not_numbers() {
        assert_eq!(shell_execute_error(31).kind(), io::ErrorKind::NotFound);
        assert!(shell_execute_error(31).to_string().contains("no program"));
        assert!(shell_execute_error(28).to_string().contains("did not start"));
        assert_eq!(shell_execute_error(0).kind(), io::ErrorKind::OutOfMemory);
        assert_eq!(shell_execute_error(5).raw_os_error(), Some(5));
    }
}
