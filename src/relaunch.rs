//! Remembers that Hello was running a moment ago. macOS quits and reopens Hello after a
//! permission change ("Quit & Reopen"), and Hello is a menu bar app, so without this the
//! relaunch would come back with no window and look like it only quit.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const FILE_NAME: &str = "last-alive";
/// A launch this soon after the previous instance was alive counts as a relaunch.
const RECENT: Duration = Duration::from_secs(120);
/// How often the running app notes that it is alive.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

fn path() -> Option<PathBuf> {
    crate::app_paths::support_dir()
        .ok()
        .map(|directory| directory.join(FILE_NAME))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Notes that Hello is running right now.
pub fn heartbeat() {
    if let Some(path) = path()
        && let Err(error) = fs::write(&path, now_ms().to_string())
    {
        tracing::debug!(%error, "could not note that Hello is running");
    }
}

/// Whether the previous instance was alive moments ago on this same boot, so this
/// launch is a relaunch rather than a login or a manual start after a while.
pub fn previous_instance_was_just_running() -> bool {
    let Some(path) = path() else {
        return false;
    };
    let previous_ms = fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok());
    previous_ms.is_some_and(|previous_ms| is_relaunch(previous_ms, now_ms(), boot_time_ms()))
}

fn is_relaunch(previous_ms: u64, now_ms: u64, boot_ms: Option<u64>) -> bool {
    let recent = now_ms.saturating_sub(previous_ms) <= RECENT.as_millis() as u64;
    // A heartbeat from before this boot is a shutdown, not a relaunch.
    let same_boot = boot_ms.is_none_or(|boot_ms| previous_ms >= boot_ms);
    recent && same_boot
}

#[cfg(target_os = "macos")]
fn boot_time_ms() -> Option<u64> {
    let mut boot = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut size = std::mem::size_of::<libc::timeval>();
    let name = c"kern.boottime";
    // SAFETY: a read-only sysctl into a correctly sized timeval.
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut boot as *mut libc::timeval).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && boot.tv_sec > 0)
        .then(|| boot.tv_sec as u64 * 1_000 + boot.tv_usec as u64 / 1_000)
}

#[cfg(not(target_os = "macos"))]
fn boot_time_ms() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_right_after_a_heartbeat_is_a_relaunch() {
        assert!(is_relaunch(1_000_000, 1_030_000, Some(500_000)));
        assert!(is_relaunch(1_000_000, 1_000_000 + 120_000, None));
    }

    #[test]
    fn an_old_heartbeat_or_a_new_boot_is_not() {
        assert!(!is_relaunch(1_000_000, 1_000_000 + 120_001, Some(500_000)));
        assert!(!is_relaunch(1_000_000, 1_030_000, Some(1_010_000)));
    }

    #[test]
    fn heartbeat_round_trips_through_the_file() {
        let directory = std::env::temp_dir().join(format!("hello-relaunch-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        // SAFETY: this test owns the variable for its own directory.
        unsafe { std::env::set_var("HELLO_APPLICATION_SUPPORT_DIR", &directory) };
        heartbeat();
        assert!(previous_instance_was_just_running());
        fs::write(directory.join(FILE_NAME), "1").unwrap();
        assert!(!previous_instance_was_just_running());
        let _ = fs::remove_dir_all(directory);
    }
}
