//! The choices about running a guest's work that the card has not yet confirmed, each in one named
//! place so that changing one is one edit.
//!
//! What was read in Intel's Windows driver and in compute-runtime about each is in the private
//! design notes. Nothing here has run on a card.

use crate::xe::abi;

/// The flags a guest process's address space is made with. No scratch page: a batch that touches
/// an address nothing is mapped at must fault, so its queue is banned and the guest hears its device
/// is gone, rather than read zeros and go on with wrong results.
pub const VM_FLAGS: u32 = 0;
/// The flags of the second address space a process gets for work that never ends: see [`Rings`].
pub const RING_VM_FLAGS: u32 = abi::VM_LONG_RUNNING;

/// What becomes of a submission whose batch parks in a semaphore wait instead of ending. Intel's
/// compute driver submits such a ring once and then feeds it from user mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rings {
    /// It runs on a queue in a long-running address space of the same process, which xe gives no
    /// job timeout. The ring writes the queue's fence itself, so the card is asked for none, and a
    /// lost ring is noticed by asking xe whether its queue was banned.
    LongRunning,
    /// It is refused, and its queue stops. With this, the guest's driver must be kept from making
    /// rings some other way, such as by not offering it the engines it makes them on.
    Refused,
}

pub const RINGS: Rings = Rings::LongRunning;

/// The unit of a submission's command length, as the message gives it: bytes.
pub const COMMAND_LENGTH_UNIT: u64 = 1;

/// What a new hardware queue's private data gets written back at +8: nothing. Intel's kernel
/// driver writes there only for a queue made with a native progress fence, which this backend never
/// offers, so the qword stays as the guest sent it.
pub const WRITES_QUEUE_TOKEN: bool = false;

/// Register state Intel's Windows kernel driver sets for every context that xe's own context image
/// may not have. Nothing is done about any of it: a wrong picture or a slow kernel on a real card
/// is to be read against this list first.
pub const MAY_DIFFER_FROM_WINDOWS: [&str; 4] = [
    "register 0x5584 bit 5, set for every context on Windows and unnamed in xe",
    "the rows of the Windows driver's workaround table for this stepping, which were not read",
    "the tiled-resource translation table, which xe never turns on; the record leaves its two feature bits clear",
    "the cache-policy table: the guest's indexes mean what Windows programs, and xe programs its own",
];

/// How often work on the card is looked at for runs xe ended early.
pub const LOOK_AT_RUNS_EVERY: std::time::Duration = std::time::Duration::from_millis(50);
