//! The Intel render device in the guest, switched off on the software desktop and on again for the
//! hardware one: see [`crate::intel_desktop::switches_device_off`]. It is done through the agent,
//! which needs nothing from the desktop. The device stays in the VM, as without it Windows puts the
//! session on a display QEMU never shows.

use super::Task;
use crate::failure::Result;
use crate::gpu_identity::PciIdentity;
use crate::gpu_maker::GpuMaker;
use crate::intel_desktop::{device_off_file, device_off_for_start, for_start};
use crate::log::write_log;
use std::fs;

const OFF: &str = "Intel device off in the guest: desktop drawn by the basic display, no GPU for programs";
const LEFT_ON: &str = "Intel device left on with the software desktop, for work on the card: the screen \
     stays black, so tasks go through the guest agent";
const BACK_ON: &str = "Intel device on again in the guest";
/// What the guest says of a device Windows has stopped, as a switched-off one is.
const STOPPED: &str = "Error";
const PATIENCE_SECONDS: u64 = 60;

/// Switches the devices the guest shows with this identity on or off, and says each one's status.
fn switch_script(identity: PciIdentity, on: bool) -> String {
    let verb = if on { "Enable" } else { "Disable" };
    format!(
        "$d=@(Get-PnpDevice -PresentOnly -ea 0 | Where-Object {{ $_.InstanceId -like '{}' }}); \
         foreach($x in $d){{ {verb}-PnpDevice -InstanceId $x.InstanceId -Confirm:$false -ea 0 }}; \
         foreach($x in $d){{ (Get-PnpDevice -InstanceId $x.InstanceId).Status }}",
        identity.windows_pattern()
    )
}

/// Whether the guest's answer shows every device stopped, or every one running.
fn all_stopped(said: &str, stopped: bool) -> bool {
    let statuses: Vec<&str> = said.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    !statuses.is_empty() && statuses.iter().all(|status| (*status == STOPPED) == stopped)
}

fn is_intel(task: &Task) -> bool {
    task.machine.windows_maker() == Some(GpuMaker::Intel)
}

/// Before the GPU is checked: a device the app switched off on an earlier start is on again, so the
/// check sees the GPU, and a hardware desktop gets its card.
pub(super) fn switch_back_on(task: &Task, shown: PciIdentity) -> Result<()> {
    let file = device_off_file(task.context, &task.machine);
    if !is_intel(task) || !file.exists() {
        return Ok(());
    }
    let said = task.ask_guest(&switch_script(shown, true), PATIENCE_SECONDS);
    let how =
        if all_stopped(&said, false) { String::new() } else { format!(" (the guest said: {})", said.trim()) };
    write_log(task.context, &format!("{BACK_ON}{how}"))?;
    let _ = fs::remove_file(file);
    Ok(())
}

/// Once the GPU is up: on the software desktop the device goes off, so the basic display draws.
pub(super) fn switch_off_for_software_desktop(task: &Task, shown: PciIdentity) -> Result<()> {
    if !is_intel(task) || for_start(task.context, &task.machine).has_display_driver() {
        return Ok(());
    }
    if !device_off_for_start(task.context, &task.machine) {
        write_log(task.context, LEFT_ON)?;
        return Ok(());
    }
    let said = task.ask_guest(&switch_script(shown, false), PATIENCE_SECONDS);
    let how =
        if all_stopped(&said, true) { String::new() } else { format!(" (the guest said: {})", said.trim()) };
    fs::write(device_off_file(task.context, &task.machine), format!("{OFF}\n"))?;
    write_log(task.context, &format!("{OFF}{how}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu_identity::INTEL_RENDER;

    #[test]
    fn the_script_finds_the_device_by_the_identity_it_shows() {
        let off = switch_script(INTEL_RENDER, false);
        assert!(off.contains(r"-like 'PCI\VEN_8086&DEV_CFFE*'"));
        assert!(off.contains("Disable-PnpDevice") && !off.contains("Enable-PnpDevice"));
        assert!(switch_script(INTEL_RENDER, true).contains("Enable-PnpDevice"));
    }

    #[test]
    fn the_answer_is_read_device_by_device() {
        assert!(all_stopped("Error\r\n", true));
        assert!(all_stopped("OK\n", false));
        assert!(!all_stopped("OK\nError\n", true));
        assert!(!all_stopped("", true), "no device found is not a device off");
    }
}
