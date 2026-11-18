//! What a guest on an Intel card gets at this test stage, where nothing runs on the card.
//!
//! Its desktop composes in software. With Intel's Direct3D 9, 10 and 11 drivers given to it, DWM
//! starts on the adapter, crashes each time it presents, as nothing it sends is run, and Windows then
//! stops the adapter and the basic display with it: a lab guest showed this, as a tester's did.
//! Refused those drivers, DWM draws in software, the adapter stays up, and Direct3D 12, compute and
//! Vulkan still come to the backend, which is what a run is for. Which program is refused which
//! driver is the backend's `stage` module; the compositor gets none. Take this out when the backend
//! draws.

use std::ffi::OsStr;

/// `0` gives the guest Intel's Direct3D 9, 10 and 11 drivers too, for an experiment. Unset, or
/// anything else, keeps the stage's software desktop.
pub const SOFTWARE_DESKTOP_VARIABLE: &str = "GUESTHOUSE_INTEL_SOFTWARE_DESKTOP";
const DRIVERS_FOR_THE_DESKTOP: &str = "0";

/// Whether the guest's desktop composes in software, from the variable's value.
pub fn software_desktop(value: Option<&OsStr>) -> bool {
    value.is_none_or(|value| value != DRIVERS_FOR_THE_DESKTOP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_is_in_software_unless_a_person_asks_for_intels_drivers() {
        assert!(software_desktop(None), "the stage's own choice");
        assert!(software_desktop(Some(OsStr::new("1"))), "as a lab exported it before");
        assert!(!software_desktop(Some(OsStr::new("0"))));
    }
}
