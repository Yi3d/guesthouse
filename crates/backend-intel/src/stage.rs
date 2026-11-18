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

/// `0` asks for the hardware desktop and `1` for the software one. Unset, or anything else, leaves
/// the choice to [`Desktop::default_for`].
pub const DESKTOP_VARIABLE: &str = "GUESTHOUSE_INTEL_SOFTWARE_DESKTOP";
const HARDWARE_DESKTOP_VALUE: &str = "0";
const SOFTWARE_DESKTOP_VALUE: &str = "1";

/// The desktop this stage gives a guest on a real card unless a person asks for the other. The one
/// line to change when the hardware desktop becomes the default.
pub const STAGE_DESKTOP: Desktop = Desktop::Software;

/// A recorded card stands in for a card in the lab, and nothing can draw on it, so its guests keep
/// the software desktop unless someone asks.
const RECORDED_CARD_DESKTOP: Desktop = Desktop::Software;

/// Where the guest's desktop is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Desktop {
    /// In software, on the basic display, with Intel's card for everything else.
    Software,
    /// By Intel's card, with every driver given and the display driver in.
    Hardware,
}

impl Desktop {
    /// The desktop a person asked for in [`DESKTOP_VARIABLE`]'s value, if they asked.
    pub fn asked(value: Option<&OsStr>) -> Option<Self> {
        match value? {
            value if value == HARDWARE_DESKTOP_VALUE => Some(Self::Hardware),
            value if value == SOFTWARE_DESKTOP_VALUE => Some(Self::Software),
            _ => None,
        }
    }

    /// The desktop a guest gets when nobody asked: the stage's, or a recorded card's.
    pub fn default_for(recorded_card: bool) -> Self {
        if recorded_card { RECORDED_CARD_DESKTOP } else { STAGE_DESKTOP }
    }

    /// The desktop from [`DESKTOP_VARIABLE`]'s value, or the default.
    pub fn from_value(value: Option<&OsStr>, recorded_card: bool) -> Self {
        Self::asked(value).unwrap_or(Self::default_for(recorded_card))
    }

    /// What a person asked for in this process's environment, if anything.
    pub fn asked_in_environment() -> Option<Self> {
        Self::asked(std::env::var_os(DESKTOP_VARIABLE).as_deref())
    }

    /// The desktop this process was started with.
    pub fn from_environment() -> Self {
        let recorded_card = crate::recording::recorded_card_file().is_some();
        Self::from_value(std::env::var_os(DESKTOP_VARIABLE).as_deref(), recorded_card)
    }

    /// The value of [`DESKTOP_VARIABLE`] that asks for this desktop.
    pub fn value(self) -> &'static str {
        match self {
            Self::Hardware => HARDWARE_DESKTOP_VALUE,
            Self::Software => SOFTWARE_DESKTOP_VALUE,
        }
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
    fn each_value_asks_for_its_own_desktop_whatever_the_default() {
        for recorded_card in [false, true] {
            let from = |value: &str| Desktop::from_value(Some(OsStr::new(value)), recorded_card);
            assert_eq!(from("0"), Desktop::Hardware);
            assert_eq!(from("1"), Desktop::Software, "as a lab exported it");
            assert_eq!(from("yes"), Desktop::default_for(recorded_card));
            assert_eq!(Desktop::from_value(None, recorded_card), Desktop::default_for(recorded_card));
        }
        for desktop in [Desktop::Software, Desktop::Hardware] {
            assert_eq!(Desktop::asked(Some(OsStr::new(desktop.value()))), Some(desktop));
        }
    }

    #[test]
    fn a_real_card_gets_the_stages_desktop_and_a_recorded_one_the_software_desktop() {
        assert_eq!(Desktop::default_for(false), STAGE_DESKTOP);
        assert_eq!(Desktop::default_for(true), Desktop::Software);
    }
}
