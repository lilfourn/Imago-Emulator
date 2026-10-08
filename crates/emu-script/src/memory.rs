//! A CPU's view of memory, as scripts read and write it: little-endian
//! values of 1, 2 or 4 bytes, through the core's debugger.

use emu_core::Core;

/// The `bytes`-byte little-endian value at `address` as `cpu` sees it,
/// read without side effects, or `None` if any byte cannot be read.
///
/// Reads go through [`emu_core::Debugger::read`], which also reaches ROM
/// and the I/O registers whose reads change nothing. A core without a
/// debugger is read through [`Core::peek`], as player 0's plain RAM.
pub fn read(core: &mut dyn Core, cpu: usize, address: u32, bytes: u32) -> Option<u32> {
    (0..bytes).try_fold(0, |value, index| {
        let byte = read_byte(core, cpu, address.wrapping_add(index))?;
        Some(value | u32::from(byte) << (8 * index))
    })
}

/// Writes `value`'s low `bytes` bytes, little-endian, at `address` as `cpu`
/// sees it, without side effects or time passing. Returns whether every
/// byte was memory that took it: RAM, video memory, palettes and OAM, never
/// ROM or I/O registers. Bytes that can be written are, even if others
/// cannot.
pub fn write(core: &mut dyn Core, cpu: usize, address: u32, bytes: u32, value: u32) -> bool {
    (0..bytes).fold(true, |all, index| {
        let byte = value.to_le_bytes()[index as usize];
        write_byte(core, cpu, address.wrapping_add(index), byte) && all
    })
}

fn read_byte(core: &mut dyn Core, cpu: usize, address: u32) -> Option<u8> {
    match core.debugger() {
        Some(debugger) => debugger.read(cpu, address),
        None => core.peek(0, address),
    }
}

fn write_byte(core: &mut dyn Core, cpu: usize, address: u32, value: u8) -> bool {
    match core.debugger() {
        Some(debugger) => debugger.write(cpu, address, value),
        None => core.poke(0, address, value),
    }
}
