//! An Intel machine started on the hardware desktop is watched until it has been up long enough to
//! show whether Windows keeps the card. When the compositor can't use the card, Windows stops the
//! card for the session within a minute or two, and a tester would be left with no desktop. Then the
//! machine is put on the software desktop, and started again on it in the same start: see
//! [`crate::intel_desktop`].

use super::{STOP_WAIT_SECONDS, Task};
use crate::failure::Result;
use crate::gpu_maker::GpuMaker;
use crate::intel_desktop;
use crate::log::write_log;
use crate::vm_host::Stopped;
use guesthouse_backend_intel::stage::Desktop;
use std::time::Duration;

/// How long after Windows boots the desktop must have stayed on the card. The lab's and a tester's
/// stops came 50 to 80 seconds in.
const WATCHED_UPTIME_SECONDS: u64 = 150;
const POLL: Duration = Duration::from_secs(15);
/// How many looks there are at most, for a guest that stops answering.
const LOOKS: u32 = 16;
const LOOK_SECONDS: u64 = 30;
/// More compositor crashes than this since boot is a crash loop.
const CRASH_LIMIT: u32 = 4;
const MARK: &str = "hardware-desktop:";

/// What the guest says about its desktop since it booted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Look {
    uptime_seconds: u64,
    /// Winlogon asked to stop the session's use of the GPU (DxgKrnl-Admin 465).
    session_stops: u32,
    /// Render devices Windows reports with no problem.
    working_render_devices: u32,
    compositor_crashes: u32,
}

impl Look {
    /// Why the hardware desktop has failed, if it has.
    fn failure(&self) -> Option<String> {
        if self.session_stops > 0 {
            return Some("Windows stopped the session's use of the card".to_owned());
        }
        if self.working_render_devices == 0 {
            return Some("Windows turned the card off".to_owned());
        }
        (self.compositor_crashes > CRASH_LIMIT)
            .then(|| format!("the desktop's compositor crashed {} times", self.compositor_crashes))
    }

    fn parse(said: &str) -> Option<Self> {
        let words: Vec<u64> = said
            .split(MARK)
            .nth(1)?
            .split_whitespace()
            .take(4)
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()
            .ok()?;
        let [uptime_seconds, stops, working, crashes] = words[..] else { return None };
        let count = |value: u64| u32::try_from(value).unwrap_or(u32::MAX);
        Some(Self {
            uptime_seconds,
            session_stops: count(stops),
            working_render_devices: count(working),
            compositor_crashes: count(crashes),
        })
    }
}

/// The guest's answer, as four numbers after [`MARK`].
fn look_script() -> String {
    format!(
        "$boot = (Get-CimInstance Win32_OperatingSystem).LastBootUpTime; \
         $up = [int]((Get-Date) - $boot).TotalSeconds; \
         $stops = @(Get-WinEvent -FilterHashtable @{{LogName='Microsoft-Windows-DxgKrnl-Admin'; Id=465; StartTime=$boot}} -ea 0).Count; \
         $working = @(Get-CimInstance Win32_VideoController | Where-Object {{ ({}) -and $_.ConfigManagerErrorCode -eq 0 }}).Count; \
         $crashes = @(Get-WinEvent -FilterHashtable @{{LogName='Application'; Id=1000; StartTime=$boot}} -ea 0 | \
         Where-Object {{ $_.Message -match 'dwm\\.exe' }}).Count; \
         \"{MARK}$up $stops $working $crashes\"",
        super::gpu_check::is_render_device()
    )
}

/// Whether this start put the desktop on an Intel card, so that it has to be watched.
fn watched(task: &Task) -> bool {
    task.machine.windows_maker() == Some(GpuMaker::Intel)
        && intel_desktop::for_start(task.context, &task.machine) == Desktop::Hardware
}

/// Looks until the guest has been up long enough, or the desktop has failed. A guest that stops
/// answering is not taken as a failure on its own: the look after the GPU check decides.
fn watch(task: &Task) -> Option<String> {
    task.step("Watching the desktop on the card");
    for attempt in 0..LOOKS {
        if attempt > 0 {
            task.sleep(POLL);
        }
        let Some(look) = Look::parse(&task.ask_guest(&look_script(), LOOK_SECONDS)) else { continue };
        if let Some(why) = look.failure() {
            return Some(why);
        }
        if look.uptime_seconds >= WATCHED_UPTIME_SECONDS {
            return None;
        }
    }
    None
}

/// Whether the hardware desktop failed on this boot, and why. `gpu_missing` is the after-boot check
/// having found no card.
pub(super) fn failed(task: &Task, gpu_missing: bool) -> Option<String> {
    if !watched(task) {
        return None;
    }
    if gpu_missing {
        return Some("Windows started without the card".to_owned());
    }
    watch(task)
}

/// What the user is told once the machine is up on the software desktop.
fn told(why: &str) -> String {
    format!(
        "The desktop didn't come up on the Intel card ({why}), so this machine is on the software \
         desktop until the next build."
    )
}

/// Puts the machine on the software desktop and stops it, ready to start again. Says why in the log.
pub(super) fn fall_back_and_stop(task: &mut Task, why: &str) -> Result<String> {
    write_log(task.context, &format!("hardware desktop: {why}; back to the software desktop"))?;
    intel_desktop::fall_back(task.context, &task.machine, why)?;
    task.step("Starting again on the software desktop");
    if task.host.stop(&task.machine, STOP_WAIT_SECONDS)? == Stopped::TurnedOff {
        write_log(
            task.context,
            "hardware desktop: Windows did not shut down in time, so the VM was turned off",
        )?;
    }
    Ok(told(why))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look(stops: u32, working: u32, crashes: u32) -> Look {
        Look {
            uptime_seconds: 60,
            session_stops: stops,
            working_render_devices: working,
            compositor_crashes: crashes,
        }
    }

    #[test]
    fn a_stop_a_card_turned_off_or_a_crash_loop_is_a_failure() {
        assert_eq!(look(0, 1, 0).failure(), None);
        assert_eq!(look(0, 1, CRASH_LIMIT).failure(), None, "a few crashes while it settles");
        assert!(look(1, 1, 0).failure().unwrap().contains("stopped"));
        assert!(look(0, 0, 0).failure().unwrap().contains("turned the card off"));
        assert!(look(0, 1, CRASH_LIMIT + 1).failure().unwrap().contains("crashed 5 times"));
    }

    #[test]
    fn the_guests_answer_is_read_after_its_mark() {
        let said = format!("noise\n{MARK}151 0 1 3\n");
        let expected =
            Look { uptime_seconds: 151, session_stops: 0, working_render_devices: 1, compositor_crashes: 3 };
        assert_eq!(Look::parse(&said), Some(expected));
        assert_eq!(Look::parse("ERROR: the agent said nothing"), None);
        assert_eq!(Look::parse(&format!("{MARK}12 0")), None);
    }

    #[test]
    fn the_message_says_why_and_until_when() {
        let message = told("Windows stopped the session's use of the card");
        assert!(message.contains("(Windows stopped the session's use of the card)"));
        assert!(message.contains("until the next build"));
    }
}
