//! Submissions: an xe exec queue behind each of the guest's queues, made on its first work, and
//! what becomes of each batch.
//!
//! A batch that ends runs in the process's VM. xe writes the guest's fence at its end, as a user
//! fence, and signals a point of the queue's own timeline, by which a run that ended without
//! writing its fence is found. A batch that parks is a ring: see [`super::choices::Rings`].

use super::choices::{self, Rings};
use super::{CardWork, engines};
use crate::protocol::command_header::{Header, Length};
use crate::xe::abi::Sync;
use crate::xe::batch::{self, Ending};
use crate::xe::plan::{self, Backing, ExecVerdict, Point};
use guesthouse_backend::{BackendError, CommandBuffer, Doorbell, HostServices, SpaceId, Work, WorkStart};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// A run on the card whose end is waited for.
#[derive(Debug, Clone, Copy)]
pub(super) struct Run {
    /// The point of the queue's timeline xe signals when it ends, however it ends. Zero for a ring.
    pub point: u64,
    /// The guest's fence, by guest physical address, and the value this run brings it to.
    pub fence: u64,
    pub value: u64,
    /// The batch: its GPU address, its length in bytes, and where it goes on if it chains.
    pub batch: u64,
    pub length: u64,
    pub chain: Option<u64>,
    /// When it was sent, and how many changes its space's mappings had had by then.
    pub sent: Option<Instant>,
    pub changes_before: u64,
}

#[derive(Debug)]
pub(super) struct Queue {
    pub space: SpaceId,
    /// The xe queue for batches that end, with the timeline its runs signal.
    ordinary: Option<(u32, u32)>,
    next_point: u64,
    pub in_flight: VecDeque<Run>,
    /// The xe queue for a ring, in the space's long-running VM, and the last fence value a ring
    /// submission named.
    ring: Option<u32>,
    ring_fence: Option<(u64, u64)>,
    /// The latest ring submission, and whether its queue's ban has been looked into.
    ring_run: Option<Run>,
    ring_banned_seen: bool,
}

/// Work checked and ready, waiting for its doorbell.
#[derive(Debug)]
pub(super) struct Prepared {
    queue: u32,
    xe_queue: u32,
    batch: u64,
    syncs: Vec<Sync>,
    /// `None` for a ring, which signals nothing.
    run: Option<Run>,
    /// What a ring submission is, for the log if its queue is banned.
    ring_run: Option<Run>,
}

/// Why a span of GPU memory couldn't be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReadError {
    NotMapped,
    Unreadable,
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotMapped => "it isn't inside one mapping",
            Self::Unreadable => "it can't be read",
        })
    }
}

/// Why a submission is refused and its queue stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    SpaceFailed,
    HeaderSize(usize),
    PatchList,
    NoEngine(u32),
    NotMapped { address: u64, length: u64 },
    Unreadable { address: u64 },
    RunsOn { address: u64 },
    Ring,
    Xe(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SpaceFailed => f.write_str("its process's work was given up earlier"),
            Self::HeaderSize(size) => {
                write!(f, "its private data is {size} bytes, which is no header's size")
            }
            Self::PatchList => {
                f.write_str("it asks for addresses to be patched into the batch, which nothing does")
            }
            Self::NoEngine(node) => write!(f, "node {node} has no engine on this card"),
            Self::NotMapped { address, length } => {
                write!(f, "its batch at {address:#x} for {length:#x} bytes isn't inside one mapping")
            }
            Self::Unreadable { address } => write!(f, "the end of its batch at {address:#x} can't be read"),
            Self::RunsOn { address } => {
                write!(f, "its batch at {address:#x} neither ends, chains nor parks, and would run on")
            }
            Self::Ring => f.write_str("its batch parks and never ends, and rings are switched off"),
            Self::Xe(error) => write!(f, "xe made no queue for it: {error}"),
        }
    }
}

/// What a submission's batch is, once checked.
enum Kind {
    /// Ends, or chains on at the address.
    Ends(Option<u64>),
    Ring,
}

impl CardWork {
    /// Checks a submission and prepares it. Nothing is sent before its doorbell.
    pub fn start(&mut self, host: &mut dyn HostServices, work: &Work<'_>, header: &Header) -> WorkStart {
        if !work.may_run || work.queue_failed {
            return WorkStart::Finished;
        }
        match self.prepare(host, work, header) {
            Ok(Some(prepared)) => {
                let doorbell = self.new_id();
                self.prepared.insert(doorbell, prepared);
                WorkStart::Started { doorbell, work: 1 }
            }
            Ok(None) => WorkStart::Finished,
            Err(refused) => {
                log::warn!("intel work: queue {:#x} in space {:?} stops: {refused}", work.queue, work.space);
                WorkStart::QueueFailed
            }
        }
    }

    /// `Ok(None)` is work this stage doesn't run, or can't yet: it is finished unrun.
    fn prepare(
        &mut self,
        host: &mut dyn HostServices,
        work: &Work<'_>,
        header: &Header,
    ) -> Result<Option<Prepared>, Refused> {
        if self.space_failed(work.space) {
            return Err(Refused::SpaceFailed);
        }
        let Some(commands) = work.commands else {
            // A context's submission: where its commands are isn't read from the message yet, and
            // its private data isn't known to be this header.
            log::debug!(
                "intel work: queue {:#x} names no commands, so its work is finished unrun",
                work.queue
            );
            return Ok(None);
        };
        if header.verdict == Length::Neither {
            return Err(Refused::HeaderSize(header.length));
        }
        let compact = header.compact.as_ref();
        if compact.is_some_and(|compact| compact.patch_list) {
            return Err(Refused::PatchList);
        }
        if !self.stage.runs(compact.map(|compact| compact.context_type)) {
            return Ok(None);
        }
        let engine = engines::for_node(&self.engines, work.node).ok_or(Refused::NoEngine(work.node))?;
        let kind = match self.ending(host, work.space, commands)? {
            Ending::Ends => Kind::Ends(None),
            Ending::Chains(to) => Kind::Ends(Some(to)),
            Ending::Parks if choices::RINGS == Rings::LongRunning => Kind::Ring,
            Ending::Parks => return Err(Refused::Ring),
            Ending::RunsOn => return Err(Refused::RunsOn { address: commands.address }),
        };
        let space = work.space;
        let queue = self.queues.entry(work.queue).or_insert_with(|| Queue {
            space,
            ordinary: None,
            next_point: 0,
            in_flight: VecDeque::new(),
            ring: None,
            ring_fence: None,
            ring_run: None,
            ring_banned_seen: false,
        });
        let length = u64::from(commands.length) * choices::COMMAND_LENGTH_UNIT;
        let run_of = |point: u64, chain: Option<u64>| Run {
            point,
            fence: work.fence,
            value: work.fence_value,
            batch: commands.address,
            length,
            chain,
            sent: None,
            changes_before: 0,
        };
        match kind {
            Kind::Ring => {
                let xe_queue = match queue.ring {
                    Some(xe_queue) => xe_queue,
                    None => {
                        let vm = self.ring_vm(space).map_err(|error| Refused::Xe(error.to_string()))?;
                        let made = self
                            .xe
                            .create_queue(vm, engine)
                            .map_err(|error| Refused::Xe(error.to_string()))?;
                        self.queues.get_mut(&work.queue).expect("made above").ring = Some(made);
                        made
                    }
                };
                // The ring writes the queue's fence itself as it goes. A fence written by xe when
                // the ring at last stops would bring it back to this old value, so none is asked.
                let queue = self.queues.get_mut(&work.queue).expect("made above");
                queue.ring_fence = Some((work.fence, work.fence_value));
                Ok(Some(Prepared {
                    queue: work.queue,
                    xe_queue,
                    batch: commands.address,
                    syncs: Vec::new(),
                    run: None,
                    ring_run: Some(run_of(0, None)),
                }))
            }
            Kind::Ends(chain) => {
                let (xe_queue, done) = match queue.ordinary {
                    Some(made) => made,
                    None => {
                        let vm = self.spaces.get(&space).ok_or(Refused::SpaceFailed)?.vm;
                        let made =
                            self.make_ordinary(vm, engine).map_err(|error| Refused::Xe(error.to_string()))?;
                        self.queues.get_mut(&work.queue).expect("made above").ordinary = Some(made);
                        made
                    }
                };
                let queue = self.queues.get_mut(&work.queue).expect("made above");
                queue.next_point += 1;
                let point = queue.next_point;
                let held = self.spaces.get(&space).ok_or(Refused::SpaceFailed)?;
                let last_bind =
                    (held.bind_point > 0).then_some(Point { timeline: held.binds, point: held.bind_point });
                // The card writes the fence where the guest watches it, if that is mapped and
                // aligned as xe wants. Otherwise the host writes it when the run has ended.
                let fence_mapped = work.fence_gpu_address.is_multiple_of(plan::FENCE_ALIGNMENT)
                    && held.holding(work.fence_gpu_address, plan::FENCE_ALIGNMENT).is_some();
                let fence = fence_mapped.then_some((work.fence_gpu_address, work.fence_value));
                let syncs = plan::exec_syncs(last_bind, fence, Point { timeline: done, point });
                let run = run_of(point, chain);
                Ok(Some(Prepared {
                    queue: work.queue,
                    xe_queue,
                    batch: commands.address,
                    syncs,
                    run: Some(run),
                    ring_run: None,
                }))
            }
        }
    }

    /// An xe queue in a VM with the timeline its runs signal.
    fn make_ordinary(
        &mut self,
        vm: u32,
        engine: crate::xe::abi::EngineInstance,
    ) -> std::io::Result<(u32, u32)> {
        let done = self.xe.create_syncobj()?;
        match self.xe.create_queue(vm, engine) {
            Ok(queue) => Ok((queue, done)),
            Err(error) => {
                let _ = self.xe.destroy_syncobj(done);
                Err(error)
            }
        }
    }

    /// How a batch ends, read from its last bytes where they lie: a buffer through the CPU's view
    /// of it, guest pages through the core.
    fn ending(
        &mut self,
        host: &mut dyn HostServices,
        space: SpaceId,
        commands: CommandBuffer,
    ) -> Result<Ending, Refused> {
        let (address, length) = (commands.address, u64::from(commands.length) * choices::COMMAND_LENGTH_UNIT);
        let not_mapped = Refused::NotMapped { address, length };
        let held = self.spaces.get(&space).ok_or(Refused::SpaceFailed)?;
        if held.holding(address, length).is_none() || length == 0 {
            return Err(not_mapped);
        }
        let tail_length = length.min(batch::ENDING_BYTES as u64) & !3;
        let skipped = length - tail_length;
        let tail =
            self.read_gpu(host, space, address + skipped, tail_length).map_err(|error| match error {
                ReadError::NotMapped => Refused::NotMapped { address, length },
                ReadError::Unreadable => Refused::Unreadable { address: address + skipped },
            })?;
        Ok(batch::ending_at(&tail, address + skipped))
    }

    /// Reads bytes of a space at a GPU address, all of which must lie in one mapping: of a buffer
    /// through the CPU's view of it, of guest pages through the core.
    pub(super) fn read_gpu(
        &mut self,
        host: &mut dyn HostServices,
        space: SpaceId,
        address: u64,
        length: u64,
    ) -> Result<Vec<u8>, ReadError> {
        let held = self.spaces.get(&space).ok_or(ReadError::NotMapped)?;
        let (memory, offset) = held.holding(address, length).ok_or(ReadError::NotMapped)?;
        let mut bytes = vec![0u8; usize::try_from(length).map_err(|_| ReadError::Unreadable)?];
        let in_buffer =
            matches!(self.blocks.get(&memory).map(|block| block.backing), Some(Backing::Buffer { .. }));
        let read = if in_buffer {
            self.cpu_view(memory).is_ok_and(|view| view.read(offset, &mut bytes))
        } else {
            host.read_guest_span(space, address, &mut bytes) == Some(bytes.len())
        };
        if read { Ok(bytes) } else { Err(ReadError::Unreadable) }
    }

    /// Sends prepared work. A refusal for now keeps it for the next ring of the same doorbell.
    pub fn ring_doorbell(&mut self, doorbell: u64) -> Result<Doorbell, BackendError> {
        let Some(prepared) = self.prepared.get(&doorbell) else {
            return Err(BackendError::new("a doorbell for work that isn't prepared"));
        };
        let Some(space) = self.queues.get(&prepared.queue).map(|queue| queue.space) else {
            self.prepared.remove(&doorbell);
            return Err(BackendError::new("a doorbell for a queue that has gone"));
        };
        // A ring's VM signals nothing, so its binds are waited for here rather than by xe.
        if prepared.run.is_none() {
            let ring = self.spaces.get(&space).and_then(|space| space.ring.as_ref());
            if ring.is_some_and(|ring| !ring.binds_in_place()) {
                return Ok(Doorbell::NotYet);
            }
        }
        let attempted = prepared.run.or(prepared.ring_run);
        let sent = self.xe.exec(prepared.xe_queue, prepared.batch, &prepared.syncs);
        match sent.as_ref().map_err(plan::exec_verdict) {
            Ok(()) => {
                let prepared = self.prepared.remove(&doorbell).expect("found above");
                let changes_before = self.spaces.get(&space).map_or(0, |held| held.changes_made);
                if let Some(queue) = self.queues.get_mut(&prepared.queue) {
                    let stamped = |run: Run| Run { sent: Some(Instant::now()), changes_before, ..run };
                    if let Some(run) = prepared.run {
                        queue.in_flight.push_back(stamped(run));
                    }
                    if let Some(run) = prepared.ring_run {
                        queue.ring_run = Some(stamped(run));
                        queue.ring_banned_seen = false;
                    }
                }
                Ok(Doorbell::Rung)
            }
            Err(ExecVerdict::NotYet) => Ok(Doorbell::NotYet),
            Err(verdict) => {
                let error = sent.expect_err("matched as an error");
                let prepared_queue = self.prepared.get(&doorbell).map_or(0, |prepared| prepared.queue);
                self.prepared.remove(&doorbell);
                if verdict == ExecVerdict::Lost {
                    self.explain_cancel(prepared_queue, space, attempted);
                    self.fail_space(
                        space,
                        "xe cancelled a submission, as it does once a queue is reset or banned",
                    );
                } else {
                    log::error!(
                        "intel work: xe refused a submission, which is this backend's fault: {error}"
                    );
                }
                Err(BackendError::new(format!("xe refused the submission: {error}")))
            }
        }
    }

    /// Looks at runs on the card for ones xe ended early: it stands in at the fence of each, and a
    /// queue xe banned takes its whole process's work with it.
    pub fn poll(&mut self, host: &mut dyn HostServices, now: Instant) {
        if self
            .last_look
            .is_some_and(|last| now.saturating_duration_since(last) < choices::LOOK_AT_RUNS_EVERY)
        {
            return;
        }
        self.last_look = Some(now);
        let ids: Vec<u32> = self.queues.keys().copied().collect();
        for id in ids {
            self.look_at_runs(host, id, now);
            self.look_at_ring(host, id, now);
        }
    }

    fn look_at_runs(&mut self, host: &mut dyn HostServices, id: u32, now: Instant) {
        let Some(queue) = self.queues.get_mut(&id) else { return };
        let Some((xe_queue, done)) = queue.ordinary else { return };
        let space = queue.space;
        // The first run that ended without its fence, which is the one xe gave up on, and how many
        // after it were lost with it.
        let mut cut_short: Option<Run> = None;
        let mut lost_with_it = 0;
        while let Some(run) = queue.in_flight.front().copied() {
            match self.xe.point_reached(done, run.point, Duration::ZERO) {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) => {
                    log::warn!("intel work: queue {id:#x}: asking xe whether a run ended failed: {error}");
                    break;
                }
            }
            queue.in_flight.pop_front();
            if stand_in(host, id, run.fence, run.value) {
                match cut_short {
                    None => cut_short = Some(run),
                    Some(_) => lost_with_it += 1,
                }
            }
        }
        // A run that ended without its fence was cut short. Whether the queue was banned for it
        // decides if the process goes on; a question xe won't answer counts as banned, as in Mesa.
        if let Some(run) = cut_short
            && self.xe.queue_banned(xe_queue).unwrap_or(true)
        {
            let behind = self.queues.get(&id).map_or(0, |queue| queue.in_flight.len());
            let lines = self.ban_lines(host, id, &run, lost_with_it, behind, now);
            for line in lines {
                log::warn!("{line}");
            }
            self.fail_space(space, "xe banned one of its queues after a hang or a fault");
        }
    }

    fn look_at_ring(&mut self, host: &mut dyn HostServices, id: u32, now: Instant) {
        let Some(queue) = self.queues.get(&id) else { return };
        let (Some(xe_queue), space) = (queue.ring, queue.space) else { return };
        if queue.ring_banned_seen || !self.xe.queue_banned(xe_queue).unwrap_or(false) {
            return;
        }
        let (fence, run) = (queue.ring_fence, queue.ring_run);
        if let Some(queue) = self.queues.get_mut(&id) {
            queue.ring_banned_seen = true;
        }
        if let Some((fence, value)) = fence {
            stand_in(host, id, fence, value);
        }
        if let Some(run) = run {
            let lines = self.ban_lines(host, id, &run, 0, 0, now);
            for line in lines {
                log::warn!("{line}");
            }
        }
        self.fail_space(space, "xe banned the queue its ring ran on");
    }

    /// Destroys what backs a queue. Work prepared for it and not yet sent goes with it.
    pub fn close_queue(&mut self, queue: u32) {
        self.prepared.retain(|_, prepared| prepared.queue != queue);
        let Some(gone) = self.queues.remove(&queue) else { return };
        let (ordinary, done) = gone.ordinary.unzip();
        for xe_queue in [ordinary, gone.ring].into_iter().flatten() {
            if let Err(error) = self.xe.destroy_queue(xe_queue) {
                log::warn!("intel work: destroying queue {queue:#x}'s xe queue failed: {error}");
            }
        }
        if let Some(done) = done
            && let Err(error) = self.xe.destroy_syncobj(done)
        {
            log::warn!("intel work: destroying queue {queue:#x}'s sync object failed: {error}");
        }
    }

    pub fn live_queues(&self) -> usize {
        self.queues.len()
    }
}

/// Writes a fence the card never wrote, for a run that will never get there. Says whether it had to.
fn stand_in(host: &mut dyn HostServices, queue: u32, fence: u64, value: u64) -> bool {
    match host.fence_value(fence) {
        Ok(reached) if reached >= value => false,
        Ok(_) => {
            log::warn!(
                "intel work: queue {queue:#x}: xe ended a run before fence value {value:#x} was written, so \
                 the host writes it"
            );
            if let Err(error) = host.write_u64(fence, value) {
                log::warn!("intel work: standing in for a lost run: {error}");
            }
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::command_header::tests::compact as compact_header;
    use crate::stage::Stage;
    use crate::work::testing::Rig;
    use crate::xe::abi;
    use crate::xe::fake::Call;

    /// A space with a batch of one store, then the end, and the fence block: [`Rig::new`].
    fn ready(stage: Stage) -> Rig {
        let mut rig = Rig::new(stage);
        let mut dwords = batch::store_qword(rig.data_address() + 0x40, 0xC0FFEE).to_vec();
        dwords.push(batch::BATCH_END);
        rig.write_batch(&dwords);
        rig
    }

    #[test]
    fn a_batch_that_ends_runs_and_the_card_writes_the_guests_fence() {
        let mut rig = ready(Stage::Everything);
        let work = rig.submission(1, 7);
        let start = rig.start(&work);
        let WorkStart::Started { doorbell, work: 1 } = start else { panic!("{start:?}") };
        assert!(rig.xe.calls_named("exec").is_empty(), "nothing is sent before the doorbell");
        assert_eq!(rig.work.ring_doorbell(doorbell).unwrap(), Doorbell::Rung);
        let Some(Call::Exec { batch, syncs, .. }) = rig.xe.calls_named("exec").pop() else { panic!() };
        assert_eq!(batch, rig.batch_address());
        // The last bind is waited for, the guest's fence is the user fence, and the queue's own
        // timeline is signalled.
        assert!(matches!(syncs[0], Sync::After { .. }));
        assert_eq!(syncs[1], Sync::UserFence { address: rig.fence_gpu_address(), value: 7 });
        assert!(matches!(syncs[2], Sync::Signal { point: 1, .. }));
        assert_eq!(rig.fence(), 7, "written by the card, not by the host");
        assert_eq!(rig.data_qword(0x40), 0xC0FFEE);
        rig.poll();
        assert!(!rig.work.space_failed(rig.space));
        assert!(rig.host.written.is_empty(), "the host stood in for nothing");
    }

    #[test]
    fn a_queue_gets_one_xe_queue_on_its_nodes_engine_at_its_first_work() {
        let mut rig = ready(Stage::Everything);
        for value in 1..=3 {
            let work = rig.submission(1, value);
            let WorkStart::Started { doorbell, .. } = rig.start(&work) else { panic!() };
            rig.work.ring_doorbell(doorbell).unwrap();
        }
        let made = rig.xe.calls_named("create_queue");
        assert_eq!(made.len(), 1);
        let Call::CreateQueue { engine, .. } = &made[0] else { panic!() };
        assert_eq!(engine.class, crate::xe_query::Engine::COMPUTE, "node 5");
        assert_eq!(rig.fence(), 3);
        rig.work.close_queue(1);
        assert_eq!(rig.work.live_queues(), 0);
        assert_eq!(rig.xe.calls_named("destroy_queue").len(), 1);
    }

    #[test]
    fn work_that_may_not_run_or_that_the_stage_leaves_out_is_finished_unrun() {
        let mut rig = ready(Stage::Compute);
        let mut work = rig.submission(1, 1);
        work.may_run = false;
        assert_eq!(rig.start(&work), WorkStart::Finished);
        let mut work = rig.submission(1, 1);
        work.queue_failed = true;
        assert_eq!(rig.start(&work), WorkStart::Finished);
        // Direct3D 12's context type, at the stage that runs compute only.
        let work = rig.submission(1, 1).context_type(7);
        assert_eq!(rig.start(&work), WorkStart::Finished);
        // A context's submission names no commands yet.
        let mut work = rig.submission(1, 1);
        work.commands = None;
        assert_eq!(rig.start(&work), WorkStart::Finished);
        assert!(rig.xe.calls_named("create_queue").is_empty() && rig.xe.calls_named("exec").is_empty());
        let work = rig.submission(1, 1);
        assert!(matches!(rig.start(&work), WorkStart::Started { .. }), "OpenCL's does run");
    }

    #[test]
    fn a_submission_the_kernel_driver_would_refuse_stops_its_queue() {
        let mut rig = ready(Stage::Everything);
        let mut work = rig.submission(1, 1);
        work.private.truncate(100);
        assert_eq!(rig.start(&work), WorkStart::QueueFailed, "no header's size");
        let work = rig.submission(1, 1).patch_list();
        assert_eq!(rig.start(&work), WorkStart::QueueFailed);
        let mut work = rig.submission(1, 1);
        work.node = 9;
        assert_eq!(rig.start(&work), WorkStart::QueueFailed, "a node with no engine");
        assert!(rig.xe.calls_named("exec").is_empty());
    }

    #[test]
    fn a_batch_that_is_not_mapped_or_does_not_end_is_refused_before_xe_sees_it() {
        let mut rig = ready(Stage::Everything);
        let mut work = rig.submission(1, 1);
        work.commands = Some(CommandBuffer { address: 0x9_0000_0000, length: 64 });
        assert_eq!(rig.start(&work), WorkStart::QueueFailed, "nothing is mapped there");
        let mut work = rig.submission(1, 1);
        work.commands = Some(CommandBuffer { address: rig.batch_address() + 0x1000 - 8, length: 64 });
        assert_eq!(rig.start(&work), WorkStart::QueueFailed, "it runs past its mapping's end");
        rig.write_batch(&[0x7A00_0004, 0x1234, 0x5678]);
        let work = rig.submission(1, 1);
        assert_eq!(rig.start(&work), WorkStart::QueueFailed, "no end");
        assert!(rig.xe.calls_named("create_queue").is_empty());
    }

    #[test]
    fn a_batch_in_guest_pages_is_read_through_the_core() {
        let mut rig = ready(Stage::Everything);
        // The data pages are guest pages: put a batch there.
        let dwords = [batch::NOOP, batch::BATCH_END];
        rig.write_data(0x800, &batch::bytes(&dwords));
        let mut work = rig.submission(1, 1);
        work.commands = Some(CommandBuffer { address: rig.data_address() + 0x800, length: 8 });
        assert!(matches!(rig.start(&work), WorkStart::Started { .. }));
        rig.write_data(0x800, &batch::bytes(&[batch::NOOP, batch::NOOP]));
        assert_eq!(rig.start(&work), WorkStart::QueueFailed);
    }

    #[test]
    fn a_full_queue_is_not_yet_and_the_same_doorbell_rings_again() {
        let mut rig = ready(Stage::Everything);
        let work = rig.submission(1, 4);
        let WorkStart::Started { doorbell, .. } = rig.start(&work) else { panic!() };
        rig.xe.fail("exec", libc::EAGAIN);
        assert_eq!(rig.work.ring_doorbell(doorbell).unwrap(), Doorbell::NotYet);
        assert_eq!(rig.fence(), 0);
        rig.xe.heal();
        assert_eq!(rig.work.ring_doorbell(doorbell).unwrap(), Doorbell::Rung);
        assert_eq!(rig.fence(), 4);
        assert!(rig.work.ring_doorbell(doorbell).is_err(), "sent once");
    }

    #[test]
    fn a_cancelled_submission_fails_its_process_and_another_refusal_only_the_call() {
        let mut rig = ready(Stage::Everything);
        let work = rig.submission(1, 1);
        let WorkStart::Started { doorbell, .. } = rig.start(&work) else { panic!() };
        rig.xe.fail("exec", libc::EINVAL);
        assert!(rig.work.ring_doorbell(doorbell).is_err());
        assert!(!rig.work.space_failed(rig.space), "a refusal that is our own fault bans nothing");
        let work = rig.submission(1, 2);
        let WorkStart::Started { doorbell, .. } = rig.start(&work) else { panic!() };
        rig.xe.fail("exec", libc::ECANCELED);
        assert!(rig.work.ring_doorbell(doorbell).is_err());
        assert!(rig.work.space_failed(rig.space));
        rig.xe.heal();
        let work = rig.submission(1, 3);
        assert_eq!(rig.start(&work), WorkStart::QueueFailed, "nothing more of this process runs");
    }

    #[test]
    fn a_run_xe_ended_early_is_stood_in_for_and_a_banned_queue_fails_only_its_own_process() {
        let mut rig = ready(Stage::Everything);
        let other = rig.second_space();
        rig.xe.hold_runs();
        let work = rig.submission(1, 9);
        let WorkStart::Started { doorbell, .. } = rig.start(&work) else { panic!() };
        rig.work.ring_doorbell(doorbell).unwrap();
        rig.poll();
        assert!(rig.host.written.is_empty(), "still running: nothing to stand in for");
        rig.xe.lose_held_runs();
        rig.poll();
        assert_eq!(rig.host.written, [(rig.fence_guest_address(), 9)], "the host wrote the fence");
        assert_eq!(rig.fence(), 9);
        assert!(rig.work.space_failed(rig.space));
        let work = rig.submission(1, 10);
        assert_eq!(rig.start(&work), WorkStart::QueueFailed);
        // Another process of the same guest goes on.
        assert!(!rig.work.space_failed(other));
        let work = rig.submission_in(other, 2, 1);
        let WorkStart::Started { doorbell, .. } = rig.start(&work) else { panic!() };
        assert_eq!(rig.work.ring_doorbell(doorbell).unwrap(), Doorbell::Rung);
    }

    #[test]
    fn a_batch_that_faults_is_found_the_same_way() {
        let mut rig = Rig::new(Stage::Everything);
        let mut dwords = batch::store_qword(0x9_0000_0000, 1).to_vec();
        dwords.push(batch::BATCH_END);
        rig.write_batch(&dwords);
        let work = rig.submission(1, 5);
        let WorkStart::Started { doorbell, .. } = rig.start(&work) else { panic!() };
        rig.work.ring_doorbell(doorbell).unwrap();
        assert_eq!(rig.fence(), 0, "the card wrote no fence");
        rig.poll();
        assert_eq!(rig.fence(), 5);
        assert!(rig.work.space_failed(rig.space));
        assert_eq!(rig.xe.faults(), [0x9_0000_0000]);
    }

    #[test]
    fn a_fence_that_is_not_mapped_is_written_by_the_host_when_the_run_ends() {
        let mut rig = ready(Stage::Everything);
        let mut work = rig.submission(1, 6);
        work.fence_gpu_address = 0x8_0000_0000;
        let WorkStart::Started { doorbell, .. } = rig.start(&work) else { panic!() };
        rig.work.ring_doorbell(doorbell).unwrap();
        let Some(Call::Exec { syncs, .. }) = rig.xe.calls_named("exec").pop() else { panic!() };
        assert!(!syncs.iter().any(|sync| matches!(sync, Sync::UserFence { .. })));
        rig.poll();
        assert_eq!(rig.fence(), 6);
        assert!(!rig.work.space_failed(rig.space), "the run went through: nothing was banned");
    }

    #[test]
    fn runs_are_looked_at_no_more_often_than_every_fifty_milliseconds() {
        let mut rig = ready(Stage::Everything);
        let work = rig.submission(1, 1);
        let WorkStart::Started { doorbell, .. } = rig.start(&work) else { panic!() };
        rig.xe.hold_runs();
        rig.work.ring_doorbell(doorbell).unwrap();
        let now = Instant::now();
        rig.work.poll(&mut rig.host, now);
        rig.work.poll(&mut rig.host, now + Duration::from_millis(10));
        assert_eq!(rig.xe.calls_named("point_reached").len(), 1);
        rig.work.poll(&mut rig.host, now + Duration::from_millis(60));
        assert_eq!(rig.xe.calls_named("point_reached").len(), 2);
    }

    /// A ring as Intel's compute driver submits it: work, then a semaphore wait, and no end.
    fn ring(rig: &mut Rig) {
        let mut dwords = batch::store_qword(rig.data_address() + 0x40, 0xBEEF).to_vec();
        dwords.extend([batch::SEMAPHORE_WAIT_HEADER | 1 << 15 | 1 << 12, 1, 0x4000_1000, 0, 0]);
        rig.write_batch(&dwords);
    }

    #[test]
    fn a_ring_runs_on_a_long_running_queue_with_no_fence_and_no_signal() {
        let mut rig = Rig::new(Stage::Everything);
        ring(&mut rig);
        let work = rig.submission(1, 3);
        let WorkStart::Started { doorbell, .. } = rig.start(&work) else { panic!() };
        assert_eq!(rig.work.ring_doorbell(doorbell).unwrap(), Doorbell::Rung);
        let Some(Call::Exec { queue, syncs, .. }) = rig.xe.calls_named("exec").pop() else { panic!() };
        assert!(syncs.is_empty(), "the ring writes the fence itself: {syncs:?}");
        let Some(Call::CreateQueue { vm, .. }) = rig.xe.calls_named("create_queue").pop() else { panic!() };
        assert_eq!(rig.xe.vm_flags(vm), Some(abi::VM_LONG_RUNNING));
        assert_eq!(rig.data_qword(0x40), 0xBEEF, "the work before the wait ran");
        rig.poll();
        assert!(!rig.work.space_failed(rig.space) && rig.host.written.is_empty());
        // xe bans the ring's queue: the host stands in at the fence and the process stops.
        rig.xe.ban(queue);
        rig.poll_later();
        assert_eq!(rig.host.written, [(rig.fence_guest_address(), 3)]);
        assert!(rig.work.space_failed(rig.space));
    }

    #[test]
    fn a_queue_can_carry_a_ring_and_batches_that_end_side_by_side() {
        let mut rig = Rig::new(Stage::Everything);
        ring(&mut rig);
        let work = rig.submission(1, 1);
        let WorkStart::Started { doorbell, .. } = rig.start(&work) else { panic!() };
        rig.work.ring_doorbell(doorbell).unwrap();
        rig.write_batch(&[batch::BATCH_END]);
        let work = rig.submission(1, 2);
        let WorkStart::Started { doorbell, .. } = rig.start(&work) else { panic!() };
        rig.work.ring_doorbell(doorbell).unwrap();
        assert_eq!(rig.fence(), 2);
        let vms: Vec<u32> = rig
            .xe
            .calls_named("create_queue")
            .iter()
            .map(|call| match call {
                Call::CreateQueue { vm, .. } => *vm,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(vms.len(), 2);
        assert_ne!(vms[0], vms[1], "one in each of the process's two address spaces");
        rig.work.close_queue(1);
        assert_eq!(rig.xe.calls_named("destroy_queue").len(), 2);
    }

    #[test]
    fn the_header_used_in_these_tests_is_a_compact_one() {
        assert_eq!(Header::read(&compact_header()).verdict, Length::Compact);
    }
}
