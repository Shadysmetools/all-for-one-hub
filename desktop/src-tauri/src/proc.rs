//! Kill the whole `claude` process tree (M4).
//!
//! - Stop / barge-in: `taskkill /PID <pid> /T /F` on Windows (spawned directly, no shell),
//!   `kill -KILL -<pgid>` on unix (claude is started as its own process group).
//! - App exit or crash (SEC-R2-M4): on Windows every claude child is put in a Job Object with
//!   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so the OS kills the tree when the app dies, even on
//!   a crash. On unix (and as a second net on Windows) live pids are tracked and killed when
//!   the app exits (`kill_all`, called from the run loop's Exit event).

use std::process::{Command, Stdio};
use std::sync::Mutex;

/// Program and argv that kill the tree rooted at `pid`.
pub fn kill_tree_argv(pid: u32) -> (&'static str, Vec<String>) {
    if cfg!(windows) {
        ("taskkill", vec!["/PID".into(), pid.to_string(), "/T".into(), "/F".into()])
    } else {
        ("kill", vec!["-KILL".into(), format!("-{pid}")])
    }
}

/// Best effort: kills the process and everything it started. Returns whether the helper ran OK.
pub fn kill_tree(pid: u32) -> bool {
    let (prog, args) = kill_tree_argv(pid);
    let mut cmd = Command::new(prog);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000); // CREATE_NO_WINDOW
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

/// Kill the tree, then the child itself (covers a failed helper).
pub async fn kill_child(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        let _ = tokio::task::spawn_blocking(move || kill_tree(pid)).await;
    }
    let _ = child.kill().await;
}

// ---- live pids -------------------------------------------------------------------

static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn live() -> std::sync::MutexGuard<'static, Vec<u32>> {
    LIVE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Registers a running claude child; unregisters on drop. On Windows it also joins the
/// kill-on-close Job Object.
pub struct Tracked(Option<u32>);

pub fn track(child: &tokio::process::Child) -> Tracked {
    let pid = child.id();
    if let Some(p) = pid {
        live().push(p);
    }
    #[cfg(windows)]
    if let Some(h) = child.raw_handle() {
        if !job::assign_to_app_job(h as _) {
            // SEC L1: without the job, a crash of One would leave this claude tree running.
            eprintln!("could not add the claude process to the kill-on-close Job Object: crash protection is off for this turn");
        }
    }
    Tracked(pid)
}

impl Drop for Tracked {
    fn drop(&mut self) {
        if let Some(p) = self.0 {
            live().retain(|x| *x != p);
        }
    }
}

/// Kill every tracked claude tree. Called when the app exits.
pub fn kill_all() {
    let pids: Vec<u32> = std::mem::take(&mut *live());
    for p in pids {
        kill_tree(p);
    }
}

#[cfg(test)]
pub fn live_count() -> usize {
    live().len()
}

// ---- Windows Job Object --------------------------------------------------------------

#[cfg(windows)]
pub mod job {
    use std::ffi::c_void;
    use std::sync::OnceLock;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// A Job Object whose processes die when the last handle to it closes.
    pub struct Job(HANDLE);
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn new() -> Option<Job> {
            // SAFETY: plain Win32 calls with a zeroed, correctly sized info struct.
            unsafe {
                let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if h.is_null() {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    CloseHandle(h);
                    return None;
                }
                Some(Job(h))
            }
        }

        pub fn assign(&self, process: HANDLE) -> bool {
            // SAFETY: both handles are valid for the duration of the call.
            unsafe { AssignProcessToJobObject(self.0, process) != 0 }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: we own the handle. Closing it kills every process still in the job.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// The app-wide job. It is never dropped: the OS closes it when the app exits or crashes,
    /// and that kills every claude tree in it.
    static APP_JOB: OnceLock<Option<Job>> = OnceLock::new();

    fn create_app_job() -> Option<Job> {
        let job = Job::new();
        if job.is_none() {
            eprintln!("could not create the kill-on-close Job Object (SEC L1): no crash protection for claude processes");
        }
        job
    }

    /// False when there is no job or the assignment failed; the caller logs it.
    pub fn assign_to_app_job(process: HANDLE) -> bool {
        APP_JOB.get_or_init(create_app_job).as_ref().is_some_and(|j| j.assign(process))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn l1_a_failed_job_assignment_is_reported_to_the_caller_not_swallowed() {
        let job = job::Job::new().expect("job");
        // a null process handle cannot be assigned: the caller must see `false` (and logs it)
        assert!(!job.assign(std::ptr::null_mut()));
    }

    #[test]
    fn argv_is_a_direct_tree_kill_without_a_shell() {
        let (prog, args) = kill_tree_argv(4242);
        assert!(!["cmd", "cmd.exe", "sh", "bash", "powershell"].contains(&prog));
        if cfg!(windows) {
            assert_eq!((prog, args), ("taskkill", vec!["/PID", "4242", "/T", "/F"].into_iter().map(String::from).collect::<Vec<_>>()));
        } else {
            assert_eq!(args, vec!["-KILL".to_string(), "-4242".to_string()]);
        }
    }

    #[tokio::test]
    async fn tracked_pids_are_registered_and_released() {
        let before = live_count();
        let prog = if cfg!(windows) { "ping" } else { "sleep" };
        let args: &[&str] = if cfg!(windows) { &["-n", "30", "127.0.0.1"] } else { &["30"] };
        let mut child = tokio::process::Command::new(prog).args(args).stdout(Stdio::null()).kill_on_drop(true).spawn().unwrap();
        {
            let _t = track(&child);
            assert_eq!(live_count(), before + 1);
        }
        assert_eq!(live_count(), before, "dropping the guard unregisters");
        let _ = child.kill().await;
    }

    /// SEC-R2-M4: closing the Job Object (what the OS does when the app exits or crashes)
    /// kills a process that is in it, with no help from the app.
    #[cfg(windows)]
    #[test]
    fn job_object_kills_its_processes_when_closed() {
        use std::os::windows::io::AsRawHandle;
        let job = job::Job::new().expect("create job");
        let mut child = Command::new("ping").args(["-n", "60", "127.0.0.1"]).stdout(Stdio::null()).spawn().unwrap();
        assert!(job.assign(child.as_raw_handle() as _), "assign the child to the job");
        assert!(child.try_wait().unwrap().is_none(), "still running while the job is open");
        drop(job); // the last handle closes
        let start = std::time::Instant::now();
        let ended = loop {
            if child.try_wait().unwrap().is_some() {
                break true;
            }
            if start.elapsed() > std::time::Duration::from_secs(5) {
                break false;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        if !ended {
            let _ = child.kill();
        }
        assert!(ended, "closing the job must kill the process");
    }

    /// Live: `cmd` starts a long `ping` grandchild; killing the tree must kill both.
    /// Run: cargo test kill_tree_kills_grandchildren -- --ignored --nocapture
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn kill_tree_kills_grandchildren() {
        let mut parent = Command::new("cmd").args(["/c", "ping -n 60 127.0.0.1 > nul"]).stdout(Stdio::null()).spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let ps = |filter: &str| {
            let out = Command::new("powershell")
                .args(["-NoProfile", "-Command", &format!("(Get-CimInstance Win32_Process -Filter \"{filter}\" | Measure-Object).Count")])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().unwrap()
        };
        let filter = format!("ParentProcessId={}", parent.id());
        assert_eq!(ps(&filter), 1, "grandchild is running before the kill");
        assert!(kill_tree(parent.id()));
        let _ = parent.wait();
        assert_eq!(ps(&filter), 0, "grandchild is gone after the tree kill");
    }
}
