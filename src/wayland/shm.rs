//! A `wl_shm`-backed pixel buffer pool.
//!
//! We never paint more than one popup at a time, so a single buffer is
//! recycled. The pool exposes `slice`/`slice_mut` views into the mapping; the
//! caller writes into it then hands the offset to wayland as a `wl_buffer`.

use std::fs::File;

use memmap2::{MmapMut, MmapOptions};

use crate::errors::Result;

/// Size in bytes of a single shm buffer. 2 MiB covers 800 x 256 RGBA.
pub const POOL_SIZE: usize = 2 * 1024 * 1024;

/// Bytes per pixel in the RGBA8 buffers this daemon renders.
pub const BYTES_PER_PIXEL: usize = 4;

pub struct ShmPool {
    // The fd must outlive the mapping; dropping it first would leave the
    // mapping pointing at a recycled descriptor.
    _file: File,
    map: MmapMut,
}

impl ShmPool {
    pub fn create() -> Result<Self> {
        let file = tempfile::tempfile()?;
        file.set_len(POOL_SIZE as u64)?;
        // SAFETY: `file` is a freshly created anonymous tempfile opened
        // read-write. Nothing else holds a descriptor to it, so no other
        // process can write through this mapping. `MmapMut` is required (not
        // `Mmap`) because `Mmap` deliberately does not implement `DerefMut` —
        // we need `&mut [u8]` to paint. The mapping is stored alongside the
        // `File` in `Self`, so the descriptor outlives it and there is never a
        // second aliasing `&mut` to the same region.
        let map = unsafe { MmapOptions::new().len(POOL_SIZE).map_mut(&file)? };
        Ok(Self { _file: file, map })
    }

    pub fn slice(&self, offset: usize, len: usize) -> &[u8] {
        &self.map[offset..offset + len]
    }

    pub fn slice_mut(&mut self, offset: usize, len: usize) -> &mut [u8] {
        &mut self.map[offset..offset + len]
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Fill the `w` x `h` region at the start of `buf` with `rgba`, row by row.
///
/// Stops at the end of `buf` rather than panicking if the buffer is smaller
/// than `w * h * 4`; a short buffer is a caller bug, but painting a clipped
/// region is more useful in a daemon than an abort.
pub fn clear(buf: &mut [u8], w: u32, h: u32, rgba: [u8; 4]) {
    let stride = w as usize * BYTES_PER_PIXEL;
    for y in 0..h as usize {
        let start = y * stride;
        let Some(row) = buf.get_mut(start..start + stride) else {
            return;
        };
        for px in row.chunks_exact_mut(BYTES_PER_PIXEL) {
            px.copy_from_slice(&rgba);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_round_trip() {
        let mut p = ShmPool::create().expect("create");
        assert_eq!(p.slice(0, 4).len(), 4);
        p.slice_mut(0, 4).copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(p.slice(0, 4), &[0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn pool_is_poole_size_and_starts_zeroed() {
        let p = ShmPool::create().expect("create");
        assert_eq!(p.len(), POOL_SIZE);
        assert!(p.slice(0, 64).iter().all(|&b| b == 0));
    }

    #[test]
    fn clear_writes_color_to_every_pixel() {
        let mut buf = vec![0u8; 4 * 3];
        clear(&mut buf, 1, 3, [1, 2, 3, 4]);
        assert_eq!(buf, vec![1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4]);
    }

    #[test]
    fn clear_leaves_trailing_buffer_untouched() {
        // Buffer holds 4x4 pixels; we only clear a 2x2 top-left region.
        let mut buf = vec![0u8; 4 * 4 * 4];
        clear(&mut buf, 2, 2, [9, 9, 9, 9]);
        let painted = 2 * 2 * 4;
        assert!(buf[..painted].iter().all(|&b| b == 9));
        assert!(buf[painted..].iter().all(|&b| b == 0));
    }

    #[test]
    fn clear_does_not_panic_on_short_buffer() {
        // Buffer is too small for even one full row (stride 16 > len 8).
        let mut buf = vec![0u8; 8];
        clear(&mut buf, 4, 4, [1, 1, 1, 1]);
        assert_eq!(buf, vec![0u8; 8], "must paint nothing, not a partial row");
    }

    #[test]
    fn clear_paints_full_rows_then_stops_at_clip() {
        // w=1 -> stride 4. Buffer holds 2 full rows; a third row is clipped.
        let mut buf = vec![0u8; 8];
        clear(&mut buf, 1, 3, [1, 1, 1, 1]);
        assert_eq!(buf, vec![1, 1, 1, 1, 1, 1, 1, 1]);
    }
}
