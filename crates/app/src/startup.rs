//! Start-with-Windows. A per-user **logon scheduled task**, not the `Run` key:
//! Explorer works through `Run` entries one at a time, waiting on each, so at
//! sign-in Wutils could start minutes late (after the user already opened it,
//! so the late copy just exited). Task Scheduler fires the logon trigger right
//! away, in parallel with the shell starting.
//!
//! A standard user may register a logon task for their own account — no
//! elevation. If registration fails anyway (policy, service off), the `Run` key
//! is the fallback. Exactly one of the two exists when enabled, neither when
//! disabled, so this converges from any previous state.

use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize};
use windows::Win32::System::TaskScheduler::{ITaskService, TASK_CREATE_OR_UPDATE, TASK_LOGON_INTERACTIVE_TOKEN, TaskScheduler};
use windows::Win32::System::Threading::{
    ABOVE_NORMAL_PRIORITY_CLASS, GetCurrentProcess, NORMAL_PRIORITY_CLASS, OpenProcessToken, SetPriorityClass,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::{BSTR, PWSTR, Result};

/// Command-line flag for a launch at sign-in: start in the tray, and exit
/// quietly (without showing the window) if Wutils is already running.
pub const AUTOSTART_ARG: &str = "--autostart";

static BOOSTED: AtomicBool = AtomicBool::new(false);

/// Raises the process to above-normal priority for the sign-in rush, when
/// dozens of startup apps compete for CPU and disk.
pub fn boost() {
    if unsafe { SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS) }.is_ok() {
        BOOSTED.store(true, Ordering::Release);
    }
}

/// Back to normal priority once the startup work is done. No-op unless boosted.
pub fn end_boost() {
    if BOOSTED.swap(false, Ordering::AcqRel) {
        let _ = unsafe { SetPriorityClass(GetCurrentProcess(), NORMAL_PRIORITY_CLASS) };
    }
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Wutils";

/// Applies the setting on a worker thread: COM + the Task Scheduler service
/// take a few tens of ms, which shouldn't delay startup or the UI. Queued
/// requests collapse to the newest, so the last toggle wins.
pub fn apply_async(enabled: bool) {
    static WORKER: OnceLock<Sender<bool>> = OnceLock::new();
    let tx = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<bool>();
        std::thread::spawn(move || {
            // Own MTA on this thread; never touches the UI thread's apartment.
            let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            while let Ok(mut latest) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    latest = newer;
                }
                apply(latest);
            }
            if com {
                unsafe { CoUninitialize() };
            }
        });
        tx
    });
    let _ = tx.send(enabled);
}

fn apply(enabled: bool) {
    let exe = std::env::current_exe().unwrap_or_default();
    if !enabled {
        let _ = delete_task();
        let _ = wutils_core::reg::delete_value(RUN_KEY, RUN_VALUE);
        return;
    }
    match register_task(&exe) {
        Ok(()) => {
            // Drop the old/fallback entry, or Wutils would be launched twice.
            let _ = wutils_core::reg::delete_value(RUN_KEY, RUN_VALUE);
        }
        Err(e) => {
            wutils_core::log::line(&format!("startup: task registration failed ({e}); using Run key"));
            let _ = wutils_core::reg::set_string(RUN_KEY, Some(RUN_VALUE), &format!("\"{}\" {AUTOSTART_ARG}", exe.display()));
        }
    }
}

/// Per-user task name, so two accounts on one PC don't collide.
fn task_name() -> String {
    match std::env::var("USERNAME") {
        Ok(user) if !user.is_empty() => format!("Wutils ({user})"),
        _ => "Wutils".to_owned(),
    }
}

fn task_service() -> Result<ITaskService> {
    unsafe {
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
        let none = VARIANT::default();
        service.Connect(&none, &none, &none, &none)?;
        Ok(service)
    }
}

fn register_task(exe: &Path) -> Result<()> {
    let sid = current_user_sid().ok_or_else(windows::core::Error::from_thread)?;
    let xml = task_xml(&sid, exe);
    unsafe {
        let folder = task_service()?.GetFolder(&BSTR::from(r"\"))?;
        let none = VARIANT::default();
        folder.RegisterTask(
            &BSTR::from(task_name()),
            &BSTR::from(xml),
            TASK_CREATE_OR_UPDATE.0,
            &none,
            &none,
            TASK_LOGON_INTERACTIVE_TOKEN,
            &none,
        )?;
    }
    wutils_core::log::line(&format!("startup: registered logon task for {}", exe.display()));
    Ok(())
}

fn delete_task() -> Result<()> {
    unsafe {
        let folder = task_service()?.GetFolder(&BSTR::from(r"\"))?;
        // Fails with "not found" when it isn't registered; that's the goal anyway.
        folder.DeleteTask(&BSTR::from(task_name()), 0)
    }
}

/// The task definition. Notes on the non-defaults:
/// - `LogonTrigger` with our own SID, no `Delay`: fires as soon as we sign in.
/// - `Priority` 4 = normal. The default (7) runs the process *below normal*
///   CPU and low I/O priority for its whole life.
/// - No battery conditions and no execution time limit (the default kills the
///   task after 72 hours, which would quit the tray app).
/// - `IgnoreNew`: a second trigger while running doesn't start another copy.
fn task_xml(sid: &str, exe: &Path) -> String {
    let dir = exe.parent().unwrap_or(Path::new("."));
    format!(
        r#"<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Starts Wutils when you sign in.</Description></RegistrationInfo>
  <Triggers>
    <LogonTrigger><Enabled>true</Enabled><UserId>{sid}</UserId></LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{sid}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{exe}</Command>
      <Arguments>{AUTOSTART_ARG}</Arguments>
      <WorkingDirectory>{dir}</WorkingDirectory>
    </Exec>
  </Actions>
</Task>"#,
        exe = xml_escape(&exe.display().to_string()),
        dir = xml_escape(&dir.display().to_string()),
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The signed-in user's SID as a string (`S-1-5-21-...`), which Task Scheduler
/// accepts wherever it takes an account and which survives renames.
fn current_user_sid() -> Option<String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        // u64 storage keeps the TOKEN_USER header suitably aligned.
        let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
        let ok = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len).is_ok();
        let _ = CloseHandle(token);
        if !ok {
            return None;
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).ok()?;
        let sid = text.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        sid
    }
}
