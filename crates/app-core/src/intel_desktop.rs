//! Where an Intel machine's desktop is drawn on its next start, and the way back to the software
//! desktop when the card's didn't come up.
//!
//! A person's word in [`DESKTOP_VARIABLE`](guesthouse_backend_intel::stage::DESKTOP_VARIABLE)
//! decides, unless that same word already failed on this build. Without it a machine whose hardware
//! desktop failed on this build keeps the software one, a recorded card keeps the software one, and
//! any other gets the stage's. A new build, or a person asking for the hardware desktop after the
//! stage's own try failed, tries it again.
//!
//! Once its daemon is up, a start goes by what the daemon gave: a hardware desktop asked at a stage
//! where nothing the compositor sends runs is the software desktop, with the daemon's reason in the
//! run folder. See [`guesthouse_daemon::desktop_note`].

use crate::context::Context;
use crate::gpu_maker::GpuMaker;
use crate::machine::Machine;
use crate::updates::VERSION;
use guesthouse_backend_intel::stage::{Desktop, LISTENING_NOTE_START, RUN_VARIABLE, Stage};
use guesthouse_daemon::desktop_note::read_note;
use std::ffi::OsStr;
use std::fs;
use std::path::PathBuf;

const FALLBACK_FILE: &str = "intel-software-desktop";
/// Kept while the app has the Intel render device switched off in the guest, with why.
const DEVICE_OFF_FILE: &str = "intel-device-off";
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

/// Where the app notes that it switched the Intel render device off in the guest.
pub fn device_off_file(context: &Context, machine: &Machine) -> PathBuf {
    machine.folder(context).join(DEVICE_OFF_FILE)
}

/// Whether the Intel render device is switched off in the guest on `desktop`. On the software
/// desktop the compositor takes it for what it draws, and refused Intel's driver it keeps asking
/// and draws nothing. With the device off, it draws in software on the basic display. A person who
/// asks for work on a real card keeps the device, for that work, and the screen stays black: by
/// asking for the software desktop, or for compute work in [`RUN_VARIABLE`], which also holds on a
/// machine that fell back from the hardware desktop. Nothing runs, and the device goes off, on a
/// recorded card, at the listening stage, or where the daemon's note (`daemon_note`, see
/// [`Desktop::note`]) says the stage is listening.
pub fn switches_device_off(
    desktop: Desktop,
    asked: Option<Desktop>,
    recorded_card: bool,
    run: Option<&OsStr>,
    daemon_note: Option<&str>,
) -> bool {
    if desktop.has_display_driver() {
        return false;
    }
    let (stage, _) = Stage::asked(run);
    let daemon_listens = daemon_note.is_some_and(|note| note.starts_with(LISTENING_NOTE_START));
    let work_runs = !recorded_card && stage.is_real() && !daemon_listens;
    let work_asked_for = asked == Some(Desktop::Software) || stage == Stage::Compute;
    !(work_runs && work_asked_for)
}

/// [`switches_device_off`] for this start of an Intel machine.
pub fn device_off_for_start(context: &Context, machine: &Machine) -> bool {
    machine.windows_maker() == Some(GpuMaker::Intel)
        && switches_device_off(
            for_start(context, machine),
            Desktop::asked_in_environment(),
            context.recorded_intel_card.is_some(),
            std::env::var_os(RUN_VARIABLE).as_deref(),
            given_in_software(context).as_deref(),
        )
}

/// The desktop for this start in this process: the software desktop if the daemon gave it, and
/// otherwise the one chosen.
pub fn for_start(context: &Context, machine: &Machine) -> Desktop {
    if given_in_software(context).is_some() {
        return Desktop::Software;
    }
    chosen(context, machine, Desktop::asked_in_environment())
}

/// Why this start's daemon gave the software desktop though the hardware one was asked, if it did.
pub fn given_in_software(context: &Context) -> Option<String> {
    read_note(&context.run_folder(None))
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
    fn a_hardware_desktop_the_daemon_gave_in_software_is_the_software_desktop() {
        let scratch = Scratch::new();
        let context = scratch_context(&scratch);
        let machine = machine(&context);
        fs::create_dir_all(context.run_folder(None)).unwrap();
        let note = |why| guesthouse_daemon::desktop_note::write_note(&context.run_folder(None), why).unwrap();
        note(Some("stage=Listen layer=card"));
        assert_eq!(given_in_software(&context).as_deref(), Some("stage=Listen layer=card"));
        assert_eq!(for_start(&context, &machine), Desktop::Software);
        assert!(!for_start(&context, &machine).has_display_driver());
        note(None);
        assert_eq!(
            for_start(&context, &machine),
            chosen(&context, &machine, Desktop::asked_in_environment())
        );
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

    #[test]
    fn the_device_goes_off_on_the_software_desktop_unless_a_person_asked_for_work_on_a_card() {
        let (soft, hard) = (Desktop::Software, Desktop::Hardware);
        let word = |word| Some(OsStr::new(word));
        let compute = word("compute");
        assert!(!switches_device_off(hard, Some(hard), false, None, None), "the hardware desktop keeps it");
        assert!(switches_device_off(soft, None, false, None, None), "fallen back or by default");
        assert!(
            switches_device_off(soft, Some(hard), true, None, None),
            "the daemon gave the software desktop"
        );
        assert!(!switches_device_off(soft, Some(soft), false, None, None), "asked, with work on the card");
        assert!(switches_device_off(soft, Some(soft), true, None, None), "a recorded card runs nothing");
        for listening in ["listen", "0", "LISTEN", " listen ", "yes"] {
            assert!(switches_device_off(soft, Some(soft), false, word(listening), None), "{listening:?}");
        }
        assert!(!switches_device_off(soft, None, false, compute, None), "compute asked keeps it");
        assert!(!switches_device_off(soft, None, false, word("Compute"), None));
        assert!(switches_device_off(soft, None, true, compute, None), "but not on a recorded card");
        let listens = Some("stage=Listen layer=host-self-test: 1 of its steps failed");
        assert!(switches_device_off(soft, Some(soft), false, None, listens), "the daemon runs nothing");
        assert!(switches_device_off(soft, None, false, compute, listens));
        let compute_note = Some("stage=Compute: GUESTHOUSE_INTEL_RUN_ON_CARD=compute runs compute work only");
        assert!(!switches_device_off(soft, None, false, compute, compute_note));
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Card {
        /// A real card whose self-test passes.
        Real,
        /// A real card the daemon leaves listening: its self-test failed, or it isn't on xe.
        Listening,
        Recorded,
    }

    /// One start, from the two switches, a fallback on this build (whether a person had asked for
    /// what failed) and the card, through the app's choice, the daemon's stage and note and the
    /// app's device step: the desktop given, whether the Intel device is on in the guest, and
    /// whether work runs on the card.
    fn start(
        run: Option<&str>,
        desktop: Option<&str>,
        fell_back: Option<bool>,
        card: Card,
    ) -> (Desktop, bool, bool) {
        use guesthouse_backend_intel::stage::Fallback as Refused;
        let run = run.map(OsStr::new);
        let asked = Desktop::asked(desktop.map(OsStr::new));
        let recorded = card == Card::Recorded;
        let chosen = pick(asked, Desktop::default_for(recorded), fell_back);
        let (stage, listening) = match card {
            Card::Real => Stage::asked(run),
            Card::Listening => (Stage::Listen, Some(Refused::SelfTestFailed(1))),
            Card::Recorded => (Stage::Listen, Some(Refused::RecordedCard)),
        };
        let (given, why) = chosen.at_stage(stage, listening.as_ref(), false);
        let note = Desktop::note(given, why, stage, listening.as_ref());
        let for_start = if note.is_some() { Desktop::Software } else { chosen };
        assert_eq!(for_start, given, "the app and the daemon agree on the desktop");
        let on = !switches_device_off(given, asked, recorded, run, note.as_deref());
        (given, on, on && stage.is_real())
    }

    #[test]
    fn every_switch_fallback_and_card_gives_the_desktop_device_and_work_of_the_docs_table() {
        use Card::{Listening, Real, Recorded};
        let (hard, soft) = (Desktop::Hardware, Desktop::Software);
        let (by_default, asked) = (Some(false), Some(true));
        // (stage word, desktop word, fell back, card) => (desktop, device on, work on the card)
        let rows = [
            ((None, None, None, Real), (hard, true, true)),
            ((None, None, by_default, Real), (soft, false, false)),
            ((None, None, asked, Real), (soft, false, false)),
            ((None, Some("0"), None, Real), (hard, true, true)),
            ((None, Some("0"), by_default, Real), (hard, true, true)),
            ((None, Some("0"), asked, Real), (soft, false, false)),
            ((None, Some("1"), None, Real), (soft, true, true)),
            ((None, Some("1"), by_default, Real), (soft, true, true)),
            ((None, Some("1"), asked, Real), (soft, true, true)),
            ((Some("1"), None, None, Real), (hard, true, true)),
            ((Some("everything"), None, by_default, Real), (soft, false, false)),
            ((Some("compute"), None, None, Real), (soft, true, true)),
            ((Some("compute"), None, by_default, Real), (soft, true, true)),
            ((Some("compute"), None, asked, Real), (soft, true, true)),
            ((Some("compute"), Some("0"), None, Real), (soft, true, true)),
            ((Some("compute"), Some("0"), asked, Real), (soft, true, true)),
            ((Some("compute"), Some("1"), by_default, Real), (soft, true, true)),
        ];
        for ((run, desktop, fell_back, card), expected) in rows {
            assert_eq!(
                start(run, desktop, fell_back, card),
                expected,
                "{run:?} {desktop:?} {fell_back:?} {card:?}"
            );
        }
        let any_run = [None, Some("1"), Some("compute"), Some("0"), Some("listen"), Some("yes")];
        let nothing_runs = [Some("0"), Some("listen"), Some("LISTEN"), Some("yes")];
        for desktop in [None, Some("0"), Some("1")] {
            for fell_back in [None, by_default, asked] {
                for run in nothing_runs {
                    assert_eq!(
                        start(run, desktop, fell_back, Real),
                        (soft, false, false),
                        "{run:?} {desktop:?}"
                    );
                }
                for run in any_run {
                    for card in [Listening, Recorded] {
                        let got = start(run, desktop, fell_back, card);
                        assert_eq!(got, (soft, false, false), "{run:?} {desktop:?} {fell_back:?} {card:?}");
                    }
                }
            }
        }
    }
}
