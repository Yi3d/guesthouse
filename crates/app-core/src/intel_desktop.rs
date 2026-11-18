//! Where an Intel machine's desktop is drawn on its next start, and the way back to the software
//! desktop when the card's didn't come up.
//!
//! A person's word in [`DESKTOP_VARIABLE`](guesthouse_backend_intel::stage::DESKTOP_VARIABLE)
//! decides, unless that same word already failed on this build. Without it a machine whose hardware
//! desktop failed on this build keeps the software one, a recorded card keeps the software one, and
//! any other gets the stage's. A new build, or a person asking for the hardware desktop after the
//! stage's own try failed, tries it again.

use crate::context::Context;
use crate::machine::Machine;
use crate::updates::VERSION;
use guesthouse_backend_intel::stage::Desktop;
use std::fs;
use std::path::PathBuf;

const FALLBACK_FILE: &str = "intel-software-desktop";
/// The second line of the file: whether a person had asked for the hardware desktop that failed.
const ASKED: &str = "asked";
const BY_DEFAULT: &str = "default";

/// Why a machine is back on the software desktop, and the build that put it there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fallback {
    pub build: String,
    /// A person had asked for the hardware desktop that failed.
    pub asked: bool,
    pub reason: String,
}

/// Where the fallback is kept: the build, then [`ASKED`] or [`BY_DEFAULT`], then why.
pub fn fallback_file(context: &Context, machine: &Machine) -> PathBuf {
    machine.folder(context).join(FALLBACK_FILE)
}

pub fn fallback(context: &Context, machine: &Machine) -> Option<Fallback> {
    let text = fs::read_to_string(fallback_file(context, machine)).ok()?;
    let mut lines = text.lines();
    let build = lines.next()?.trim().to_owned();
    let asked = lines.next().is_some_and(|line| line.trim() == ASKED);
    let reason = lines.collect::<Vec<_>>().join(" ").trim().to_owned();
    Some(Fallback { build, asked, reason })
}

/// The next starts go to the software desktop, for this reason, until there's another build.
pub fn fall_back(context: &Context, machine: &Machine, reason: &str) -> std::io::Result<()> {
    let how = if Desktop::asked_in_environment() == Some(Desktop::Hardware) { ASKED } else { BY_DEFAULT };
    fs::write(fallback_file(context, machine), format!("{VERSION}\n{how}\n{reason}\n"))
}

/// The desktop for this start, with what a person asked for, if they did.
pub fn chosen(context: &Context, machine: &Machine, asked: Option<Desktop>) -> Desktop {
    let default = Desktop::default_for(context.recorded_intel_card.is_some());
    let on_this_build = fallback(context, machine).filter(|fallback| fallback.build == VERSION);
    pick(asked, default, on_this_build.map(|fallback| fallback.asked))
}

/// A person's word, then a fallback on this build, then the default. `fell_back` says whether this
/// build fell back, and if so whether a person had asked for what failed. The same word failing
/// again is not tried again, which also keeps a fallback within one start from going round.
fn pick(asked: Option<Desktop>, default: Desktop, fell_back: Option<bool>) -> Desktop {
    match (asked, fell_back) {
        (Some(Desktop::Hardware), Some(true)) => Desktop::Software,
        (Some(desktop), _) => desktop,
        (None, Some(_)) => Desktop::Software,
        (None, None) => default,
    }
}

/// The desktop for this start in this process.
pub fn for_start(context: &Context, machine: &Machine) -> Desktop {
    chosen(context, machine, Desktop::asked_in_environment())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Scratch, scratch_context};
    use guesthouse_backend_intel::stage::STAGE_DESKTOP;

    fn machine(context: &Context) -> Machine {
        let machine = Machine::new("arc", "Arc", "/m/disk.qcow2", "");
        fs::create_dir_all(machine.folder(context)).unwrap();
        machine
    }

    #[test]
    fn with_either_default_a_fallback_gives_software_until_a_new_word() {
        for default in [Desktop::Software, Desktop::Hardware] {
            assert_eq!(pick(None, default, None), default);
            assert_eq!(pick(None, default, Some(false)), Desktop::Software);
            assert_eq!(pick(None, default, Some(true)), Desktop::Software);
            assert_eq!(
                pick(Some(Desktop::Hardware), default, Some(false)),
                Desktop::Hardware,
                "asked after the stage's try"
            );
            assert_eq!(
                pick(Some(Desktop::Hardware), default, Some(true)),
                Desktop::Software,
                "the same word failed"
            );
            assert_eq!(pick(Some(Desktop::Hardware), default, None), Desktop::Hardware);
            assert_eq!(pick(Some(Desktop::Software), default, None), Desktop::Software);
        }
    }

    #[test]
    fn nobody_asking_gives_the_stages_desktop_and_a_recorded_card_the_software_one() {
        let scratch = Scratch::new();
        let mut context = scratch_context(&scratch);
        let machine = machine(&context);
        assert_eq!(chosen(&context, &machine, None), STAGE_DESKTOP);
        context.recorded_intel_card = Some(scratch.path("b580.card"));
        assert_eq!(chosen(&context, &machine, None), Desktop::Software);
        assert_eq!(chosen(&context, &machine, Some(Desktop::Hardware)), Desktop::Hardware);
    }

    #[test]
    fn a_fallback_is_read_back_and_lasts_for_its_build_only() {
        let scratch = Scratch::new();
        let context = scratch_context(&scratch);
        let machine = machine(&context);
        fs::write(
            fallback_file(&context, &machine),
            format!("{VERSION}\n{ASKED}\nWindows stopped the card\n"),
        )
        .unwrap();
        let kept = fallback(&context, &machine).unwrap();
        let expected = Fallback {
            build: VERSION.to_owned(),
            asked: true,
            reason: "Windows stopped the card".to_owned(),
        };
        assert_eq!(kept, expected);
        assert_eq!(chosen(&context, &machine, Some(Desktop::Hardware)), Desktop::Software);
        fs::write(fallback_file(&context, &machine), format!("0.0.1\n{ASKED}\nan older build's\n")).unwrap();
        assert_eq!(
            chosen(&context, &machine, Some(Desktop::Hardware)),
            Desktop::Hardware,
            "a new build tries again"
        );
        assert_eq!(chosen(&context, &machine, None), STAGE_DESKTOP);
    }

    #[test]
    fn falling_back_writes_this_build() {
        let scratch = Scratch::new();
        let context = scratch_context(&scratch);
        let machine = machine(&context);
        fall_back(&context, &machine, "the compositor kept crashing").unwrap();
        let kept = fallback(&context, &machine).unwrap();
        assert_eq!((kept.build.as_str(), kept.reason.as_str()), (VERSION, "the compositor kept crashing"));
    }
}
