//! What the buffer list of a submission is made of, logged every few seconds.
//!
//! Each submission names every buffer its space has taken in (see `submit`), and the kernel locks
//! and checks each one every time. A buffer made on the card for one space alone could instead be
//! made in that space's own open as always valid, and leave the list. This counts, for each space
//! that ran work, how many of its buffers are of each kind, so a tester's log shows how much that
//! would take off the list. It changes nothing.
//!
//! The line is `amd list space=N runs=R named=T made_here=A made_shared=B daemon_reads=C
//! guest_pages=D adopted=E own=F`. `made_here` is made on the card and in no other space;
//! `made_shared` is made on the card and taken into another space too; `daemon_reads` is how many
//! of those two the daemon itself reads for the screen copy; `guest_pages` is guest memory taken in
//! as a dma-buf; `adopted` is host pages adopted by address; `own` is the daemon's own buffers,
//! which the backend's books don't hold.

use crate::memory::GemHandle;
use crate::space_vm::Origin;
use guesthouse_backend::SpaceId;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

/// How often the line is written, for each space that ran work since.
pub const EVERY: Duration = Duration::from_secs(10);

/// The runs each space sent since the last line.
#[derive(Debug, Default)]
pub struct Census {
    since: Option<Instant>,
    runs: BTreeMap<SpaceId, u64>,
}

/// One space's list, by kind.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Kinds {
    pub named: usize,
    pub made_here: usize,
    pub made_shared: usize,
    pub daemon_reads: usize,
    pub guest_pages: usize,
    pub adopted: usize,
    pub own: usize,
}

impl Census {
    /// Counts a run in `space`. Says whether a line is due, and if so hands over the runs counted
    /// since the last and starts again.
    pub fn note(&mut self, space: SpaceId, now: Instant) -> Option<BTreeMap<SpaceId, u64>> {
        *self.runs.entry(space).or_insert(0) += 1;
        let since = *self.since.get_or_insert(now);
        if now.duration_since(since) < EVERY {
            return None;
        }
        self.since = Some(now);
        Some(std::mem::take(&mut self.runs))
    }
}

/// Sorts each space's buffers, by their handles on the first open, into kinds. `lists` holds every
/// space, as a buffer an idle space holds is still shared. `origins` is what
/// the backend made or adopted; `daemon_reads` is what the daemon reads itself.
pub fn sort(
    lists: &BTreeMap<SpaceId, Vec<GemHandle>>,
    origins: &HashMap<GemHandle, Origin>,
    daemon_reads: &HashSet<GemHandle>,
) -> BTreeMap<SpaceId, Kinds> {
    let mut spaces_holding: HashMap<GemHandle, usize> = HashMap::new();
    for list in lists.values() {
        for &buffer in list {
            *spaces_holding.entry(buffer).or_insert(0) += 1;
        }
    }
    let sorted = lists.iter().map(|(&space, list)| {
        let mut kinds = Kinds { named: list.len(), ..Kinds::default() };
        for buffer in list {
            match origins.get(buffer) {
                Some(Origin::Made) => {
                    if spaces_holding[buffer] > 1 {
                        kinds.made_shared += 1;
                    } else {
                        kinds.made_here += 1;
                    }
                    if daemon_reads.contains(buffer) {
                        kinds.daemon_reads += 1;
                    }
                }
                Some(Origin::HostPages { as_dma_buf: true, .. }) => kinds.guest_pages += 1,
                Some(Origin::HostPages { as_dma_buf: false, .. }) => kinds.adopted += 1,
                None => kinds.own += 1,
            }
        }
        (space, kinds)
    });
    sorted.collect()
}

/// Writes the line for each space that ran.
pub fn log(runs: &BTreeMap<SpaceId, u64>, sorted: &BTreeMap<SpaceId, Kinds>) {
    for line in lines(runs, sorted) {
        log::info!("{line}");
    }
}

/// The line for each space that ran.
fn lines(runs: &BTreeMap<SpaceId, u64>, sorted: &BTreeMap<SpaceId, Kinds>) -> Vec<String> {
    let mut lines = Vec::new();
    for (space, kinds) in sorted {
        let Some(runs) = runs.get(space) else { continue };
        lines.push(format!(
            "amd list space={} runs={runs} named={} made_here={} made_shared={} daemon_reads={} \
             guest_pages={} adopted={} own={}",
            space.0,
            kinds.named,
            kinds.made_here,
            kinds.made_shared,
            kinds.daemon_reads,
            kinds.guest_pages,
            kinds.adopted,
            kinds.own
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_due_only_once_the_time_has_passed() {
        let start = Instant::now();
        let mut census = Census::default();
        assert!(census.note(SpaceId(1), start).is_none());
        assert!(census.note(SpaceId(2), start + Duration::from_secs(3)).is_none());
        let runs = census.note(SpaceId(1), start + EVERY).expect("due");
        assert_eq!(runs.get(&SpaceId(1)), Some(&2));
        assert_eq!(runs.get(&SpaceId(2)), Some(&1));
        assert!(census.note(SpaceId(1), start + EVERY + Duration::from_secs(1)).is_none());
    }

    #[test]
    fn buffers_are_sorted_by_where_they_came_from_and_who_else_holds_them() {
        let (private, shared, pages, adopted, own) =
            (GemHandle(1), GemHandle(2), GemHandle(3), GemHandle(4), GemHandle(5));
        let origins = HashMap::from([
            (private, Origin::Made),
            (shared, Origin::Made),
            (pages, Origin::HostPages { address: 0, size: 0x1000, as_dma_buf: true }),
            (adopted, Origin::HostPages { address: 0, size: 0x1000, as_dma_buf: false }),
        ]);
        let lists = BTreeMap::from([
            (SpaceId(1), vec![private, shared, pages, adopted, own]),
            (SpaceId(2), vec![shared]),
        ]);
        let sorted = sort(&lists, &origins, &HashSet::from([shared]));
        assert_eq!(
            sorted[&SpaceId(1)],
            Kinds {
                named: 5,
                made_here: 1,
                made_shared: 1,
                daemon_reads: 1,
                guest_pages: 1,
                adopted: 1,
                own: 1
            }
        );
        assert_eq!(
            sorted[&SpaceId(2)],
            Kinds { named: 1, made_shared: 1, daemon_reads: 1, ..Kinds::default() }
        );
    }

    #[test]
    fn a_space_that_ran_nothing_still_makes_a_buffer_shared_but_gets_no_line() {
        let buffer = GemHandle(7);
        let origins = HashMap::from([(buffer, Origin::Made)]);
        let lists = BTreeMap::from([(SpaceId(1), vec![buffer]), (SpaceId(9), vec![buffer])]);
        let sorted = sort(&lists, &origins, &HashSet::new());
        assert_eq!(sorted[&SpaceId(1)].made_shared, 1);
        assert_eq!(sorted[&SpaceId(1)].made_here, 0);
        let runs = BTreeMap::from([(SpaceId(1), 3)]);
        assert_eq!(lines(&runs, &sorted).len(), 1);
    }
}
