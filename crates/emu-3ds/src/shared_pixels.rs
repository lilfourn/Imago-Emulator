//! Memory the app and the helper both map, where the helper leaves each
//! frame's pixels for the app to copy, on Unix (macOS and Linux).
//!
//! A 3DS frame drawn at 4× is 11 MB. Through the helper's stdout, a pipe
//! of 64 KB on macOS, it took about 170 writes and as many reads, each
//! waking the other process: 2 ms a frame on an M5 Max with the GPU busy,
//! an eighth of a 60 Hz frame. Here the helper writes the pixels once and
//! the link's [`FRAME`](crate::link::FRAME) message carries only their
//! sizes and the sound.
//!
//! The app makes the memory, a POSIX shared memory object, and names it
//! to the helper in [`ENV`]. The helper opens it and removes the name at
//! once, so nothing is left behind however either process ends; the app
//! removes it too, in case the helper never opened it. The two take turns
//! with it, as they do with the link: the helper writes a frame's pixels
//! before it answers a [`RUN`](crate::link::RUN), and the app reads them
//! before it asks for the next.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};

/// The environment variable that names the memory to the helper.
pub const ENV: &str = "DS_EMULATOR_3DS_PIXELS";

/// Only the user may open it.
const MODE: libc::mode_t = 0o600;

/// A mapping of a shared memory object, read and written in place.
pub struct SharedPixels {
    start: NonNull<u8>,
    len: usize,
}

// SAFETY: the mapping is plain memory that belongs to this value alone in
// this process; moving it to another thread moves the only access to it.
unsafe impl Send for SharedPixels {}

impl SharedPixels {
    /// Makes `len` bytes of shared memory with a name no other memory has,
    /// returning it and its name, which is removed when dropped.
    ///
    /// # Errors
    ///
    /// Fails if the system will not make or map it.
    pub fn create(len: usize) -> io::Result<(Self, Name)> {
        // A name of at most 31 bytes, as macOS allows.
        static MADE: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "/ds3ds.{}.{}",
            std::process::id(),
            MADE.fetch_add(1, Ordering::Relaxed)
        );
        let fd = open(&name, libc::O_RDWR | libc::O_CREAT | libc::O_EXCL)?;
        let length = libc::off_t::try_from(len).map_err(io::Error::other);
        let sized = length.and_then(|length| {
            // SAFETY: `fd` is a shared memory object this process just
            // made; sizing it touches no memory of ours.
            if unsafe { libc::ftruncate(fd.as_raw_fd(), length) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        });
        let name = Name(name);
        Ok((sized.and_then(|()| map(&fd, len))?, name))
    }

    /// Opens the memory `name`, of `len` bytes, that the app made, and
    /// removes its name.
    ///
    /// # Errors
    ///
    /// Fails if there is no such memory, it is smaller, or it will not
    /// map.
    pub fn open(name: &str, len: usize) -> io::Result<Self> {
        let fd = open(name, libc::O_RDWR);
        unlink(name);
        let fd = fd?;
        // SAFETY: an all-zero `stat` is a valid value for fstat to fill.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is open, and `stat` is ours to fill.
        if unsafe { libc::fstat(fd.as_raw_fd(), &raw mut stat) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if usize::try_from(stat.st_size).map_or(true, |size| size < len) {
            return Err(io::Error::other(format!(
                "the shared memory holds {} bytes, not {len}",
                stat.st_size
            )));
        }
        map(&fd, len)
    }

    /// Its bytes.
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the mapping holds `len` bytes, readable while `self`
        // lives; the other process writes them only while this one waits.
        unsafe { std::slice::from_raw_parts(self.start.as_ptr(), self.len) }
    }

    /// Its bytes, to write.
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `bytes`, and `&mut self` makes this the only borrow.
        unsafe { std::slice::from_raw_parts_mut(self.start.as_ptr(), self.len) }
    }
}

impl Drop for SharedPixels {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly what `map` mapped, which no borrow
        // outlives.
        unsafe {
            libc::munmap(self.start.as_ptr().cast(), self.len);
        }
    }
}

/// The name of memory this process made, removed when dropped; mappings
/// of the memory last until unmapped.
pub struct Name(String);

impl Name {
    /// The name, as the helper opens it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Drop for Name {
    fn drop(&mut self) {
        unlink(&self.0);
    }
}

/// Removes the memory's name `name`.
fn unlink(name: &str) {
    if let Ok(name) = CString::new(name) {
        // SAFETY: `name` is a C string; a name already gone is no harm.
        unsafe {
            libc::shm_unlink(name.as_ptr());
        }
    }
}

/// Opens the shared memory object `name` with `flags`.
fn open(name: &str, flags: libc::c_int) -> io::Result<OwnedFd> {
    let name = CString::new(name).map_err(io::Error::other)?;
    // SAFETY: `name` is a C string. macOS's shm_open is variadic, taking
    // the mode as an int, as C promotes it.
    #[cfg(target_os = "macos")]
    let fd = unsafe { libc::shm_open(name.as_ptr(), flags, libc::c_uint::from(MODE)) };
    // SAFETY: `name` is a C string.
    #[cfg(not(target_os = "macos"))]
    let fd = unsafe { libc::shm_open(name.as_ptr(), flags, MODE) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: shm_open returned a new descriptor that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Maps `len` bytes of `fd` to read and write, shared with every process
/// that maps it.
fn map(fd: &OwnedFd, len: usize) -> io::Result<SharedPixels> {
    if len == 0 {
        return Err(io::Error::other("no bytes to share"));
    }
    // SAFETY: maps a descriptor this process holds, at an address the
    // system picks; the mapping outlives the descriptor.
    let start = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            0,
        )
    };
    if start == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    let start = NonNull::new(start.cast::<u8>()).ok_or_else(|| io::Error::other("mapped at 0"))?;
    Ok(SharedPixels { start, len })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_one_side_writes_the_other_reads_and_the_name_goes() {
        let (mut made, name) = SharedPixels::create(4096).expect("made");
        let name_text = name.as_str().to_owned();
        made.bytes_mut()[..4].copy_from_slice(b"3DS!");
        let mut opened = SharedPixels::open(&name_text, 4096).expect("opened");
        assert_eq!(&opened.bytes()[..4], b"3DS!");
        opened.bytes_mut()[4095] = 7;
        assert_eq!(made.bytes()[4095], 7);
        // Opening removed the name, so no one else can, and nothing is
        // left once both are dropped.
        assert!(SharedPixels::open(&name_text, 4096).is_err());
        drop(name);
        drop(opened);
        drop(made);
    }

    #[test]
    fn memory_smaller_than_asked_for_is_refused() {
        let (_made, name) = SharedPixels::create(4096).expect("made");
        let error = SharedPixels::open(name.as_str(), 1 << 20)
            .err()
            .expect("too small");
        // macOS rounds the memory up to whole pages, so the size it says
        // is not always 4096.
        assert!(error.to_string().contains("not 1048576"), "{error}");
    }

    #[test]
    fn dropping_the_name_removes_it() {
        let (_made, name) = SharedPixels::create(4096).expect("made");
        let text = name.as_str().to_owned();
        drop(name);
        assert!(SharedPixels::open(&text, 4096).is_err());
    }
}
