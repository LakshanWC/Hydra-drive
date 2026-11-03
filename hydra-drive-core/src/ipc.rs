//! hydra-drive-core/src/ipc.rs
//!
//! Add `pub mod ipc;` to lib.rs.
//! windows-sys features needed: Win32_Foundation, Win32_Security,
//! Win32_System_Threading, Win32_System_Memory, Win32_UI_Shell,
//! Win32_UI_WindowsAndMessaging.
#![allow(unused_variables, dead_code)]
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::{env, io, mem};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetExitCodeProcess, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::fill_root;

// =====================================================================
// Shared names and codes (used by BOTH main app and helper)
// =====================================================================

pub const MOUNT_POINT: &str = "V:";
pub const HELPER_EXE_NAME: &str = "hydra-virtual-drive-helper.exe";
pub const POOL_FOLDER_NAME: &str = "Virtual_Drive_Pool";

// Named Windows objects. Both sides must spell these identically.
pub const STOP_EVENT_NAME: &str = "Local\\HydraVD_Stop";
pub const READY_EVENT_NAME: &str = "Local\\HydraVD_Ready";
pub const HELPER_MUTEX_NAME: &str = "Local\\HydraVD_HelperRunning";
pub const SHARED_STATE_NAME: &str = "Local\\HydraVD_SharedState";

// Launch arguments (main -> helper, fixed at start)
pub const ARG_SIZE: &str = "--size";
pub const ARG_ROOT: &str = "--root";
pub const ARG_MOUNT: &str = "--mount";
pub const ARG_PARENT_PID: &str = "--parent-pid";

// Helper exit codes (helper -> main, tells main WHY it ended)
pub const EXIT_OK: u32 = 0;
pub const EXIT_BAD_ARGS: u32 = 2;
pub const EXIT_WINFSP_MISSING: u32 = 3;
pub const EXIT_MOUNT_FAILED: u32 = 4;
pub const EXIT_ALREADY_RUNNING: u32 = 5;
pub const EXIT_IPC_FAILED: u32 = 6;
pub const EXIT_UAC_DECLINED: u32 = 1223; // ERROR_CANCELLED, reported by main itself

// =====================================================================
// Types
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProgramStatus {
    Disabled,   // user turned "support administrator installers" off
    Stopped,    // not running (never started, or exited normally)
    Starting,   // launched, waiting for UAC / mount
    Running,    // mounted and ready
    Failed(u32) // exit code, see describe_exit_code
}

/// Values main writes and the helper reads (lives in shared memory).
/// Atomics, so no lock is needed. The helper must clamp what it reads,
/// since any normal process could write to this mapping.
#[repr(C)]
pub struct SharedState {
    pub used_bytes: AtomicU64,
    pub total_bytes: AtomicU64,
}

/// Handles for the named objects. Main creates them, the helper opens them.
pub struct IpcObjects {
    pub stop_event: HANDLE,
    pub ready_event: HANDLE,
    pub shared_state: *mut SharedState,
    // TODO: Drop impl closing the handles and unmapping the view
}

/// Main app's handle to the launched (elevated) helper process.
pub struct HelperProcess {
    pub process: HANDLE, // from ShellExecuteExW + SEE_MASK_NOCLOSEPROCESS
}

impl Drop for HelperProcess {
    fn drop(&mut self) {
        if (self.process as usize) != 0 {
            unsafe {
                CloseHandle(self.process);
            }
        }
    }
}

/// Validated launch arguments, as parsed by the helper.
#[derive(Debug)]
pub struct HelperArgs {
    pub total_size: u64,
    pub root: PathBuf,
    pub mount_point: String,
    pub parent_pid: u32,
}

// =====================================================================
// SHARED: both sides call these
// =====================================================================

/// Main side: create the stop event, ready event and shared-state mapping.
pub fn create_ipc_objects() -> io::Result<IpcObjects> {
    todo!()
}

/// Helper side: open the objects main created (minimum rights only).
pub fn open_ipc_objects() -> io::Result<IpcObjects> {
    todo!()
}

pub fn signal_event(event: HANDLE) {
    todo!()
}

pub fn reset_event(event: HANDLE) {
    todo!()
}

pub fn is_event_set(event: HANDLE) -> bool {
    todo!()
}

/// Turn an exit code into text for the UI ("UAC prompt declined", ...).
pub fn describe_exit_code(code: u32) -> &'static str {
    todo!()
}

/// Is a process with this id still alive?
pub fn is_process_alive(pid: u32) -> bool {
    todo!()
}

// Shared values. Main writes, helper's get_volume_info reads.
pub fn set_used_bytes(ipc: &IpcObjects, used: u64) {
    todo!()
}

pub fn set_total_bytes(ipc: &IpcObjects, total: u64) {
    todo!()
}

/// Helper side: returns used.min(total), because the mapping is untrusted.
pub fn get_used_bytes(ipc: &IpcObjects) -> u64 {
    todo!()
}

pub fn get_total_bytes(ipc: &IpcObjects) -> u64 {
    todo!()
}

// =====================================================================
// MAIN APP side (consider moving to the main app crate: only main
// launches the helper, so the helper doesn't need to carry this code)
// =====================================================================

/// <exe folder>\hydra-virtual-drive-helper.exe; Err if missing, so the UI
/// can say "helper not found next to the app".
pub fn helper_exe_path() -> io::Result<PathBuf> {
    let main_exe_path = env::current_exe()?;

    if let Some(exe_dir) = main_exe_path.parent() {
        let helper_path = exe_dir.join(HELPER_EXE_NAME);

        if helper_path.exists() {
            return Ok(helper_path);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} not found next to the main executable", HELPER_EXE_NAME),
    ))
}

/// Is a helper already running? (named mutex check, avoids a second UAC prompt)
pub fn is_hydra_drive_helper_running() -> bool {
    todo!()
}

/// NUL-terminated UTF-16 for Win32 W-functions.
fn to_wide(s: impl AsRef<OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

/// Launch the helper elevated (UAC prompt) with size/root/mount/parent-pid args.
///
/// Returns immediately after the UAC prompt is answered (it does NOT wait for
/// the helper to finish). If the user declines UAC, the error has
/// `raw_os_error() == Some(EXIT_UAC_DECLINED as i32)`.
pub fn start_hydra_drive_helper(total_size: u64, root: &Path) -> io::Result<HelperProcess> {
    let helper_path = helper_exe_path()?;

    // A path ending in '\' directly before the closing quote would escape
    // the quote (C runtime argv rules), so trim trailing backslashes.
    let root_str = root.display().to_string();
    let root_str = root_str.trim_end_matches('\\');

    let parent_pid = unsafe { GetCurrentProcessId() };
    let params = format!(
        "{} {} {} \"{}\" {} {} {} {}",
        ARG_SIZE, total_size, ARG_ROOT, root_str, ARG_MOUNT, MOUNT_POINT, ARG_PARENT_PID, parent_pid,
    );

    // These Vecs must stay alive until ShellExecuteExW returns.
    let verb = to_wide("runas");
    let file = to_wide(helper_path.as_os_str());
    let params = to_wide(params);

    let mut sei: SHELLEXECUTEINFOW = unsafe { mem::zeroed() };
    sei.cbSize = mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    sei.fMask = SEE_MASK_NOCLOSEPROCESS;
    sei.lpVerb = verb.as_ptr();
    sei.lpFile = file.as_ptr();
    sei.lpParameters = params.as_ptr();
    // sei.nShow = SW_HIDE as i32;
    sei.nShow = SW_SHOWNORMAL as i32;

    if unsafe { ShellExecuteExW(&mut sei) } == 0 {
        // UAC declined => ERROR_CANCELLED (1223)
        return Err(io::Error::last_os_error());
    }
    if (sei.hProcess as usize) == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "ShellExecuteExW returned no process handle",
        ));
    }

    Ok(HelperProcess { process: sei.hProcess })
}

/// Signal the stop event and wait up to `timeout_ms` for the helper to exit.
/// Returns true if it exited. (A normal process can't kill an elevated one.)
pub fn stop_hydra_drive_helper(helper: &HelperProcess, ipc: &IpcObjects, timeout_ms: u32) -> bool {
    signal_event(ipc.stop_event);
    unsafe { WaitForSingleObject(helper.process, timeout_ms) == WAIT_OBJECT_0 }
}

/// Current helper status, for the UI dot:
/// ready event set -> Running; handle alive, not ready -> Starting;
/// exited with code != 0 -> Failed(code); exited cleanly -> Stopped.
pub fn get_hydra_drive_helper_operational_status(
    helper: Option<&HelperProcess>,
    ipc: &IpcObjects,
) -> ProgramStatus {
    todo!()
}

/// Exit code if the helper has ended, None while it is still running.
pub fn get_helper_exit_code(helper: &HelperProcess) -> Option<u32> {
    unsafe {
        if WaitForSingleObject(helper.process, 0) == WAIT_TIMEOUT {
            return None;
        }
        let mut code: u32 = 0;
        if GetExitCodeProcess(helper.process, &mut code) == 0 {
            return None;
        }
        Some(code)
    }
}

// =====================================================================
// HELPER side
// =====================================================================

/// Parse std::env::args(), validate, and reject anything unexpected:
/// size > 0 and sane, root exists and its folder name is POOL_FOLDER_NAME,
/// mount point looks like "X:". Returns the exit code to use on failure.
/// Parse and validate launch arguments (without the exe path).
/// Call as: `parse_and_validate_args(env::args().skip(1))`
pub fn parse_and_validate_args<I>(args: I) -> Result<HelperArgs, u32>
where
    I: IntoIterator<Item = String>,
{
    let mut size = None;
    let mut root = None;
    let mut mount = None;
    let mut parent_pid = None;

    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        // Every flag takes exactly one value; a missing value is bad args.
        let value = it.next().ok_or(EXIT_BAD_ARGS)?;
        match flag.as_str() {
            ARG_SIZE => size = value.parse::<u64>().ok(),
            ARG_ROOT => root = Some(PathBuf::from(value)),
            ARG_MOUNT => mount = Some(value),
            ARG_PARENT_PID => parent_pid = value.parse::<u32>().ok(),
            _ => return Err(EXIT_BAD_ARGS), // reject anything unexpected
        }
    }

    let total_size = size.filter(|s| *s > 0).ok_or(EXIT_BAD_ARGS)?;
    let root = root.ok_or(EXIT_BAD_ARGS)?;
    let mount_point = mount.ok_or(EXIT_BAD_ARGS)?;
    let parent_pid = parent_pid.ok_or(EXIT_BAD_ARGS)?;

    if !root.is_dir()
        || root.file_name().and_then(|n| n.to_str()) != Some(POOL_FOLDER_NAME)
    {
        return Err(EXIT_BAD_ARGS);
    }
    let b = mount_point.as_bytes();
    if !(b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':') {
        return Err(EXIT_BAD_ARGS);
    }

    Ok(HelperArgs { total_size, root, mount_point, parent_pid })
}

/// Take the single-instance mutex. Err(EXIT_ALREADY_RUNNING) if held.
pub fn acquire_single_instance_mutex() -> Result<HANDLE, u32> {
    todo!()
}

/// Tell main the helper's state. Running sets the ready event;
/// failures are reported through the process exit code.
pub fn send_operational_status_of_hydra_helper_to_main(ipc: &IpcObjects, status: ProgramStatus) {
    todo!()
}

/// Block (no CPU) until the stop event fires OR the main app exits,
/// whichever comes first. WaitForMultipleObjects on two handles.
pub fn wait_for_stop_or_parent_exit(ipc: &IpcObjects, parent_pid: u32) {
    todo!()
}

/// Optional: is the main app still alive? (Running / Stopped)
pub fn get_main_application_operational_state(parent_pid: u32) -> ProgramStatus {
    todo!()
}

/// After mounting: trim the working set and set BELOW_NORMAL priority.
/// Do NOT use background mode (it lowers I/O priority and slows installs).
pub fn reduce_helper_footprint() {
    todo!()
}

// Removed from your original list:
//   send_flag_to_hydra_helper_to_create_v_drive: starting the helper already
//   means "create the drive". Size and root go in as launch arguments.


