//! A recording of a tester's Arc B580 stands in for the card: the backend built from it gives the
//! guest the record the daemon on the real card gave, as that daemon's log line says it.

use guesthouse_backend_intel::IntelBackend;
use guesthouse_backend_intel::protocol::record;
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
