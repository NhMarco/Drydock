use thiserror::Error;

#[cfg(windows)]
pub fn ensure_elevated(arguments: &[String]) -> Result<bool, ElevationError> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::UI::Shell::{IsUserAnAdmin, ShellExecuteW};
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    if unsafe { IsUserAnAdmin() } != 0 {
        return Ok(true);
    }
    let executable = std::env::current_exe()?;
    let executable: Vec<u16> = executable.as_os_str().encode_wide().chain(Some(0)).collect();
    let verb: Vec<u16> = "runas\0".encode_utf16().collect();
    let parameters = arguments
        .iter()
        .map(|argument| quote_windows_argument(argument))
        .collect::<Vec<_>>()
        .join(" ");
    let parameters: Vec<u16> = parameters.encode_utf16().chain(Some(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            executable.as_ptr(),
            parameters.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if result <= 32 {
        return Err(ElevationError::Rejected(result));
    }
    Ok(false)
}

#[cfg(not(windows))]
pub fn ensure_elevated(_arguments: &[String]) -> Result<bool, ElevationError> {
    Ok(true)
}

#[cfg(windows)]
fn quote_windows_argument(value: &str) -> String {
    if !value.is_empty()
        && !value
            .chars()
            .any(|character| character.is_whitespace() || character == '"')
    {
        return value.to_owned();
    }
    let mut output = String::from("\"");
    let mut backslashes = 0;
    for character in value.chars() {
        if character == '\\' {
            backslashes += 1;
            continue;
        }
        if character == '"' {
            output.push_str(&"\\".repeat(backslashes * 2 + 1));
            output.push('"');
        } else {
            output.push_str(&"\\".repeat(backslashes));
            output.push(character);
        }
        backslashes = 0;
    }
    output.push_str(&"\\".repeat(backslashes * 2));
    output.push('"');
    output
}

#[derive(Debug, Error)]
pub enum ElevationError {
    #[error("Administrator access was not granted (ShellExecute code {0})")]
    Rejected(isize),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn windows_arguments_are_quoted_without_changing_content() {
        assert_eq!(quote_windows_argument("plain"), "plain");
        assert_eq!(quote_windows_argument("two words"), "\"two words\"");
        assert_eq!(
            quote_windows_argument("C:\\path with space\\"),
            "\"C:\\path with space\\\\\""
        );
        assert_eq!(quote_windows_argument("say\"hello"), "\"say\\\"hello\"");
    }
}
