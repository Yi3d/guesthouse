//! Which of Intel's drivers a guest program is given at this test stage, where nothing runs on the
//! card. The daemon turns the software desktop on unless a person asks otherwise (its own
//! `intel_stage` says when).
//!
//! With the software desktop, Direct3D 9, 10 and 11 programs are refused Intel's drivers and draw in
//! software. The desktop's compositor is refused Direct3D 12 as well: refused 11, it takes 12, and
//! as nothing it draws is run, it gives up and crashes over and over until Windows stops the adapter
//! for every program. Other programs keep Direct3D 12, compute and Vulkan, which is what a run is for.

/// The desktop's compositor, by its program's file name.
const COMPOSITOR: &str = "dwm.exe";

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

/// Whether `program` is refused the driver for this Direct3D version, and why.
pub fn withheld(software_desktop: bool, direct3d_12: bool, program: &str) -> Option<Withheld> {
    if !software_desktop {
        return None;
    }
    if program.eq_ignore_ascii_case(COMPOSITOR) {
        return Some(Withheld::FromTheCompositor);
    }
    (!direct3d_12).then_some(Withheld::BeforeDirect3d12)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compositor_gets_nothing_and_others_get_direct3d_12_in_the_software_desktop() {
        assert_eq!(withheld(true, true, "DWM.EXE"), Some(Withheld::FromTheCompositor));
        assert_eq!(withheld(true, false, "dwm.exe"), Some(Withheld::FromTheCompositor));
        assert_eq!(withheld(true, false, "game.exe"), Some(Withheld::BeforeDirect3d12));
        assert_eq!(withheld(true, true, "game.exe"), None);
        assert_eq!(withheld(false, false, "dwm.exe"), None, "all of Intel's drivers for everyone");
    }
}
