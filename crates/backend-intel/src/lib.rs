//! The Intel backend: the host's open xe driver, and the private data Intel's Windows driver passes
//! to its kernel driver.
//!
//! It reads the card through sysfs and xe's read-only queries, builds the adapter record the
//! guest's user-mode drivers ask for, and answers the guest so that they can make devices, contexts,
//! queues and allocations and submit. Every exchange that is Intel's own gets a line in the log, in
//! [`traffic`].
//!
//! What becomes of the guest's work is the [`stage`]'s word. While listening, nothing reaches the
//! card: a submission comes back finished, unrun, and video memory is only a name. Past that,
//! [`work`] makes real buffers, address spaces and queues through [`xe`] and runs the guest's
//! batches. That has run against a stand-in for xe only, never on a card, so the backend tests the
//! card itself at each start and stays listening if any of it fails.
//!
//! The guest-protocol side, in [`protocol`], is plain byte work tested without a guest. The host
//! side is [`card`] and the modules under it, and `intel-probe` prints what it reads.
//!
//! Where the trait does not fit Intel:
//! - A submission's batch address and length are in the submission message, not in Intel's private
//!   data. [`Work`] carries them for a hardware queue's submission, and not yet for a context's.
//! - Escapes have no status of their own, so one this backend's rules refuse can only be logged.
//! - [`Backend::finish_queue_data`] sees the queue's data with the message's padding after it, so
//!   its true size can't be checked.
//! - The core describes every node but the first as a copy engine, whatever gmmlib's numbering
//!   says the node is.

// The shared test helpers name this crate as a test outside it does.
#[cfg(test)]
extern crate self as guesthouse_backend_intel;

mod books;
pub mod card;
pub mod generation;
pub mod hwconfig;
pub mod ioctl;
pub mod protocol;
pub mod recording;
pub mod stage;
pub mod sysfs;
mod traffic;
mod watch;
pub mod work;
pub mod xe;
pub mod xe_query;

use books::Books;
use card::CardFacts;
use generation::Generation;
use guesthouse_backend::guest_text::QUERY_USER_DRIVER_NAME;
use guesthouse_backend::{
    AdapterFacts, AllocationShape, Backend, BackendError, COPY_PIXEL_BYTES, CardLabel, Doorbell, DriverFound,
    DriverSurvey, GpuRange, GuestBooks, HostServices, MapRequest, MemoryId, MemoryKind, PciCard, Refusal,
    RegistryName, RegistryValue, Setup, SlotCopy, SpaceId, StagingCopy, StagingStart, Status, UnmapRequest,
    VendorQuery, Work, WorkStart,
};
pub use ioctl::{INTEL_VENDOR, intel_render_node};
use protocol::allocation::Resource;
use protocol::command_header::Header;
use protocol::context::Context;
use protocol::escape::Escape;
use protocol::record::{EngineMaxima, GtSystem, IpWord, Memory, Platform, RecordFacts, StageThreads};
use protocol::{gaps, guest_driver, record, shape};
use stage::{Desktop, Fallback, Stage};
use std::os::fd::OwnedFd;
use std::time::Instant;
use traffic::{QueryOutcome, Traffic};
use watch::PREFIX;
use work::CardWork;
use work::copies::Rows;
use xe::device::Xe;
use xe::plan::BufferUse;
use xe_query::{Engine, Gt};

/// How this backend names the host driver's branch: the Linux kernel's xe.
const HOST_BRANCH: &str = "xe";
/// The part of the system's memory offered as shared memory, as Windows gives a card half of the
/// memory as shared. An integrated card has no other, so for it this is the video memory too.
const SHARED_MEMORY_PART: u64 = 2;

/// See the crate documentation.
#[derive(Debug)]
pub struct IntelBackend {
    card: CardFacts,
    generation: &'static Generation,
    facts: AdapterFacts,
    record: Vec<u8>,
    books: Books,
    traffic: Traffic,
    started: Instant,
    /// See [`IntelBackend::set_desktop`].
    desktop: Desktop,
    /// Work on the card, at every stage past listening. `None` is the listening stage: memory and
    /// spaces are [`Books`]' names, and every submission is finished unrun.
    work: Option<CardWork>,
}

impl IntelBackend {
    /// The backend for a card whose facts are already read, which is how a test makes one. Refuses
    /// a card whose graphics has no profile.
    pub fn from_card(card: CardFacts) -> Result<Self, Refusal> {
        let ip = card.main_gt().map(|gt| gt.ip).unwrap_or_default();
        let generation = generation::for_card(ip, card.pci.device).ok_or_else(|| {
            Refusal(format!(
                "This Intel card, device {:04x} with graphics IP {ip}, is of a generation this build has no \
                 profile for. Run `intel-probe --record card.txt` and send card.txt with the probe's output, so \
                 one can be made.",
                card.pci.device
            ))
        })?;
        // compute-runtime works out threads per EU by dividing the thread count by the EU count, so a
        // record with none would stop it at the start, or worse.
        if card.eu_count() == 0 {
            return Err(Refusal(
                "The xe driver reported no EUs for this Intel card, so the guest can't be told its shape."
                    .into(),
            ));
        }
        let shared = card.system_memory() / SHARED_MEMORY_PART;
        let record = record::build(&record_facts(&card, generation, shared));
        // A card with memory of its own offers that; one without offers its share of the system's.
        let own = card.card_memory();
        let video_memory = if own > 0 { own } else { shared };
        let reachable = card.reachable_card_memory();
        let bar_size = if reachable > 0 { reachable } else { video_memory };
        let facts = AdapterFacts {
            card: PciCard { vendor: INTEL_VENDOR, device: card.pci.device, revision: card.pci.revision },
            name: adapter_name(&card),
            video_memory,
            gtt_memory: 0,
            bar_size,
            node_count: protocol::nodes::COUNT,
            engines: card.engine_families(),
            guest_driver_version: 0,
        };
        let backend = Self {
            card,
            generation,
            facts,
            record,
            books: Books::default(),
            traffic: Traffic::default(),
            started: Instant::now(),
            desktop: Desktop::Hardware,
            work: None,
        };
        backend.log_start();
        Ok(backend)
    }

    /// Where the guest's desktop is drawn, which decides the drivers each program is given: see
    /// [`stage`]. The daemon sets the stage's own unless a person asks otherwise.
    pub fn set_desktop(&mut self, desktop: Desktop) {
        self.desktop = desktop;
        match desktop {
            Desktop::Software => log::info!(
                "{PREFIX} software-desktop on: Direct3D 9, 10 and 11 drivers refused, and every driver to the compositor"
            ),
            Desktop::Hardware => log::info!("{PREFIX} software-desktop off: every driver given"),
        }
    }

    pub fn card(&self) -> &CardFacts {
        &self.card
    }

    pub fn generation(&self) -> &'static Generation {
        self.generation
    }

    /// Blocks of memory still held, for a test to see that frees reach the backend.
    pub fn live_memory_blocks(&self) -> usize {
        self.work.as_ref().map_or_else(|| self.books.live_memory(), CardWork::live_blocks)
    }

    /// The adapter record the guest is given, before its build stamp is put in.
    pub fn record(&self) -> &[u8] {
        &self.record
    }

    fn log_start(&self) {
        let card = &self.card;
        log::info!(
            "{PREFIX} card pci={:04x}:{:04x} rev={:#04x} subsystem={:04x}:{:04x} address={} driver={} kernel={} \
             profile=\"{}\" render_ip={} eus={} system_memory={:#x} card_memory={:#x}",
            card.pci.vendor,
            card.pci.device,
            card.pci.revision,
            card.pci.subsystem_vendor,
            card.pci.subsystem_device,
            card.address.as_deref().unwrap_or("-"),
            card.driver,
            card.kernel.as_deref().unwrap_or("-"),
            self.generation.name,
            card.main_gt().map(|gt| gt.ip).unwrap_or_default(),
            card.eu_count(),
            card.system_memory(),
            card.card_memory()
        );
        for (what, error) in &card.unanswered {
            log::warn!("{PREFIX} card query=\"{what}\" unanswered=\"{error}\"");
        }
        for guess in self.generation.guesses {
            log::info!("{PREFIX} profile-guess \"{guess}\"");
        }
        log::info!("{PREFIX} record {}", record::summary(&self.record));
        log::info!("{PREFIX} record-partition [{}]", protocol::partition::Text(protocol::partition::ours()));
        for gap in gaps::unset(shape::filled(&self.record)) {
            log::info!("{PREFIX} record-gap \"{gap}\"");
        }
        if self.card.config.has_vram() {
            log::info!("{PREFIX} record-gap \"{}\"", gaps::LOCAL_MEMORY);
        }
    }

    /// How much of the guest's work runs on the card.
    pub fn stage(&self) -> Stage {
        self.work.as_ref().map_or(Stage::Listen, CardWork::stage)
    }

    /// Starts work on the card at the stage asked for, or stays listening and says in the log which
    /// layer refused. Nothing here fails the guest's start: a card that can't run work is still
    /// one whose guest can be listened to. `open` opens the card for work, and `sysfs` is the
    /// card's device folder.
    pub fn start_work_on_card(
        &mut self,
        asked: (Stage, Option<Fallback>),
        open: impl FnOnce() -> std::io::Result<Box<dyn Xe + Send>>,
        sysfs: Option<&std::path::Path>,
    ) {
        match self.work_at(asked, open, sysfs) {
            Ok(work) => {
                log::info!(
                    "{PREFIX} stage={:?}: the guest's work runs on the card, with at most {} MiB of video \
                     memory; {}=0 turns that off",
                    work.stage(),
                    work.cap() >> 20,
                    stage::RUN_VARIABLE
                );
                self.work = Some(work);
            }
            Err(fallback) => {
                self.work = None;
                log::warn!(
                    "{PREFIX} stage=Listen {fallback}. Nothing reaches the card: the guest's work is \
                     finished unrun"
                );
            }
        }
    }

    fn work_at(
        &mut self,
        (asked, why): (Stage, Option<Fallback>),
        open: impl FnOnce() -> std::io::Result<Box<dyn Xe + Send>>,
        sysfs: Option<&std::path::Path>,
    ) -> Result<CardWork, Fallback> {
        if !asked.is_real() {
            return Err(why.unwrap_or(Fallback::AskedFor));
        }
        if self.card.driver != ioctl::XE_DRIVER {
            return Err(Fallback::NotXe(self.card.driver.clone()));
        }
        let mut xe = open().map_err(|error| Fallback::NodeNotOpened(error.to_string()))?;
        // The same test `intel-probe --run` runs, without its two break steps: if this card and
        // kernel don't do what work on the card needs, no guest finds out the hard way.
        let mut said = Vec::new();
        let summary = xe::selftest::run(
            &mut *xe,
            &self.card,
            sysfs,
            xe::selftest::Options::default(),
            &mut std::io::empty(),
            &mut said,
        );
        for line in String::from_utf8_lossy(&said).lines() {
            if line.starts_with("FAIL") {
                log::warn!("{PREFIX} self-test {line}");
            } else {
                log::info!("{PREFIX} self-test {line}");
            }
        }
        if !summary.all_passed() {
            return Err(Fallback::SelfTestFailed(summary.failed));
        }
        let shared = self.card.system_memory() / SHARED_MEMORY_PART;
        let cap = work::cap::video_cap(&self.card.regions, shared);
        if cap < work::cap::LEAST_FOR_A_GUEST {
            return Err(Fallback::TooLittleMemory(cap));
        }
        CardWork::new(xe, &self.card, asked, cap).ok_or(Fallback::NoMemoryRegion)
    }
}

/// The facts of a card recorded elsewhere, for a lab run on a PC without it. The guest must have
/// been given the recorded card's address, so a machine made for another card can't start on it
/// unnoticed. The log says loudly that nothing here is a card.
fn recorded_card(file: &std::path::Path, address: Option<&str>) -> Result<CardFacts, Refusal> {
    let refused = |why: String| Refusal(format!("The recorded Intel card in {} {why}", file.display()));
    let text =
        std::fs::read_to_string(file).map_err(|error| refused(format!("could not be read: {error}")))?;
    let card = recording::read(&text).map_err(|bad| refused(format!("is damaged: {bad}")))?;
    let recorded = card.address.as_deref().unwrap_or("-");
    if address.is_some_and(|wanted| wanted != recorded) {
        return Err(refused(format!(
            "is of the card at {recorded}, and this guest was given the one at {}.",
            address.unwrap_or_default()
        )));
    }
    log::warn!(
        "{PREFIX} RECORDED CARD: no Intel card is opened. The facts are a recording, from {} ({}), named in \
         {}. A lab run only: nothing the guest sends reaches a card.",
        file.display(),
        recorded,
        recording::RECORDED_CARD_VARIABLE
    );
    Ok(card)
}

/// What a recorded card can be asked for: listening, whatever a person asked, as there is no card
/// to run on. A person's own request to listen keeps its own reason.
fn on_a_recorded_card((asked, why): (Stage, Option<Fallback>)) -> (Stage, Option<Fallback>) {
    (Stage::Listen, if asked.is_real() { Some(Fallback::RecordedCard) } else { why })
}

/// The opener for a card that isn't there.
fn no_card() -> std::io::Result<Box<dyn Xe + Send>> {
    Err(std::io::Error::other("there is no card"))
}

/// A copy's result as the guest is told it, with the reason in the log.
fn copied(result: Result<(), BackendError>) -> Status {
    match result {
        Ok(()) => Status::SUCCESS,
        Err(error) => {
            log::warn!("{PREFIX} a frame wasn't copied: {error}");
            Status::UNSUCCESSFUL
        }
    }
}

/// The adapter's name, as the guest's programs see it in the adapter's description and the record.
fn adapter_name(card: &CardFacts) -> String {
    format!("Intel GPU {:#06x}", card.pci.device)
}

/// What the record is built from: the card where it can say, and the generation where it can't.
fn record_facts(card: &CardFacts, generation: &Generation, shared: u64) -> RecordFacts {
    let ip = |gt: Option<&Gt>| {
        gt.map_or(IpWord::default(), |gt| IpWord::new(gt.ip.major, gt.ip.minor, gt.ip.revision))
    };
    let config = |key| card.hwconfig.value(key).unwrap_or(0);
    let threads_per_eu =
        card.hwconfig.value(hwconfig::NUM_THREADS_PER_EU).unwrap_or(generation.threads_per_eu);
    let most_dual_subslices = card.hwconfig.most_cores().unwrap_or(0);
    // The counts are of the cores the slice arrays can hold, so the arrays and the counts agree.
    let cores = shape::representable(&card.cores(), config(hwconfig::MAX_SLICES_SUPPORTED));
    let (eus, dual_subslices, slices) = if cores.is_empty() {
        (card.eu_count(), card.dual_subslices(), card.slices().unwrap_or(0))
    } else {
        let eus = cores.iter().fold(0u32, |sum, core| sum.saturating_add(core.eu_count));
        (eus, cores.len() as u32, shape::slices_in(&cores))
    };
    RecordFacts {
        platform: Platform {
            product_family: generation.product_family,
            pch_family: generation.pch_family,
            display_core: generation.display_core,
            render_core: generation.render_core,
            platform_type: generation.platform_type,
            device_id: card.pci.device,
            revision: card.pci.revision.into(),
            gt_type: generation.gt_type,
            render_ip: ip(card.main_gt()),
        },
        gt: GtSystem {
            eu_count: eus,
            thread_count: eus.saturating_mul(threads_per_eu),
            slice_count: slices,
            subslice_count: dual_subslices,
            dual_subslice_count: dual_subslices,
            max_eu_per_subslice: card.hwconfig.most_eus_per_core().unwrap_or(0),
            max_slices: config(hwconfig::MAX_SLICES_SUPPORTED),
            max_subslices: most_dual_subslices,
            max_dual_subslices: most_dual_subslices,
            threads_per_eu,
            stage_threads: StageThreads {
                vertex: config(hwconfig::TOTAL_VS_THREADS),
                hull: config(hwconfig::TOTAL_HS_THREADS),
                domain: config(hwconfig::TOTAL_DS_THREADS),
                geometry: config(hwconfig::TOTAL_GS_THREADS),
                pixel: config(hwconfig::TOTAL_PS_THREADS),
                vertex_pocs: config(hwconfig::TOTAL_VS_THREADS_POCS),
            },
            // The hwconfig's most, or what the xe driver lists where the table doesn't say.
            engine_maxima: {
                let most = |key, class| card.hwconfig.value(key).unwrap_or_else(|| card.engines_of(class));
                EngineMaxima {
                    render: most(hwconfig::MAX_RCS, Engine::RENDER),
                    compute: most(hwconfig::MAX_CCS, Engine::COMPUTE),
                    video: most(hwconfig::MAX_VCS, Engine::VIDEO_DECODE),
                    video_enhance: most(hwconfig::MAX_VECS, Engine::VIDEO_ENHANCE),
                    copy: most(hwconfig::MAX_COPY_CS, Engine::COPY),
                }
            },
        },
        timestamp_hz: card.main_gt().map_or(0, |gt| gt.reference_clock),
        adapter_name: adapter_name(card),
        render_mhz: (card.frequencies.lowest.unwrap_or(0), card.frequencies.highest.unwrap_or(0)),
        memory: Memory { dedicated: card.card_memory(), system_shared: shared, system_video: 0 },
        local_memory: card.config.has_vram(),
        cores,
        compute_engines: card.engines_of(Engine::COMPUTE),
        copy_engine: card.engines_of(Engine::COPY) > 0,
        video_engines: card.instance_mask(Engine::VIDEO_DECODE),
        video_enhance_engines: card.instance_mask(Engine::VIDEO_ENHANCE),
        address_bits: card.config.va_bits,
        xe2_tiling: generation.xe2_tiling,
    }
}

impl Backend for IntelBackend {
    fn supports(card: &PciCard) -> bool {
        card.vendor == INTEL_VENDOR
    }

    /// The host is the Linux kernel's xe and the guest one release of Intel's driver. xe has no
    /// branches of its own: the host's side of that release's data is this backend, written for it,
    /// so the host is said to be on the guest release's branch. That is all that lets the pair
    /// through, and only for the release this backend knows. Nothing has been tested yet, so a start
    /// always warns.
    fn survey(setup: &Setup) -> Result<DriverSurvey, Refusal> {
        let version = setup.guest_driver_version.to_string();
        let known = guest_driver::GUEST_RELEASES.contains(&version.as_str());
        let kernel = sysfs::kernel_release().unwrap_or_else(|| "kernel".into());
        let host = format!("{HOST_BRANCH} {kernel}");
        let branch = if known { setup.guest_branch.clone() } else { HOST_BRANCH.into() };
        Ok(DriverSurvey {
            host: DriverFound { version: host, branch },
            guest: DriverFound { version, branch: setup.guest_branch.clone() },
            host_branch_known: true,
            guest_branch_known: known,
            host_version_tested: false,
            mixed_branches_tested: false,
            card: CardLabel::Experimental,
        })
    }

    fn open(setup: &Setup, _survey: &DriverSurvey) -> Result<Self, Refusal> {
        let address = setup.card_address.map(|address| address.to_string());
        let asked = Stage::from_environment();
        let mut backend = match recording::recorded_card_file() {
            Some(file) => {
                let mut backend = Self::from_card(recorded_card(&file, address.as_deref())?)?;
                backend.start_work_on_card(on_a_recorded_card(asked), no_card, None);
                backend
            }
            None => {
                let node = intel_render_node(address.as_deref()).ok_or_else(|| {
                    Refusal(
                        "No Intel card on the xe driver was found at the address this guest was given."
                            .into(),
                    )
                })?;
                let card = CardFacts::read(&node)
                    .map_err(|error| Refusal(format!("Opening the Intel card failed: {error}")))?;
                let mut backend = Self::from_card(card)?;
                let open = || xe::device::XeCard::open(&node.device_file).map(|card| Box::new(card) as _);
                backend.start_work_on_card(asked, open, Some(&node.sysfs));
                backend
            }
        };
        backend.facts.guest_driver_version = setup.guest_driver_version.packed();
        Ok(backend)
    }

    fn facts(&self) -> &AdapterFacts {
        &self.facts
    }

    fn allocate_video_memory(
        &mut self,
        size: u64,
        _kind: MemoryKind,
    ) -> Result<(MemoryId, u64), BackendError> {
        match &mut self.work {
            Some(work) => work.allocate(size, BufferUse::Card),
            None => Ok((self.books.new_memory(), size)),
        }
    }

    fn adopt_host_pages(&mut self, host_address: u64, size: u64) -> Result<MemoryId, BackendError> {
        match &mut self.work {
            Some(work) => work.adopt(host_address, size),
            None => Ok(self.books.new_memory()),
        }
    }

    fn release_memory(&mut self, memory: MemoryId) {
        match &mut self.work {
            Some(work) => work.release(memory),
            None => self.books.release(memory),
        }
    }

    /// By the CPU, through its view of both. While listening nothing has written the video memory,
    /// so there is nothing to move.
    fn preserve_contents(
        &mut self,
        _host: &mut dyn HostServices,
        _own_space: SpaceId,
        from: MemoryId,
        to: MemoryId,
        size: u64,
    ) -> Result<(), BackendError> {
        match &mut self.work {
            Some(work) => work.preserve(from, to, size),
            None => Ok(()),
        }
    }

    fn read_memory_start(
        &mut self,
        _host: &mut dyn HostServices,
        _own_space: SpaceId,
        memory: MemoryId,
        out: &mut [u8],
    ) -> Result<(), BackendError> {
        match &mut self.work {
            Some(work) => work.read_start(memory, out),
            None => Err(BackendError::new("nothing has written video memory at this stage")),
        }
    }

    /// A buffer of the card's, as a file a viewer on the same card can import.
    fn export_memory(&mut self, memory: MemoryId, _size: u64) -> Result<OwnedFd, BackendError> {
        match &mut self.work {
            Some(work) => work.export(memory),
            None => Err(BackendError::new("nothing is on the card to export at this stage")),
        }
    }

    /// Intel's drivers have the CPU lock nearly every allocation, the display driver's slots among
    /// them, so once work runs the slots are looked for in the guest's pages too.
    fn slots_may_be_guest_pages(&self) -> bool {
        self.work.is_some()
    }

    /// Once work runs on the card, what it can spare beside the host's own desktop.
    fn video_memory_cap(&self) -> Option<u64> {
        self.work.as_ref().map(CardWork::cap)
    }

    fn create_address_space(&mut self, ranges: &[GpuRange]) -> Result<SpaceId, BackendError> {
        match &mut self.work {
            Some(work) => work.create_space(ranges),
            None => Ok(self.books.create_space(ranges)),
        }
    }

    fn destroy_address_space(&mut self, space: SpaceId) {
        match &mut self.work {
            Some(work) => work.destroy_space(space),
            None => self.books.destroy_space(space),
        }
    }

    fn map(&mut self, request: &MapRequest) -> Result<(), BackendError> {
        match &mut self.work {
            Some(work) => work.map(request),
            None => self.books.map(request),
        }
    }

    fn unmap(&mut self, unmap: &UnmapRequest) -> Result<(), BackendError> {
        match &mut self.work {
            Some(work) => work.unmap(unmap),
            None => self.books.unmap(unmap),
        }
    }

    /// Data this backend refuses gets a page, as the call fails anyway once
    /// [`Backend::allocation_data_valid`] says so.
    fn describe_allocation(&mut self, private: &[u8]) -> AllocationShape {
        match Resource::parse(private) {
            Ok(resource) => {
                self.traffic.allocation(private, &resource);
                if !resource.size_is_sane() {
                    self.traffic.refused(
                        "allocation-size",
                        "past every GPU address, given one page",
                        private,
                    );
                }
                resource.shape()
            }
            Err(_) => Resource::default().shape(),
        }
    }

    fn allocation_data_valid(&mut self, private: &[u8]) -> bool {
        let refused = Resource::parse(private).err();
        if let Some(refused) = refused {
            self.traffic.refused("allocation", refused.0, private);
        }
        refused.is_none()
    }

    /// Nothing is written back into an allocation's data.
    fn finish_allocation_data(&mut self, _private: &mut [u8], _size: u64) {}

    fn guest_placed_gpu_ranges(&self) -> &'static [GpuRange] {
        protocol::partition::guest_placed(self.card.config.va_bits)
    }

    fn finish_queue_data(&mut self, queue: u32, private: &mut [u8], serial: u32) -> Result<(), BackendError> {
        self.traffic.queue(queue, serial, private);
        Ok(())
    }

    fn adapter_opened(&mut self, guest_adapter_id: u64) {
        log::info!("{PREFIX} adapter-opened guest_id={guest_adapter_id:#x}");
    }

    fn guest_restarted(&mut self) {
        log::info!("{PREFIX} guest-restarted");
    }

    /// A partition escape asking for one gets the record's. Every other is answered by leaving the
    /// guest's buffer as it came.
    fn escape(&mut self, process: u32, data: &mut [u8], _books: &GuestBooks<'_>) {
        let escape = Escape::parse(data);
        let partition = protocol::escape::answer_partition(data, &protocol::partition::ours());
        self.traffic.escape(process, data, &escape, partition);
    }

    fn adapter_answer(&mut self, query: &VendorQuery<'_>, out: &mut Vec<u8>) -> Option<Status> {
        let outcome = match query.kind {
            record::QUERY_TYPE => {
                let (status, asked) = record::answer(&self.record, query, out);
                QueryOutcome::Record(status, asked)
            }
            guest_driver::QUERY_OPENGL => {
                let driver = guest_driver::write_opengl_info(query, &self.generation.drivers, out);
                QueryOutcome::OpenGl(Status::SUCCESS, driver)
            }
            QUERY_USER_DRIVER_NAME => {
                let (status, name) = guest_driver::write_name(query, self.desktop, out);
                if let guest_driver::NameAnswer::Withheld(why) = name {
                    self.traffic.program_withheld(query.program, why);
                }
                QueryOutcome::DriverName(status, name)
            }
            _ => match protocol::wddm_caps::answer(query, out) {
                Some(status) => QueryOutcome::Caps(status),
                None => QueryOutcome::LeftToTheCore,
            },
        };
        self.traffic.query(query, outcome);
        match outcome {
            QueryOutcome::Record(status, _)
            | QueryOutcome::DriverName(status, _)
            | QueryOutcome::OpenGl(status, _)
            | QueryOutcome::Caps(status) => Some(status),
            QueryOutcome::LeftToTheCore => None,
        }
    }

    fn registry_value(&mut self, name: &RegistryName) -> Option<RegistryValue> {
        let value = guest_driver::registry(name, &self.generation.drivers);
        self.traffic.registry(name, value.is_some());
        value
    }

    /// An engine's cycle counter once work runs on the card. While listening, or if xe won't say,
    /// the host's clock stands in for it, in nanoseconds since the backend started.
    fn gpu_clock(&mut self) -> Result<u64, BackendError> {
        let card = self.work.as_mut().and_then(CardWork::clock);
        Ok(card.unwrap_or_else(|| self.started.elapsed().as_nanos() as u64))
    }

    fn context_allowed(&mut self, node: u32, client_hint: u32) -> bool {
        let allowed = protocol::nodes::offered(node);
        if !allowed {
            self.traffic.context_refused(node, client_hint);
        }
        allowed
    }

    fn context_data_valid(&mut self, process: u32, node: u32, private: &[u8]) -> bool {
        match Context::parse(private) {
            Ok(context) => {
                self.traffic.context(process, node, private, &context);
                true
            }
            Err(refused) => {
                self.traffic.refused("context", refused.0, private);
                false
            }
        }
    }

    fn context_gone(&mut self, process: u32, node: u32) {
        log::debug!("{PREFIX} context-gone process={process:#x} node={node}");
    }

    fn process_gone(&mut self, process: u32) {
        log::debug!("{PREFIX} process-gone process={process:#x}");
    }

    /// Each submission is logged with what its header says, as far as it can be read. While
    /// listening nothing runs, so none fails: each is finished unrun.
    fn start_work(&mut self, host: &mut dyn HostServices, work: &Work<'_>) -> WorkStart {
        let header = Header::read(work.private);
        let start = match &mut self.work {
            Some(card) => card.start(host, work, &header),
            None => WorkStart::Finished,
        };
        let answer = match start {
            WorkStart::Started { .. } => "started",
            WorkStart::QueueFailed => "queue-failed",
            _ => "finished-unrun",
        };
        self.traffic.submission(work, &header, answer);
        start
    }

    fn ring_doorbell(&mut self, _queue: u32, doorbell: u64) -> Result<Doorbell, BackendError> {
        match &mut self.work {
            Some(work) => work.ring_doorbell(doorbell),
            None => Ok(Doorbell::Rung),
        }
    }

    fn poll_work(&mut self, host: &mut dyn HostServices, now: Instant) {
        if let Some(work) = &mut self.work {
            work.poll(host, now);
        }
    }

    fn close_queue(&mut self, queue: u32) {
        self.traffic.queue_closed(queue);
        if let Some(work) = &mut self.work {
            work.close_queue(queue);
        }
    }

    /// By the CPU, at once: see [`work::copies`]. While listening, what would be copied was never
    /// drawn, so the copy is done by leaving the target as it is.
    fn start_staging_copy(&mut self, _host: &mut dyn HostServices, copy: &StagingCopy) -> StagingStart {
        self.traffic.copy(if copy.is_screen { "screen" } else { "staging" });
        let Some(work) = &mut self.work else { return StagingStart::Finished(Status::SUCCESS) };
        let Some(rows) = Rows::of_staging(copy, COPY_PIXEL_BYTES) else {
            log::warn!("{PREFIX} a copy to a staging surface that this backend can't place: {copy:?}");
            return StagingStart::Finished(Status::UNSUCCESSFUL);
        };
        StagingStart::Finished(copied(work.copy_rows(copy.own_space, &rows)))
    }

    /// A whole frame into a viewer's slot, by the CPU, at once.
    fn start_slot_copy(&mut self, _host: &mut dyn HostServices, copy: &SlotCopy) -> StagingStart {
        let Some(work) = &mut self.work else { return StagingStart::Finished(Status::UNSUCCESSFUL) };
        let Some(rows) = Rows::of_slot(copy, COPY_PIXEL_BYTES) else {
            return StagingStart::Finished(Status::UNSUCCESSFUL);
        };
        StagingStart::Finished(copied(work.copy_rows(copy.own_space, &rows)))
    }

    fn poll_staging_copy(&mut self, _host: &mut dyn HostServices, _now: Instant) -> Option<Status> {
        Some(Status::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use guesthouse_backend::{DriverVersion, StartDecision, decide_start};

    /// A recording of the test card in a file of its own.
    fn recorded(name: &str, text: &str) -> std::path::PathBuf {
        let file =
            std::env::temp_dir().join(format!("guesthouse-recorded-{}-{name}.card", std::process::id()));
        std::fs::write(&file, text).unwrap();
        file
    }

    #[test]
    fn a_recorded_card_stands_in_only_for_the_card_at_its_address() {
        let file = recorded("address", &recording::write(&card::tests::lunar_lake()));
        let card = recorded_card(&file, Some("0000:00:02.0")).unwrap();
        assert_eq!(card.pci.device, 0x64A0);
        assert!(recorded_card(&file, None).is_ok(), "a guest given no address takes it too");
        let other = recorded_card(&file, Some("0000:03:00.0")).unwrap_err();
        assert!(other.0.contains("is of the card at 0000:00:02.0"), "{}", other.0);
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn a_damaged_or_missing_recording_is_refused_and_says_where_it_is() {
        let file = recorded("damaged", "guesthouse-intel-card 1\nconfg 1\n");
        assert!(recorded_card(&file, None).unwrap_err().0.contains("is damaged: line 2"));
        std::fs::remove_file(&file).unwrap();
        assert!(recorded_card(&file, None).unwrap_err().0.contains("could not be read"));
    }

    fn setup_for(version: &str) -> Setup {
        Setup {
            guest_driver_version: DriverVersion::parse(version).unwrap(),
            guest_branch: version.into(),
            driver_folder: Default::default(),
            card: None,
            card_address: None,
        }
    }

    #[test]
    fn the_known_guest_release_starts_with_a_warning_and_another_is_refused() {
        let survey = IntelBackend::survey(&setup_for(guest_driver::GUEST_RELEASES[0])).unwrap();
        assert!(matches!(decide_start(&survey), StartDecision::RunWithWarning(_)));
        assert_eq!(survey.card, CardLabel::Experimental);
        assert!(!survey.mixed_branches_tested && !survey.host_version_tested, "nothing is claimed tested");
        let survey = IntelBackend::survey(&setup_for("31.0.101.5000")).unwrap();
        assert!(matches!(decide_start(&survey), StartDecision::Refuse(_)));
    }

    #[test]
    fn the_facts_come_from_the_card() {
        let backend = IntelBackend::from_card(card::tests::lunar_lake()).unwrap();
        let facts = backend.facts();
        assert_eq!((facts.card.vendor, facts.card.device, facts.card.revision), (INTEL_VENDOR, 0x64A0, 4));
        assert_eq!(facts.video_memory, 16 << 30, "half the system memory");
        assert!(facts.engines.graphics && facts.engines.compute);
        assert_eq!(facts.node_count, 15);
    }

    #[test]
    fn the_record_is_built_from_the_card_and_its_generation() {
        let backend = IntelBackend::from_card(card::tests::lunar_lake()).unwrap();
        let summary = record::summary(backend.record());
        assert_eq!((summary.product_family, summary.render_core, summary.device_id), (1275, 0xC09, 0x64A0));
        assert_eq!(summary.render_ip, IpWord::new(20, 4, 4).0);
        assert_eq!((summary.eu_count, summary.virtual_machine, summary.compression), (64, 1, false));
    }

    /// A test card given another generation's device id and render IP.
    fn card_as(mut card: CardFacts, device: u16, ip: xe_query::IpVersion) -> CardFacts {
        card.pci.device = device;
        card.config.device_id = device;
        card.gts.iter_mut().filter(|gt| gt.kind == Gt::MAIN).for_each(|gt| gt.ip = ip);
        card
    }

    fn ip(major: u16, minor: u16) -> xe_query::IpVersion {
        xe_query::IpVersion { major, minor, revision: 0 }
    }

    #[test]
    fn a_panther_lake_card_gets_xe3s_family_core_and_threads() {
        let card = card_as(card::tests::lunar_lake(), 0xB080, ip(30, 0));
        let backend = IntelBackend::from_card(card.clone()).unwrap();
        let summary = record::summary(backend.record());
        assert_eq!((summary.product_family, summary.render_core, summary.device_id), (1300, 0x1E00, 0xB080));
        assert!(protocol::features::is_set(backend.record(), protocol::features::XE2_TILING));
        let mut bare = card;
        bare.hwconfig = hwconfig::Hwconfig::default();
        let facts = record_facts(&bare, &generation::XE3_LPG, 0);
        assert_eq!(facts.gt.thread_count, 64 * 10, "ten threads to an EU where hwconfig doesn't say");
    }

    #[test]
    fn an_alchemist_card_with_no_ip_gets_dg2s_family_and_no_xe2_tiling() {
        let backend = IntelBackend::from_card(card_as(card::tests::battlemage(), 0x56A0, ip(0, 0))).unwrap();
        let summary = record::summary(backend.record());
        assert_eq!((summary.product_family, summary.render_core, summary.device_id), (1270, 0xC07, 0x56A0));
        assert_eq!(summary.render_ip, 0, "xe gives no IP for it, so the record says none");
        assert!(!protocol::features::is_set(backend.record(), protocol::features::XE2_TILING));
        assert!(protocol::features::is_set(backend.record(), protocol::features::LOCAL_MEMORY));
    }

    #[test]
    fn meteor_lake_and_arrow_lake_cards_get_their_own_family_by_id() {
        let meteor = IntelBackend::from_card(card_as(card::tests::lunar_lake(), 0x7D55, ip(12, 71))).unwrap();
        let arrow = IntelBackend::from_card(card_as(card::tests::lunar_lake(), 0x7D67, ip(12, 70))).unwrap();
        assert_eq!(record::summary(meteor.record()).product_family, 1272);
        assert_eq!(record::summary(arrow.record()).product_family, 1273);
        assert!(!protocol::features::is_set(arrow.record(), protocol::features::XE2_TILING));
        let unplaced = IntelBackend::from_card(card_as(card::tests::lunar_lake(), 0x7DFF, ip(12, 70)));
        assert!(unplaced.unwrap_err().0.contains("intel-probe --record"));
    }

    #[test]
    fn a_card_with_memory_of_its_own_offers_that() {
        let backend = IntelBackend::from_card(card::tests::battlemage()).unwrap();
        let facts = backend.facts();
        assert_eq!((facts.video_memory, facts.bar_size), (12216 << 20, 12216 << 20));
        let summary = record::summary(backend.record());
        assert_eq!((summary.product_family, summary.render_core, summary.device_id), (1274, 0xC09, 0xE20B));
        assert_eq!(summary.eu_count, 160);
        let built = record_facts(backend.card(), &generation::XE2_HPG, 7);
        assert_eq!(built.memory, Memory { dedicated: 12216 << 20, system_shared: 7, system_video: 0 });
        assert!(built.local_memory);
        assert_eq!(
            (built.gt.max_slices, built.gt.max_dual_subslices, built.gt.max_eu_per_subslice),
            (5, 20, 8)
        );
        assert_eq!(
            (built.gt.slice_count, built.gt.dual_subslice_count, built.gt.thread_count),
            (5, 20, 1280)
        );
    }

    #[test]
    fn a_card_with_no_eus_is_refused_in_plain_words() {
        let mut card = card::tests::lunar_lake();
        card.topology.clear();
        let refusal = IntelBackend::from_card(card).unwrap_err();
        assert!(refusal.0.contains("no EUs"), "{}", refusal.0);
    }

    #[test]
    fn a_card_with_no_profile_is_refused_in_plain_words() {
        let mut card = card::tests::lunar_lake();
        card.gts[0].ip.minor = 3;
        let refusal = IntelBackend::from_card(card).unwrap_err();
        assert!(refusal.0.contains("no profile"), "{}", refusal.0);
        assert!(refusal.0.contains("intel-probe --record"), "it says what to send: {}", refusal.0);
    }

    #[test]
    fn the_thread_count_falls_back_to_the_generations_when_hwconfig_has_none() {
        let mut card = card::tests::lunar_lake();
        card.hwconfig = hwconfig::Hwconfig::default();
        let facts = record_facts(&card, &generation::XE2_LPG, 0);
        assert_eq!(facts.gt.thread_count, 64 * generation::XE2_LPG.threads_per_eu);
        assert_eq!(facts.gt.max_slices, 0, "not guessed");
    }
    /// A backend on the test card, with a stand-in for xe that `open` hands out when asked to.
    fn started(asked: (Stage, Option<Fallback>), xe: &xe::fake::FakeXe) -> (IntelBackend, bool) {
        let mut backend = IntelBackend::from_card(card::tests::battlemage()).unwrap();
        let mut opened = false;
        let open = || {
            opened = true;
            Ok(Box::new(xe.clone()) as Box<dyn Xe + Send>)
        };
        backend.start_work_on_card(asked, open, None);
        (backend, opened)
    }

    #[test]
    fn with_nothing_asked_a_card_that_passes_its_test_runs_everything() {
        let xe = xe::fake::FakeXe::default();
        let (backend, opened) = started(Stage::asked(None), &xe);
        assert!(opened);
        assert_eq!(backend.stage(), stage::FURTHEST_STAGE);
        assert_eq!(xe.live(), 0, "the test at the start left nothing on the card");
        assert!(!xe.calls_named("exec").is_empty(), "and it did run");
    }

    #[test]
    fn a_recorded_card_listens_whatever_is_asked_and_nothing_is_opened() {
        for asked in [Stage::asked(None), (Stage::Compute, None), (Stage::Everything, None)] {
            let on_recording = on_a_recorded_card(asked);
            assert_eq!(on_recording, (Stage::Listen, Some(Fallback::RecordedCard)));
            let xe = xe::fake::FakeXe::default();
            let (backend, opened) = started(on_recording, &xe);
            assert_eq!(backend.stage(), Stage::Listen);
            assert!(!opened && xe.calls().is_empty());
        }
        let asked_to_listen = (Stage::Listen, Some(Fallback::AskedFor));
        assert_eq!(on_a_recorded_card(asked_to_listen.clone()), asked_to_listen, "its own reason is kept");
        assert!(no_card().is_err());
    }

    #[test]
    fn the_off_switch_and_an_unknown_word_keep_everything_off_the_card() {
        for value in ["0", "listen", "maybe"] {
            let xe = xe::fake::FakeXe::default();
            let asked = Stage::asked(Some(std::ffi::OsStr::new(value)));
            let (mut backend, opened) = started(asked, &xe);
            assert_eq!(backend.stage(), Stage::Listen, "{value}");
            assert!(!opened, "{value}: the card isn't even opened for work");
            // What a guest does at this stage is names in books, as before.
            let (memory, _) = backend.allocate_video_memory(0x1000, MemoryKind::PLAIN).unwrap();
            let space = backend.create_address_space(&guesthouse_backend::GPU_RANGES).unwrap();
            backend.map(&work::testing::map_request(space, memory, 0x400_0000, 0, 0x1000)).unwrap();
            let mut host = work::testing::FakeHost::default();
            let submission = work::testing::Rig::new(Stage::Everything).submission(1, 1);
            assert_eq!(backend.start_work(&mut host, &submission.work()), WorkStart::Finished);
            assert_eq!(backend.ring_doorbell(1, 1).unwrap(), Doorbell::Rung);
            backend.poll_work(&mut host, Instant::now());
            backend.release_memory(memory);
            assert!(xe.calls().is_empty(), "{value}: nothing reached xe");
        }
    }

    #[test]
    fn a_card_that_fails_its_test_at_the_start_listens_and_the_guest_still_starts() {
        let xe = xe::fake::FakeXe::default();
        xe.fail("exec", libc::EOPNOTSUPP);
        let (mut backend, opened) = started(Stage::asked(None), &xe);
        assert!(opened);
        assert_eq!(backend.stage(), Stage::Listen);
        assert_eq!(xe.live(), 0, "what the test made is gone");
        xe.heal();
        let calls = xe.calls().len();
        backend.allocate_video_memory(0x1000, MemoryKind::PLAIN).unwrap();
        assert_eq!(xe.calls().len(), calls, "and the guest's memory is names again");
    }

    #[test]
    fn a_card_that_cannot_be_opened_for_work_or_is_not_on_xe_listens() {
        let mut backend = IntelBackend::from_card(card::tests::battlemage()).unwrap();
        let denied = || Err(std::io::Error::from_raw_os_error(libc::EACCES));
        backend.start_work_on_card(Stage::asked(None), denied, None);
        assert_eq!(backend.stage(), Stage::Listen);
        let mut on_i915 = card::tests::battlemage();
        on_i915.driver = "i915".into();
        let mut backend = IntelBackend::from_card(on_i915).unwrap();
        let never =
            || -> std::io::Result<Box<dyn Xe + Send>> { panic!("a card not on xe is never opened for work") };
        backend.start_work_on_card(Stage::asked(None), never, None);
        assert_eq!(backend.stage(), Stage::Listen);
    }

    #[test]
    fn past_listening_the_trait_makes_real_things_and_runs_a_batch() {
        let xe = xe::fake::FakeXe::default();
        let (mut backend, _) = started((Stage::Everything, None), &xe);
        let before = xe.calls().len();
        let (batch, size) = backend.allocate_video_memory(0x1000, MemoryKind::PLAIN).unwrap();
        assert_eq!(size, 0x1000);
        let pages = work::testing::Pages::new(0x1000);
        let fence = backend.adopt_host_pages(pages.address(), 0x1000).unwrap();
        let space = backend.create_address_space(&guesthouse_backend::GPU_RANGES).unwrap();
        backend.map(&work::testing::map_request(space, batch, 0x1_4000_0000, 0, 0x1000)).unwrap();
        backend.map(&work::testing::map_request(space, fence, 0x500_0000, 0, 0x1000)).unwrap();
        assert_eq!(backend.live_memory_blocks(), 2);
        let view = backend.work.as_mut().unwrap();
        view.write_for_test(batch, &xe::batch::bytes(&[xe::batch::BATCH_END]));
        let mut host = work::testing::FakeHost::default();
        host.fence_at(0x7000_0000, pages.address());
        let mut submission = work::testing::Rig::new(Stage::Everything).submission(7, 3);
        submission.space = space;
        submission.commands = Some(guesthouse_backend::CommandBuffer { address: 0x1_4000_0000, length: 4 });
        let WorkStart::Started { doorbell, .. } = backend.start_work(&mut host, &submission.work()) else {
            panic!("the batch should start")
        };
        assert_eq!(backend.ring_doorbell(7, doorbell).unwrap(), Doorbell::Rung);
        assert_eq!(pages.qword(0), 3, "the card wrote the guest's fence");
        backend.poll_work(&mut host, Instant::now());
        let mut start = [0u8; 4];
        backend.read_memory_start(&mut host, space, batch, &mut start).unwrap();
        assert_eq!(start, xe::batch::BATCH_END.to_le_bytes());
        backend.close_queue(7);
        backend.destroy_address_space(space);
        backend.release_memory(batch);
        backend.release_memory(fence);
        assert_eq!(xe.live(), 0);
        assert!(xe.calls().len() > before);
        assert!(backend.gpu_clock().unwrap() > 0);
    }

    #[test]
    fn past_listening_the_card_sets_the_cap_and_slots_may_be_guest_pages() {
        let xe = xe::fake::FakeXe::default();
        let (backend, _) = started((Stage::Everything, None), &xe);
        // The test card: 12216 MiB, of which a quarter is kept for the host's desktop.
        assert_eq!(backend.video_memory_cap(), Some((12216 - 3054) << 20));
        assert!(backend.slots_may_be_guest_pages());
        let (listening, _) = started((Stage::Listen, None), &xe);
        assert_eq!(listening.video_memory_cap(), None);
        assert!(!listening.slots_may_be_guest_pages());
    }

    #[test]
    fn a_frame_is_copied_into_a_viewers_slot_and_a_tiled_slot_is_refused() {
        let xe = xe::fake::FakeXe::default();
        let (mut backend, _) = started((Stage::Everything, None), &xe);
        let (frame, slot) = (work::testing::Pages::new(0x1000), work::testing::Pages::new(0x1000));
        frame.write(0, &[9u8; 0x1000]);
        let from = backend.adopt_host_pages(frame.address(), 0x1000).unwrap();
        let to = backend.adopt_host_pages(slot.address(), 0x1000).unwrap();
        let space = backend.create_address_space(&guesthouse_backend::GPU_RANGES).unwrap();
        backend.map(&work::testing::map_request(space, from, 0x400_0000, 0, 0x1000)).unwrap();
        backend.map(&work::testing::map_request(space, to, 0x500_0000, 0, 0x1000)).unwrap();
        let source = guesthouse_backend::SurfaceShape {
            width: 16,
            height: 8,
            format: 0,
            pitch: 64,
            tiling: None,
            refresh: (0, 1),
            samples: (1, 0),
            colour: 0,
        };
        let copy = SlotCopy {
            source,
            source_address: 0x400_0000,
            slot_address: 0x500_0000,
            slot_pitch: 64,
            width: 16,
            height: 8,
            own_space: space,
            slot_tiling: None,
        };
        let mut host = work::testing::FakeHost::default();
        assert_eq!(backend.start_slot_copy(&mut host, &copy), StagingStart::Finished(Status::SUCCESS));
        assert_eq!(slot.bytes()[..512], [9u8; 512]);
        assert_eq!(slot.bytes()[512], 0);
        let tiled = SlotCopy { slot_tiling: Some(4), ..copy };
        assert_eq!(backend.start_slot_copy(&mut host, &tiled), StagingStart::Finished(Status::UNSUCCESSFUL));
        let outside = SlotCopy { height: 4096, ..copy };
        assert_eq!(
            backend.start_slot_copy(&mut host, &outside),
            StagingStart::Finished(Status::UNSUCCESSFUL)
        );
        assert!(backend.export_memory(from, 0x1000).is_err(), "guest pages are not the card's to export");
    }

    #[test]
    fn a_new_queues_data_goes_back_as_it_came_with_no_token_written() {
        let mut backend = IntelBackend::from_card(card::tests::battlemage()).unwrap();
        let mut data: Vec<u8> = (0..protocol::hw_queue::SIZE as u8).collect();
        let sent = data.clone();
        backend.finish_queue_data(0x40, &mut data, 1).unwrap();
        assert_eq!(data, sent, "the qword at +8 is written only for a native fence, which is never offered");
    }

    #[test]
    fn a_card_the_hosts_desktop_has_filled_leaves_the_guest_listening() {
        let mut busy = card::tests::battlemage();
        for region in busy.regions.iter_mut().filter(|region| region.class == xe_query::MemoryRegion::VRAM) {
            region.used = region.total - (3200 << 20);
        }
        let mut backend = IntelBackend::from_card(busy).unwrap();
        let xe = xe::fake::FakeXe::default();
        let open = || Ok(Box::new(xe.clone()) as Box<dyn Xe + Send>);
        backend.start_work_on_card(Stage::asked(None), open, None);
        assert_eq!(backend.stage(), Stage::Listen, "146 MiB past the reserve is no room for a guest");
        assert_eq!(backend.video_memory_cap(), None);
        assert_eq!(xe.live(), 0);
    }
}
