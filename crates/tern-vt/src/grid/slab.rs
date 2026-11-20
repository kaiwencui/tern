//! The rows of cells a grid has: the lines of its screen, and the newest
//! lines of its scrollback, which are kept as cells until they are packed.
//! Each row is a [`Meta`] and then its cells; which line a row holds is the
//! grid's business.
//!
//! The rows are a base, enough for the screen and a few dozen lines of
//! scrollback, and while output pours in, chunks of more, each its own
//! allocation large enough that giving it back returns its memory to the
//! system at once.

use std::mem::MaybeUninit;
use std::ptr::NonNull;

use crate::grid::row::{Meta, Row};
use crate::term::cell::Cell;

/// One cell's room.
type Slot = MaybeUninit<[u32; 4]>;

const _: () = assert!(std::mem::size_of::<Slot>() == std::mem::size_of::<Cell>());
const _: () = assert!(std::mem::align_of::<Slot>() == std::mem::align_of::<Cell>());

/// `rows` rows of a meta and `columns` blank cells each.
fn block(rows: usize, columns: usize) -> Box<[Slot]> {
    let mut slots: Box<[Slot]> = std::iter::repeat_with(Slot::uninit).take(rows * (columns + 1)).collect();
    for row in slots.chunks_exact_mut(columns + 1) {
        // SAFETY: a slot has the size and alignment of a meta, and of a cell.
        unsafe {
            row[0].as_mut_ptr().cast::<Meta>().write(Meta::default());
            for slot in &mut row[1..] {
                slot.as_mut_ptr().cast::<Cell>().write(Cell::default());
            }
        }
    }
    slots
}

/// Drops the cells of every row of `slots`.
fn drop_block(slots: &mut [Slot], columns: usize) {
    for row in slots.chunks_exact_mut(columns + 1) {
        // SAFETY: the cells were initialized by `block` and are not used again.
        unsafe {
            let cells = std::ptr::slice_from_raw_parts_mut(row[1..].as_mut_ptr().cast::<Cell>(), columns);
            std::ptr::drop_in_place(cells);
        }
    }
}

/// Rows mapped from the system on their own, and given back to it whole.
struct Chunk {
    slots: NonNull<Slot>,
    len: usize,
}

impl Chunk {
    /// `rows` rows of a meta and `columns` blank cells each.
    fn new(rows: usize, columns: usize) -> Chunk {
        let len = rows * (columns + 1);
        let bytes = len * std::mem::size_of::<Slot>();
        // SAFETY: an anonymous private mapping, of memory no one else has.
        let at = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bytes,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        assert!(at != libc::MAP_FAILED, "no memory for {rows} rows of the scrollback");
        // SAFETY: the mapping is `len` slots, aligned to a page.
        let slots = unsafe { std::slice::from_raw_parts_mut(at.cast::<Slot>(), len) };
        for row in slots.chunks_exact_mut(columns + 1) {
            // SAFETY: a slot has the size and alignment of a meta, and of a cell.
            unsafe {
                row[0].as_mut_ptr().cast::<Meta>().write(Meta::default());
                for slot in &mut row[1..] {
                    slot.as_mut_ptr().cast::<Cell>().write(Cell::default());
                }
            }
        }
        Chunk { slots: NonNull::new(at.cast()).expect("a mapping"), len }
    }

    fn slots(&mut self) -> &mut [Slot] {
        // SAFETY: the chunk's own mapping.
        unsafe { std::slice::from_raw_parts_mut(self.slots.as_ptr(), self.len) }
    }

    /// Drops the cells and gives the memory back.
    fn free(mut self, columns: usize) {
        drop_block(self.slots(), columns);
        // SAFETY: the mapping made in `new`, used no more.
        unsafe { libc::munmap(self.slots.as_ptr().cast(), self.len * std::mem::size_of::<Slot>()) };
    }
}

pub(crate) struct Slab {
    base: Box<[Slot]>,
    chunks: Vec<Chunk>,
    /// Where each row starts, the base's first.
    starts: Vec<NonNull<Slot>>,
    base_rows: usize,
    columns: usize,
}

// SAFETY: a slab owns the memory its row pointers point into, as boxes of cells would, and
// hands it out only through `&self` and `&mut self`.
unsafe impl Send for Slab {}
// SAFETY: as above.
unsafe impl Sync for Slab {}

impl Slab {
    /// `rows` rows of `columns` blank cells.
    pub(crate) fn new(rows: usize, columns: usize) -> Slab {
        let mut base = block(rows, columns);
        let stride = columns + 1;
        let starts = (0..rows)
            // SAFETY: within the base, which is never null.
            .map(|row| unsafe { NonNull::new_unchecked(base.as_mut_ptr().add(row * stride)) })
            .collect();
        Slab { base, chunks: Vec::new(), starts, base_rows: rows, columns }
    }

    /// How many rows there are, chunks and all.
    #[inline]
    pub(crate) fn rows(&self) -> usize {
        self.starts.len()
    }

    /// How many rows the base has.
    #[inline]
    pub(crate) fn base_rows(&self) -> usize {
        self.base_rows
    }

    /// Adds a chunk of `rows` blank rows; returns the number of the first.
    pub(crate) fn grow(&mut self, rows: usize) -> usize {
        let first = self.starts.len();
        let chunk = Chunk::new(rows, self.columns);
        let stride = self.columns + 1;
        for row in 0..rows {
            // SAFETY: within the chunk's mapping, which is never null and does not move.
            self.starts.push(unsafe { NonNull::new_unchecked(chunk.slots.as_ptr().add(row * stride)) });
        }
        self.chunks.push(chunk);
        first
    }

    /// Gives back every chunk: no row past the base may be in use.
    pub(crate) fn drop_chunks(&mut self) {
        for chunk in self.chunks.drain(..) {
            chunk.free(self.columns);
        }
        self.starts.truncate(self.base_rows);
    }

    /// Swaps two rows, meta and cells.
    pub(crate) fn swap(&mut self, a: usize, b: usize) {
        assert!(a != b);
        let len = self.columns + 1;
        // SAFETY: the two rows are distinct, so they do not overlap; their cells change places
        // and so their owners.
        unsafe { std::ptr::swap_nonoverlapping(self.starts[a].as_ptr(), self.starts[b].as_ptr(), len) };
    }

    #[inline]
    pub(crate) fn row(&self, row: usize) -> &Row {
        // SAFETY: the row's slots are its meta and its cells, initialized by `block`, and are
        // borrowed with the slab.
        unsafe { Row::from_raw(self.starts[row].as_ptr().cast(), self.columns) }
    }

    #[inline]
    pub(crate) fn row_mut(&mut self, row: usize) -> &mut Row {
        // SAFETY: as in `row`, borrowed mutably with the slab.
        unsafe { Row::from_raw_mut(self.starts[row].as_ptr().cast(), self.columns) }
    }
}

impl Clone for Slab {
    fn clone(&self) -> Slab {
        let mut slab = Slab::new(self.base_rows, self.columns);
        let mut left = self.rows() - self.base_rows;
        for chunk in &self.chunks {
            let rows = chunk.len / (self.columns + 1);
            slab.grow(rows);
            left -= rows;
        }
        debug_assert_eq!(left, 0);
        for row in 0..self.rows() {
            let from = self.row(row);
            let to = slab.row_mut(row);
            for (to, from) in to[..].iter_mut().zip(from.cells()) {
                *to = from.clone();
            }
            to.meta = from.meta;
        }
        slab
    }
}

impl Drop for Slab {
    fn drop(&mut self) {
        self.drop_chunks();
        drop_block(&mut self.base, self.columns);
    }
}
