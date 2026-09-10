//! Process hardening applied before anything else runs.

/// Restricts runtime DLL resolution to the Windows system directory.
///
/// Windows searches the executable's own folder before `System32` when loading DLLs, so a stray
/// DLL sitting next to the EXE — e.g. a 32-bit `opengl32.dll` left behind in the Downloads
/// folder — can be loaded in place of the real one and crash the app on launch with
/// `0xc000007b` ("invalid image"). Calling this first removes the application directory (and the
/// current directory) from the search path for later `LoadLibrary` calls. Statically imported
/// DLLs are additionally protected at link time via `/DEPENDENTLOADFLAG:0x800`.
#[cfg(windows)]
pub fn harden_dll_search() {
    use windows_sys::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories};
    // Best-effort: on the rare system without this API the loader keeps its default behaviour.
    unsafe {
        SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
}

#[cfg(not(windows))]
pub fn harden_dll_search() {}
