//! Platform URL launching copied from Nova b69a1e5765ecc65ac3b00acdfe2f16e2f46a5dda.
//! Desktop bodies match crates/player/src/lib.rs; Android uses Reader's JNI
//! version/context adapter for the same VIEW/BROWSABLE/NEW_TASK intent.

/// Open `url` in the desktop's default browser, independently of video
/// handoff. Used for links such as an addon's configuration page.
#[cfg(all(not(target_os = "android"), target_os = "windows"))]
pub fn open_browser(url: &str) -> Result<(), String> {
    shell_open_url(url)
}

#[cfg(all(not(target_os = "android"), not(target_os = "windows")))]
pub fn open_browser(url: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not launch xdg-open: {e}"))
}

#[cfg(target_os = "windows")]
fn shell_open_url(url: &str) -> Result<(), String> {
    use std::ffi::c_void;

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show_command: i32,
        ) -> *mut c_void;
    }

    let operation = [b'o' as u16, b'p' as u16, b'e' as u16, b'n' as u16, 0];
    let file: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    let code = result as isize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!(
            "could not open URL with the Windows shell (code {code})"
        ))
    }
}

#[cfg(target_os = "android")]
pub fn open_browser(url: &str) -> Result<(), String> {
    crate::android_documents::open_browser(url)
}
