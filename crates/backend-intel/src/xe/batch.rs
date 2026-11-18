//! The few GPU instructions this backend writes itself, and how it tells where a batch ends.
//!
//! The encodings are the kernel's, from Linux 7.2's `drivers/gpu/drm/xe/instructions/`
//! (`xe_instr_defs.h`, `xe_mi_commands.h`, `xe_gpu_commands.h`), with the layouts its own code
//! emits in `xe_ring_ops.c` and `xe_migrate.c`. Each constant names the definition it is. The MI
//! instructions work on every engine; the copy is the copy engine's.

/// An instruction's kind is its top three bits, `XE_INSTR_CMD_TYPE`, and MI is kind zero. An MI
/// instruction's opcode is bits 28 to 23, `MI_OPCODE`.
const MI_OPCODE_SHIFT: u32 = 23;
const fn mi(opcode: u32) -> u32 {
    opcode << MI_OPCODE_SHIFT
}

/// `MI_NOOP`.
pub const NOOP: u32 = mi(0x00);
/// `MI_BATCH_BUFFER_END`.
pub const BATCH_END: u32 = mi(0x0A);
/// `MI_STORE_DATA_IMM`.
const STORE_DATA: u32 = mi(0x20);
/// `MI_SDI_GGTT`: the address is in the kernel's global table, which a guest must never reach.
pub const STORE_IN_GLOBAL_TABLE: u32 = 1 << 22;
/// The bit `MI_SDI_NUM_QW` sets: the value is 64 bits wide.
const STORE_QWORD: u32 = 1 << 21;
/// `MI_BATCH_BUFFER_START`.
const BATCH_START: u32 = mi(0x31);
/// What `get_ppgtt_flag` in `xe_ring_ops.c` adds: the address is in the queue's own VM.
const BATCH_START_IN_VM: u32 = 1 << 8;

/// `MI_SEMAPHORE_WAIT`, which holds the engine until a value in memory compares as asked. A ring
/// fed from user mode parks in one where a batch would end.
const SEMAPHORE_WAIT: u32 = mi(0x1C);
/// `MI_ARB_CHECK`, a point where the engine may switch away, with or without its pre-parser bits.
/// It moves nothing.
const ARB_CHECK: u32 = mi(0x05);
/// `MI_SEMAPHORE_WAIT | XE_INSTR_NUM_DW(5)`, as the kernel defines it for these cards.
pub const SEMAPHORE_WAIT_HEADER: u32 = SEMAPHORE_WAIT | (SEMAPHORE_WAIT_DWORDS as u32 - LENGTH_BIAS);
pub const SEMAPHORE_WAIT_DWORDS: usize = 5;

/// The dwords before an instruction's length field counts, `XE_INSTR_NUM_DW`'s two.
const LENGTH_BIAS: u32 = 2;

/// `MI_STORE_DATA_IMM | MI_SDI_NUM_QW(1)`: one 64-bit value to an address in the VM.
pub const STORE_QWORD_HEADER: u32 = STORE_DATA | STORE_QWORD | (STORE_QWORD_DWORDS as u32 - LENGTH_BIAS);
pub const STORE_QWORD_DWORDS: usize = 5;
/// `MI_BATCH_BUFFER_START | BIT(8) | XE_INSTR_NUM_DW(3)`, as `emit_bb_start` writes it.
pub const BATCH_START_HEADER: u32 =
    BATCH_START | BATCH_START_IN_VM | (BATCH_START_DWORDS as u32 - LENGTH_BIAS);
pub const BATCH_START_DWORDS: usize = 3;

/// `MI_LOAD_REGISTER_IMM | MI_LRI_NUM_REGS(1)`: one register set to a value.
pub const LOAD_REGISTER_HEADER: u32 = mi(0x22) | (LOAD_REGISTER_DWORDS as u32 - LENGTH_BIAS);
pub const LOAD_REGISTER_DWORDS: usize = 3;
/// `MI_STORE_REGISTER_MEM | XE_INSTR_NUM_DW(4)`: a register's 32 bits to an address in the VM.
pub const STORE_REGISTER_HEADER: u32 = mi(0x24) | (STORE_REGISTER_DWORDS as u32 - LENGTH_BIAS);
pub const STORE_REGISTER_DWORDS: usize = 4;

/// `MEM_COPY_CMD`, which every Xe2 card has (`has_mem_copy_instr` in `xe_pci.c`).
const MEM_COPY: u32 = 2 << 29 | 0x5A << 22;
/// `MEM_COPY_PAGE_COPY_MODE`: the width counts 256-byte pages.
const MEM_COPY_PAGES: u32 = 1 << 19;
/// `MEM_COPY_MATRIX_COPY`: rows of a width, each a pitch apart.
const MEM_COPY_MATRIX: u32 = 1 << 17;
pub const MEM_COPY_DWORDS: usize = 10;
/// The page of `MEM_COPY_PAGE_COPY_MODE`, `PAGE_COPY_MODE_PS` in `xe_migrate.c`.
pub const MEM_COPY_PAGE: u64 = 256;
/// The most a width, a height or a pitch field holds, one less than it means.
const MEM_COPY_FIELD_MOST: u64 = 1 << 16;
/// `MEM_COPY_SRC_MOCS_INDEX_MASK` and `MEM_COPY_DST_MOCS_INDEX_MASK` start here.
const MEM_COPY_SOURCE_MOCS_SHIFT: u32 = 28;
const MEM_COPY_TARGET_MOCS_SHIFT: u32 = 3;
/// The uncached entry of xe's own Xe2 cache table, `uc_index` for Lunar Lake and Battlemage in
/// `xe_mocs.c`. The kernel's copies use it, so ours do.
pub const XE2_UNCACHED_MOCS: u32 = 3;

fn address_dwords(address: u64) -> [u32; 2] {
    [address as u32, (address >> 32) as u32]
}

/// Writes a 64-bit value at a GPU address, as `emit_store_imm_ppgtt_posted` does.
pub fn store_qword(address: u64, value: u64) -> [u32; STORE_QWORD_DWORDS] {
    let [low, high] = address_dwords(address);
    [STORE_QWORD_HEADER, low, high, value as u32, (value >> 32) as u32]
}

/// Sets a register, named by its offset in the card's register space.
pub fn load_register(register: u32, value: u32) -> [u32; LOAD_REGISTER_DWORDS] {
    [LOAD_REGISTER_HEADER, register, value]
}

/// Writes a register's 32 bits at a GPU address.
pub fn store_register(register: u32, address: u64) -> [u32; STORE_REGISTER_DWORDS] {
    let [low, high] = address_dwords(address);
    [STORE_REGISTER_HEADER, register, low, high]
}

/// Goes on at another address in the same VM.
pub fn batch_start(address: u64) -> [u32; BATCH_START_DWORDS] {
    let [low, high] = address_dwords(address);
    [BATCH_START_HEADER, low, high]
}

/// Why a copy can't be written as one instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CopyTooBig;

/// Copies `bytes` from one GPU address to another, as `emit_mem_copy` does for a run of bytes: by
/// 256-byte pages when everything is a multiple of one, and byte by byte otherwise.
pub fn copy_bytes(
    source: u64,
    target: u64,
    bytes: u64,
    mocs: u32,
) -> Result<[u32; MEM_COPY_DWORDS], CopyTooBig> {
    let aligned = |value: u64| value.is_multiple_of(MEM_COPY_PAGE);
    let (mode, width) = if aligned(bytes) && aligned(source) && aligned(target) {
        (MEM_COPY_PAGES, bytes / MEM_COPY_PAGE)
    } else {
        (0, bytes)
    };
    if width == 0 || width > MEM_COPY_FIELD_MOST {
        return Err(CopyTooBig);
    }
    Ok(mem_copy(mode, width as u32, 1, (1, 1), source, target, mocs))
}

/// Copies `rows` rows of `width` bytes, the rows `pitch` apart at both ends: `emit_mem_copy`'s
/// matrix copy. Dword 3 is the source's pitch and dword 4 the target's; the kernel gives one pitch
/// for both, and so does this, so rows that are differently far apart at the two ends are copied
/// one by one with [`copy_bytes`].
pub fn copy_rows(
    source: u64,
    target: u64,
    width: u32,
    rows: u32,
    pitch: u32,
    mocs: u32,
) -> Result<[u32; MEM_COPY_DWORDS], CopyTooBig> {
    let fits = |value: u32| value != 0 && u64::from(value) <= MEM_COPY_FIELD_MOST;
    if !(fits(width) && fits(rows) && fits(pitch)) {
        return Err(CopyTooBig);
    }
    Ok(mem_copy(MEM_COPY_MATRIX, width, rows, (pitch, pitch), source, target, mocs))
}

fn mem_copy(
    mode: u32,
    width: u32,
    rows: u32,
    pitches: (u32, u32),
    source: u64,
    target: u64,
    mocs: u32,
) -> [u32; MEM_COPY_DWORDS] {
    let [source_low, source_high] = address_dwords(source);
    let [target_low, target_high] = address_dwords(target);
    [
        MEM_COPY | mode | (MEM_COPY_DWORDS as u32 - LENGTH_BIAS),
        width - 1,
        rows - 1,
        pitches.0 - 1,
        pitches.1 - 1,
        source_low,
        source_high,
        target_low,
        target_high,
        mocs << MEM_COPY_SOURCE_MOCS_SHIFT | mocs << MEM_COPY_TARGET_MOCS_SHIFT,
    ]
}

/// Dwords as the bytes a batch holds.
pub fn bytes(dwords: &[u32]) -> Vec<u8> {
    dwords.iter().flat_map(|dword| dword.to_le_bytes()).collect()
}

/// An MI instruction's opcode, or `None` for an instruction of another kind.
fn mi_opcode(dword: u32) -> Option<u32> {
    const KIND_SHIFT: u32 = 29;
    const OPCODE_MASK: u32 = 0x3F;
    (dword >> KIND_SHIFT == 0).then_some((dword >> MI_OPCODE_SHIFT) & OPCODE_MASK)
}

fn is(dword: u32, instruction: u32) -> bool {
    mi_opcode(dword) == mi_opcode(instruction)
}

/// How a batch ends, judged by its last dwords.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// `MI_BATCH_BUFFER_END`, with nothing but `MI_NOOP`s after it.
    Ends,
    /// It goes on in another batch at this address, which ends it as far as this one goes.
    Chains(u64),
    /// It parks in a semaphore wait and never ends by itself: a ring that the guest's driver
    /// feeds from user mode, writing more work after the wait and then letting it through.
    Parks,
    /// None of these: the engine would run on into whatever lies after it.
    RunsOn,
}

/// Reads the end of a batch from its last bytes. `tail` is as much of the batch's end as the
/// caller could read, and a few dwords are enough. Bytes that aren't whole dwords run on.
pub fn ending(tail: &[u8]) -> Ending {
    let (chunks, rest) = tail.as_chunks::<4>();
    if chunks.is_empty() || !rest.is_empty() {
        return Ending::RunsOn;
    }
    let dwords: Vec<u32> = chunks.iter().map(|bytes| u32::from_le_bytes(*bytes)).collect();
    let padding_from = |at: usize| dwords[at..].iter().all(|&dword| is(dword, NOOP));
    // A chain's address can read as padding, so the chain is looked for before the padding is
    // skipped: a start instruction, its two address dwords, and nothing but padding after.
    let chain = (0..dwords.len().saturating_sub(BATCH_START_DWORDS - 1))
        .rev()
        .find(|&at| is(dwords[at], BATCH_START) && padding_from(at + BATCH_START_DWORDS));
    if let Some(at) = chain {
        return Ending::Chains(u64::from(dwords[at + 2]) << 32 | u64::from(dwords[at + 1]));
    }
    let last = dwords.iter().rposition(|&dword| !is(dword, NOOP));
    // An end instruction ends the batch, unless it sits where a wait would keep its data or its
    // address: those two dwords can hold anything, and the dwords after them can't hold this.
    if let Some(last) = last
        && dwords[last] == BATCH_END
    {
        let in_a_wait = WAIT_OPERANDS_THAT_CAN_BE_ANYTHING
            .into_iter()
            .any(|back| last.checked_sub(back).is_some_and(|at| is_wait(dwords[at])));
        if !in_a_wait {
            return Ending::Ends;
        }
    }
    // A wait with nothing but padding after its five dwords: a ring, parked.
    let parks = dwords.len().checked_sub(SEMAPHORE_WAIT_DWORDS).is_some_and(|last_start| {
        (0..=last_start).rev().any(|at| is_wait(dwords[at]) && padding_from(at + SEMAPHORE_WAIT_DWORDS))
    });
    if parks {
        return Ending::Parks;
    }
    match last {
        Some(last) if is(dwords[last], BATCH_END) => Ending::Ends,
        _ => Ending::RunsOn,
    }
}

/// [`ending`] for a batch whose read tail starts at `tail_address`, which also knows one more shape
/// of ring, seen in the lab from Intel's OpenCL driver (direct submission): a semaphore wait, then
/// a chain forward into this same batch, past the chain, where nothing waits but padding and
/// `MI_ARB_CHECK`s. The engine parks in the wait; the driver writes its next work into that
/// padding before it lets the wait through. A chain back, or anywhere else, is not this.
pub fn ending_at(tail: &[u8], tail_address: u64) -> Ending {
    let found = ending(tail);
    if found != Ending::RunsOn || !tail.len().is_multiple_of(4) {
        return found;
    }
    let dwords: Vec<u32> = tail.as_chunks::<4>().0.iter().map(|bytes| u32::from_le_bytes(*bytes)).collect();
    let filler = |dword: u32| is(dword, NOOP) || is(dword, ARB_CHECK);
    let Some(chain) = (0..dwords.len().saturating_sub(BATCH_START_DWORDS - 1))
        .rev()
        .find(|&at| is(dwords[at], BATCH_START))
    else {
        return found;
    };
    let target = u64::from(dwords[chain + 2]) << 32 | u64::from(dwords[chain + 1]);
    let after_chain = tail_address + 4 * (chain + BATCH_START_DWORDS) as u64;
    let end = tail_address + 4 * dwords.len() as u64;
    let into_own_tail = (after_chain..end).contains(&target) && target.is_multiple_of(4);
    let rest_is_filler = dwords[chain + BATCH_START_DWORDS..].iter().all(|&dword| filler(dword));
    let waits_first = chain >= SEMAPHORE_WAIT_DWORDS
        && (0..=chain - SEMAPHORE_WAIT_DWORDS).rev().any(|at| {
            is_wait(dwords[at]) && chain - (at + SEMAPHORE_WAIT_DWORDS) <= MOST_BETWEEN_WAIT_AND_CHAIN
        });
    if into_own_tail && rest_is_filler && waits_first { Ending::Parks } else { found }
}

/// How many dwords may lie between such a ring's wait and its chain: in the lab, one
/// (an arbitration switch).
const MOST_BETWEEN_WAIT_AND_CHAIN: usize = 2;

/// How far after a wait's header its data and its address's low half lie.
const WAIT_OPERANDS_THAT_CAN_BE_ANYTHING: [usize; 2] = [1, 2];

/// Whether a dword is a semaphore wait's header: the opcode, and the length the instruction has
/// on these cards, whatever its mode bits say.
fn is_wait(dword: u32) -> bool {
    const LENGTH_MASK: u32 = 0xFF;
    is(dword, SEMAPHORE_WAIT) && dword & LENGTH_MASK == SEMAPHORE_WAIT_HEADER & LENGTH_MASK
}

/// How many bytes of a batch's end [`ending`] wants: room for a chain and some padding after it.
pub const ENDING_BYTES: usize = 4096;

#[cfg(test)]
mod tests {
    use super::*;

    /// The ring OpenCL's driver submitted in the lab, at 0x7b_044d_0000, 252 bytes: work started
    /// elsewhere, then a wait, an arbitration switch, and a chain to its own offset 0x58.
    fn opencl_ring() -> Vec<u32> {
        let mut dwords = vec![
            0x6109_0001,
            0x6cf1_0000,
            0x0000_0217,
            0x7a00_0004,
            0x0014_0480,
            0,
            0,
            0,
            0,
            0x1880_0101,
            0x044c_0000,
            0x0000_007b,
            0x0280_0101,
            0x0e00_9003,
            0x0000_0002,
            0x0457_0000,
            0x0000_007b,
            0,
            0x0480_0001,
            0x1880_0101,
            0x044d_0058,
            0x0000_007b,
            0x0280_0100,
        ];
        dwords.resize(63, 0);
        dwords
    }

    #[test]
    fn a_wait_then_a_chain_into_its_own_padding_is_a_ring() {
        let ring = bytes(&opencl_ring());
        assert_eq!(ending(&ring), Ending::RunsOn, "by its end alone");
        assert_eq!(ending_at(&ring, 0x7b_044d_0000), Ending::Parks);
        assert_eq!(ending_at(&ring, 0x7b_0440_0000), Ending::RunsOn, "the chain leaves the batch");
        let mut back = opencl_ring();
        back[20] = 0x044d_0000;
        assert_eq!(ending_at(&bytes(&back), 0x7b_044d_0000), Ending::RunsOn, "a chain back loops");
        let mut no_wait = opencl_ring();
        no_wait[13] = 0;
        assert_eq!(ending_at(&bytes(&no_wait), 0x7b_044d_0000), Ending::RunsOn, "no wait, no ring");
        let mut work_after = opencl_ring();
        work_after[30] = 0x7a00_0004;
        assert_eq!(ending_at(&bytes(&work_after), 0x7b_044d_0000), Ending::RunsOn);
        let early = bytes(&[0x1880_0101, 0x100c, 0, 0x0280_0100]);
        assert_eq!(ending_at(&early, 0x1000), Ending::RunsOn, "a chain too early for a wait before it");
        let ends = bytes(&[0x0e00_9003, 2, 0, 0, 0, BATCH_END]);
        assert_eq!(ending_at(&ends, 0x1000), ending(&ends), "the other shapes are read as before");
    }

    /// The values the kernel's macros give, worked from `xe_mi_commands.h` in Linux 7.2:
    /// `__MI_INSTR(op)` is `op << 23`, `MI_SDI_NUM_QW(1)` is `(2 * 1 + 3 - 2) | BIT(21)`, and
    /// `XE_INSTR_NUM_DW(3)` is 1.
    #[test]
    fn the_mi_dwords_are_the_kernels() {
        assert_eq!(NOOP, 0x0000_0000);
        assert_eq!(BATCH_END, 0x0500_0000);
        assert_eq!(STORE_QWORD_HEADER, 0x1020_0003);
        assert_eq!(STORE_QWORD_HEADER & STORE_IN_GLOBAL_TABLE, 0, "never the kernel's global table");
        assert_eq!(STORE_QWORD_HEADER & 0x3FF, 3, "MI_SDI_LEN_DW");
        assert_eq!(BATCH_START_HEADER, 0x1880_0101);
    }

    /// `emit_store_imm_ppgtt_posted` in `xe_ring_ops.c`: header, address low and high, value low
    /// and high.
    #[test]
    fn a_store_is_laid_out_as_the_kernel_writes_its_user_fence() {
        assert_eq!(
            store_qword(0x7FFF_FFE0_0008, 0x1122_3344_5566_7788),
            [0x1020_0003, 0xFFE0_0008, 0x7FFF, 0x5566_7788, 0x1122_3344]
        );
        assert_eq!(batch_start(0x1_4000_2000), [0x1880_0101, 0x4000_2000, 1]);
    }

    /// `MI_LOAD_REGISTER_IMM | MI_LRI_NUM_REGS(1)` is `0x22 << 23 | (2 * 1 + 1 - 2)`, and
    /// `MI_STORE_REGISTER_MEM | XE_INSTR_NUM_DW(4)` is `0x24 << 23 | 2`; neither asks for the
    /// kernel's global table. The guest's own batches set a register with the same first dword.
    #[test]
    fn a_register_is_set_and_read_back_with_the_kernels_instructions() {
        assert_eq!(load_register(0x7004, 0x0040_0040), [0x1100_0001, 0x7004, 0x0040_0040]);
        assert_eq!(store_register(0x7004, 0x7FFF_FFE0_0400), [0x1200_0002, 0x7004, 0xFFE0_0400, 0x7FFF]);
        assert_eq!(STORE_REGISTER_HEADER & STORE_IN_GLOBAL_TABLE, 0);
    }

    /// `emit_mem_copy` in `xe_migrate.c` for a whole number of 256-byte pages: `MEM_COPY_CMD |
    /// MEM_COPY_PAGE_COPY_MODE | (10 - 2)`, width in pages less one, then source, target, and the
    /// uncached entry for both. The three dwords between (rows less one and the two pitches less
    /// one) are 0 here; the kernel writes other values there, which the hardware ignores in page
    /// mode, as its comment says.
    #[test]
    fn a_copy_of_whole_pages_is_the_kernels_page_copy() {
        let copy = copy_bytes(0x1_4000_0000, 0x7FFF_FFE0_0000, 0x1_0000, XE2_UNCACHED_MOCS).unwrap();
        assert_eq!(copy, [0x5688_0008, 0xFF, 0, 0, 0, 0x4000_0000, 1, 0xFFE0_0000, 0x7FFF, 0x3000_0018]);
    }

    #[test]
    fn a_copy_that_is_not_whole_pages_goes_byte_by_byte() {
        let copy = copy_bytes(0x1000, 0x2001, 100, XE2_UNCACHED_MOCS).unwrap();
        assert_eq!((copy[0], copy[1]), (0x5680_0008, 99));
    }

    #[test]
    fn a_copy_of_rows_is_the_kernels_matrix_copy() {
        let copy = copy_rows(0x1000, 0x8000, 7680, 1080, 8192, XE2_UNCACHED_MOCS).unwrap();
        assert_eq!(copy[0], 0x5682_0008);
        assert_eq!(copy[1..5], [7679, 1079, 8191, 8191]);
        assert_eq!(copy[5..9], [0x1000, 0, 0x8000, 0]);
    }

    #[test]
    fn a_copy_past_what_one_instruction_holds_is_refused() {
        assert_eq!(copy_bytes(0, 0x1000, 0, 3), Err(CopyTooBig));
        assert!(copy_bytes(0, 0x100_0000, 256 << 16, 3).is_ok(), "the most pages");
        assert_eq!(copy_bytes(0, 0x200_0000, (256 << 16) + 256, 3), Err(CopyTooBig));
        assert_eq!(copy_bytes(1, 0x1000, 0x1_0001, 3), Err(CopyTooBig), "too many single bytes");
        assert_eq!(copy_rows(0, 0x1000, 0, 4, 16, 3), Err(CopyTooBig));
        assert_eq!(copy_rows(0, 0x1000, 16, 0x1_0001, 16, 3), Err(CopyTooBig));
    }

    fn tail(dwords: &[u32]) -> Vec<u8> {
        bytes(dwords)
    }

    #[test]
    fn a_batch_ends_at_its_end_instruction_whatever_padding_follows() {
        assert_eq!(ending(&tail(&[0x7A00_0004, BATCH_END])), Ending::Ends);
        assert_eq!(ending(&tail(&[BATCH_END, NOOP, NOOP, NOOP])), Ending::Ends);
        // A no-op may carry an identification number in its low bits.
        assert_eq!(ending(&tail(&[BATCH_END, NOOP | 0x40_1234])), Ending::Ends);
    }

    #[test]
    fn a_batch_that_goes_on_elsewhere_says_where() {
        let chain = batch_start(0x255_F13C_0000);
        assert_eq!(ending(&tail(&chain)), Ending::Chains(0x255_F13C_0000));
        assert_eq!(ending(&tail(&[chain[0], chain[1], chain[2], NOOP])), Ending::Chains(0x255_F13C_0000));
    }

    /// `MI_SEMAPHORE_WAIT` in Linux 7.2's `xe_mi_commands.h`: `__MI_INSTR(0x1c) | XE_INSTR_NUM_DW(5)`.
    #[test]
    fn a_batch_that_parks_in_a_semaphore_wait_is_a_ring() {
        assert_eq!(SEMAPHORE_WAIT_HEADER, 0x0E00_0003);
        // Polling, compare greater or equal: the wait a user-mode ring ends each piece of work with.
        let wait = [SEMAPHORE_WAIT_HEADER | 1 << 15 | 1 << 12, 1, 0x4000_1000, 0x7F, 0];
        assert_eq!(ending(&tail(&wait)), Ending::Parks);
        let mut padded = vec![0x7A00_0004, 0x1234];
        padded.extend(wait);
        padded.extend([NOOP, NOOP]);
        assert_eq!(ending(&tail(&padded)), Ending::Parks);
        // A wait in the middle of a batch that then ends is an ordinary batch.
        let mut ends = wait.to_vec();
        ends.push(BATCH_END);
        assert_eq!(ending(&tail(&ends)), Ending::Ends);
        assert_eq!(ending(&tail(&wait[..4])), Ending::RunsOn, "a wait cut short");
    }

    #[test]
    fn a_dword_that_reads_as_a_wait_before_an_end_does_not_make_a_batch_a_ring() {
        // An address or a value that happens to have a wait's opcode and length, with the end
        // where a wait's last two dwords would be: those can't hold an end instruction.
        let looks_like_a_wait = SEMAPHORE_WAIT_HEADER | 0x2000;
        assert_eq!(ending(&tail(&[looks_like_a_wait, 1, 2, 3, BATCH_END])), Ending::Ends);
        assert_eq!(ending(&tail(&[looks_like_a_wait, 1, 2, BATCH_END])), Ending::Ends);
        assert_eq!(ending(&tail(&[looks_like_a_wait, 1, 2, BATCH_END, NOOP, NOOP])), Ending::Ends);
        assert_eq!(ending(&tail(&[looks_like_a_wait, 1, 2, 3, BATCH_END, NOOP])), Ending::Ends);
        // The opcode alone, with another length, is no wait at all.
        assert_eq!(ending(&tail(&[0x0E00_2000, BATCH_END, NOOP, NOOP, NOOP])), Ending::Ends);
        assert_eq!(ending(&tail(&[0x0E00_2000, 1, 2, 3, 4])), Ending::RunsOn);
    }

    #[test]
    fn a_ring_whose_semaphore_lies_at_an_address_that_reads_as_an_end_is_still_a_ring() {
        // The semaphore at GPU address 0x0500_0000, which as a dword is the end instruction.
        let wait = [SEMAPHORE_WAIT_HEADER | 1 << 15 | 1 << 12, 1, BATCH_END, 0, 0];
        assert_eq!(ending(&tail(&wait)), Ending::Parks);
        // And waiting for the value 0x0500_0000.
        let wait = [SEMAPHORE_WAIT_HEADER | 1 << 15 | 1 << 12, BATCH_END, 0, 0, 0];
        assert_eq!(ending(&tail(&wait)), Ending::Parks);
    }

    #[test]
    fn a_batch_with_no_end_runs_on() {
        assert_eq!(ending(&tail(&[0x7A00_0004, 0x1234_5678])), Ending::RunsOn);
        assert_eq!(ending(&tail(&[NOOP, NOOP])), Ending::RunsOn, "only padding");
        assert_eq!(ending(&[]), Ending::RunsOn);
        assert_eq!(ending(&[0, 0, 0, 5, 0]), Ending::RunsOn, "not whole dwords");
        // A pipeline instruction whose bits 28 to 23 happen to read as the end's opcode.
        assert_eq!(ending(&tail(&[0x6500_0000])), Ending::RunsOn);
        // A chain cut short: the start instruction with only one dword after it.
        assert_eq!(ending(&tail(&[BATCH_START_HEADER, 0x1000])), Ending::RunsOn);
    }
}
