//! A recording of a tester's Arc B580 stands in for the card: the backend built from it gives the
//! guest the record the daemon on the real card gave, as that daemon's log line says it.

use guesthouse_backend_intel::IntelBackend;
use guesthouse_backend_intel::protocol::{features, record};
use guesthouse_backend_intel::recording;

const ARC_B580: &str = include_str!("cards/arc-b580.card");
/// What the daemon logged at the start of the tester's run on the card itself.
const LOGGED_RECORD: &str =
    "product_family=1274 render_core=0xc09 device=0xe20b render_ip=0x5004000 eus=160 vm=1 compression=off";

#[test]
fn the_recorded_b580_gives_the_record_its_own_daemon_gave() {
    let card = recording::read(ARC_B580).unwrap();
    assert_eq!((card.eu_count(), card.card_memory()), (160, 0x2_FB80_0000));
    let backend = IntelBackend::from_card(card.clone()).unwrap();
    assert_eq!(backend.generation().name, "Xe2 HPG (Battlemage)");
    assert_eq!(record::summary(backend.record()).to_string(), LOGGED_RECORD);
    assert_eq!(recording::read(&recording::write(&card)), Ok(card), "and it writes back the same");
}

#[test]
fn the_recorded_b580_gives_the_guest_its_own_urb() {
    let backend = IntelBackend::from_card(recording::read(ARC_B580).unwrap()).unwrap();
    assert_eq!(
        record::urb(backend.record()).to_string(),
        "size_kb=3840 vs=64-5888 hs=0-1548 gs=2-1952 ds=50-4480 task=0-1548 mesh=4-1952 max_start=127",
        "768 KB a slice in five slices, and the stages' limits as its hwconfig gives them"
    );
}

#[test]
fn the_recorded_b580_gets_its_workaround_table_from_its_render_and_media_ips() {
    let backend = IntelBackend::from_card(recording::read(ARC_B580).unwrap()).unwrap();
    let record = backend.record();
    let set = (0..3048).filter(|&entry| features::workaround(record, entry).is_some_and(|value| value != 0));
    assert_eq!(set.count(), 194);
    assert_eq!(features::workaround(record, 2964), Some(2), "render 20.01");
    assert_eq!(features::workaround(record, 2525), Some(4), "media 13.01");
    assert_eq!(features::workaround(record, 2571), Some(8), "display 14.01 at revision 0");
}
