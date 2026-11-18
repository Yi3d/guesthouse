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

/// Names what a guest's work is run on: `0` or `listen` keeps everything off the card, as the first
/// test builds did, `compute` runs compute work only, and unset runs everything this build can.
pub const RUN_VARIABLE: &str = "GUESTHOUSE_INTEL_RUN_ON_CARD";

/// How much of a guest's work runs on the card. The desktop's compositor is [`Desktop`]'s business:
/// in the software desktop it is given no driver, so it sends nothing to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Nothing is made on the card and nothing runs: memory is names, and every submission is
    /// finished unrun. This is all a recorded card can do.
    Listen,
    /// Real memory, address spaces and queues, and compute work runs: the submissions whose header
    /// says OpenCL. The rest are finished unrun.
    Compute,
    /// Everything a guest submits runs.
    Everything,
}

/// The furthest stage this build has the code for, which is what a guest gets on a card that can
/// take it unless a person asks for less.
pub const FURTHEST_STAGE: Stage = Stage::Everything;

/// Why a guest is left at [`Stage::Listen`] when more was asked for, by the layer that refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fallback {
    /// A person asked for it with [`RUN_VARIABLE`].
    AskedFor,
    /// [`RUN_VARIABLE`] holds a word this build doesn't know.
    UnknownWord(String),
    /// The card is a recording, so there is nothing to run on.
    RecordedCard,
    /// The card's kernel driver isn't xe.
    NotXe(String),
    /// The render node couldn't be opened for work.
    NodeNotOpened(String),
    /// The host's own test of xe failed this many steps.
    SelfTestFailed(u32),
    /// The card's memory regions give nowhere to put a buffer.
    NoMemoryRegion,
    /// The host's own desktop leaves the guest this many bytes of the card, which is too few.
    TooLittleMemory(u64),
}

impl std::fmt::Display for Fallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AskedFor => write!(f, "layer=switch: {RUN_VARIABLE} asks for it"),
            Self::UnknownWord(word) => {
                write!(f, "layer=switch: {RUN_VARIABLE} is \"{word}\", which this build doesn't know")
            }
            Self::RecordedCard => f.write_str("layer=card: it is a recording, with no card behind it"),
            Self::NotXe(driver) => {
                write!(f, "layer=host-driver: the card is on {driver}, and work runs through xe only")
            }
            Self::NodeNotOpened(error) => write!(f, "layer=render-node: opening it for work failed: {error}"),
            Self::SelfTestFailed(steps) => {
                write!(
                    f,
                    "layer=host-self-test: {steps} of its steps failed on this card (intel-probe --run shows them)"
                )
            }
            Self::NoMemoryRegion => f.write_str("layer=card-memory: xe named no system memory region"),
            Self::TooLittleMemory(left) => write!(
                f,
                "layer=card-memory: only {} MiB of the card is free beside the host's own use and reserve",
                left >> 20
            ),
        }
    }
}

impl Stage {
    /// The stage a person asked for with [`RUN_VARIABLE`]'s value, and why it is less than the
    /// furthest if it is. A word this build doesn't know is never read as "run".
    pub fn asked(value: Option<&OsStr>) -> (Self, Option<Fallback>) {
        let Some(value) = value else { return (FURTHEST_STAGE, None) };
        match value.to_string_lossy().trim().to_ascii_lowercase().as_str() {
            "0" | "listen" => (Self::Listen, Some(Fallback::AskedFor)),
            "compute" => (Self::Compute, None),
            "1" | "everything" => (FURTHEST_STAGE, None),
            unknown => (Self::Listen, Some(Fallback::UnknownWord(unknown.to_string()))),
        }
    }

    /// The stage this process was started with.
    pub fn from_environment() -> (Self, Option<Fallback>) {
        Self::asked(std::env::var_os(RUN_VARIABLE).as_deref())
    }

    /// Whether memory, address spaces and queues are real at this stage.
    pub fn is_real(self) -> bool {
        self != Self::Listen
    }

    /// Whether a submission whose header gives this context type runs on the card. One whose
    /// header can't be read has none, and runs only where everything does.
    pub fn runs(self, context_type: Option<u32>) -> bool {
        match self {
            Self::Listen => false,
            Self::Compute => context_type.is_some_and(|kind| COMPUTE_CONTEXT_TYPES.contains(&kind)),
            Self::Everything => true,
        }
    }
}

/// The header context types that are compute work: 5 is OpenCL. Level Zero's isn't known yet, so
/// at [`Stage::Compute`] its work is finished unrun.
pub const COMPUTE_CONTEXT_TYPES: [u32; 1] = [5];

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
    #[test]
    fn with_nothing_asked_a_guest_gets_the_furthest_stage_and_one_word_turns_it_off() {
        assert_eq!(Stage::asked(None), (FURTHEST_STAGE, None));
        assert!(FURTHEST_STAGE.is_real());
        assert_eq!(Stage::asked(Some(OsStr::new("0"))), (Stage::Listen, Some(Fallback::AskedFor)));
        assert_eq!(Stage::asked(Some(OsStr::new("listen"))), (Stage::Listen, Some(Fallback::AskedFor)));
        assert_eq!(Stage::asked(Some(OsStr::new("compute"))), (Stage::Compute, None));
        assert_eq!(Stage::asked(Some(OsStr::new("1"))), (FURTHEST_STAGE, None));
    }

    #[test]
    fn a_word_this_build_does_not_know_is_never_read_as_run() {
        for word in ["", "yes", "desktop-2", "on"] {
            let (stage, why) = Stage::asked(Some(OsStr::new(word)));
            assert_eq!(stage, Stage::Listen, "{word}");
            assert_eq!(why, Some(Fallback::UnknownWord(word.into())));
        }
    }

    #[test]
    fn each_stage_runs_what_its_name_says() {
        let (opencl, direct3d_12) = (Some(5), Some(7));
        assert!(!Stage::Listen.runs(opencl) && !Stage::Listen.runs(None));
        assert!(Stage::Compute.runs(opencl));
        assert!(!Stage::Compute.runs(direct3d_12) && !Stage::Compute.runs(None));
        assert!(Stage::Everything.runs(opencl) && Stage::Everything.runs(direct3d_12));
        assert!(Stage::Everything.runs(None));
        assert!(!Stage::Listen.is_real() && Stage::Compute.is_real());
    }

    #[test]
    fn every_fallback_names_the_layer_that_refused() {
        for fallback in [
            Fallback::AskedFor,
            Fallback::UnknownWord("x".into()),
            Fallback::RecordedCard,
            Fallback::NotXe("i915".into()),
            Fallback::NodeNotOpened("denied".into()),
            Fallback::SelfTestFailed(2),
            Fallback::NoMemoryRegion,
            Fallback::TooLittleMemory(1 << 20),
        ] {
            assert!(fallback.to_string().starts_with("layer="), "{fallback}");
        }
        assert!(Fallback::NotXe("i915".into()).to_string().contains("i915"));
    }
}
