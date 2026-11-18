//! What a guest on an Intel card gets at this test stage, where nothing runs on the card. Every part
//! of it follows one switch, [`Desktop`]: the drivers each program is given, the compositor's, and
//! whether the Guesthouse display driver may be in the guest. The day the backend runs work, the
//! stage's desktop becomes [`Desktop::Hardware`] and the rest follows.
//!
//! In the software desktop, Direct3D 9, 10 and 11 programs are refused Intel's drivers and draw in
//! software. The desktop's compositor is refused Direct3D 12 as well: refused 11, it takes 12, and
//! as nothing it draws is run, it gives up and crashes over and over until Windows stops the adapter
//! for every program. Other programs keep Direct3D 12, compute and Vulkan, which is what a run is for.
//!
//! The display driver is kept off then. With it in, a lab guest's compositor crashed in dwmcore.dll
//! at each start, as on a tester's, until Winlogon stopped the session's use of the GPU and both
//! display devices failed. On the basic display alone it draws in software and stays up.

use std::ffi::OsStr;

/// The desktop's compositor, by its program's file name.
const COMPOSITOR: &str = "dwm.exe";

/// `0` gives the guest the hardware desktop, for an experiment. Unset, or anything else, keeps
/// [`STAGE_DESKTOP`].
pub const DESKTOP_VARIABLE: &str = "GUESTHOUSE_INTEL_SOFTWARE_DESKTOP";
const HARDWARE_DESKTOP_VALUE: &str = "0";

/// The desktop this stage gives a guest unless a person asks for the other.
pub const STAGE_DESKTOP: Desktop = Desktop::Software;

/// Where the guest's desktop is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Desktop {
    /// In software, on the basic display, with Intel's card for everything else.
    Software,
    /// By Intel's card, with every driver given and the display driver in.
    Hardware,
}

impl Desktop {
    /// The desktop from [`DESKTOP_VARIABLE`]'s value.
    pub fn from_value(value: Option<&OsStr>) -> Self {
        match value {
            Some(value) if value == HARDWARE_DESKTOP_VALUE => Self::Hardware,
            _ => STAGE_DESKTOP,
        }
    }

    /// The desktop this process was started with.
    pub fn from_environment() -> Self {
        Self::from_value(std::env::var_os(DESKTOP_VARIABLE).as_deref())
    }

    /// Whether `program` is refused the driver for this Direct3D version, and why.
    pub fn withheld(self, direct3d_12: bool, program: &str) -> Option<Withheld> {
        if self == Self::Hardware {
            return None;
        }
        if program.eq_ignore_ascii_case(COMPOSITOR) {
            return Some(Withheld::FromTheCompositor);
        }
        (!direct3d_12).then_some(Withheld::BeforeDirect3d12)
    }

    /// Whether the Guesthouse display driver may be in the guest.
    pub fn has_display_driver(self) -> bool {
        self == Self::Hardware
    }
}

/// Why a driver is withheld.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Withheld {
    /// Direct3D 9, 10 or 11, from every program.
    BeforeDirect3d12,
    /// Any version, from the compositor.
    FromTheCompositor,
}

impl Withheld {
    pub fn why(self) -> &'static str {
        match self {
            Self::BeforeDirect3d12 => "held back by the software desktop switch",
            Self::FromTheCompositor => "the compositor gets no Intel driver while the desktop is in software",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compositor_gets_nothing_and_others_get_direct3d_12_in_the_software_desktop() {
        let software = Desktop::Software;
        assert_eq!(software.withheld(true, "DWM.EXE"), Some(Withheld::FromTheCompositor));
        assert_eq!(software.withheld(false, "dwm.exe"), Some(Withheld::FromTheCompositor));
        assert_eq!(software.withheld(false, "game.exe"), Some(Withheld::BeforeDirect3d12));
        assert_eq!(software.withheld(true, "game.exe"), None);
        assert_eq!(Desktop::Hardware.withheld(false, "dwm.exe"), None, "all of Intel's drivers for everyone");
    }

    #[test]
    fn the_display_driver_comes_with_the_hardware_desktop_only() {
        assert!(!Desktop::Software.has_display_driver());
        assert!(Desktop::Hardware.has_display_driver());
    }

    #[test]
    fn the_desktop_is_the_stages_unless_a_person_asks_for_the_hardware_one() {
        assert_eq!(Desktop::from_value(None), STAGE_DESKTOP);
        assert_eq!(Desktop::from_value(Some(OsStr::new("1"))), Desktop::Software, "as a lab exported it");
        assert_eq!(Desktop::from_value(Some(OsStr::new("0"))), Desktop::Hardware);
    }
}
