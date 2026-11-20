//! The older lines of the scrollback, packed one after another into pages of
//! a fixed size.
//!
//! Lines go in at the new end and come out at the old one. A line takes no
//! memory of its own: it is a record (see [`super::pack`]) in a page, and
//! once every line of the oldest page is past the length of the scrollback,
//! the whole page goes. The pages are kept for reuse, one of them, so that a
//! scrollback that is full asks for no memory as lines go through it.
//!
//! Lines are numbered as they go in. The scrollback's length is kept to the
//! line: a page's lines that are older than the oldest line kept are still
//! in it, but are no longer lines of the scrollback.

use std::collections::VecDeque;

use crate::grid::pack::{Packer, Record, plain_bound};
use crate::term::cell::Cell;

/// The size of a page in bytes: a few hundred lines of text.
pub(crate) const PAGE_BYTES: usize = 64 * 1024;

#[derive(Clone)]
struct Page {
    /// The records, one after another.
    bytes: Vec<u8>,
    /// Where each record starts.
    starts: Vec<u32>,
    /// The number of the page's first line.
    first: u64,
}

impl Page {
    /// The number one past the page's last line.
    #[inline]
    fn end(&self) -> u64 {
        self.first + self.starts.len() as u64
    }
}

#[derive(Clone)]
pub(crate) struct Pages {
    pages: VecDeque<Page>,
    /// A page no longer used, kept for the next one needed.
    spare: Option<Page>,
    page_bytes: usize,
    /// The number the next line that goes in is given.
    next: u64,
    /// The number of the oldest line kept.
    oldest: u64,
    packer: Packer,
    /// Where a line is packed before it goes into a page.
    scratch: Vec<u8>,
}

impl Pages {
    /// Pages of `page_bytes` bytes each.
    pub(crate) fn new(page_bytes: usize) -> Pages {
        Pages {
            pages: VecDeque::new(),
            spare: None,
            page_bytes: page_bytes.max(64),
            next: 0,
            oldest: 0,
            packer: Packer::default(),
            scratch: Vec::new(),
        }
    }

    /// The size of a page.
    pub(crate) fn page_bytes(&self) -> usize {
        self.page_bytes
    }

    /// Whether no line is kept.
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.next == self.oldest
    }

    /// Gives back the room packing a long line made, beyond the usual.
    pub(crate) fn shrink_scratch(&mut self) {
        if self.scratch.capacity() > self.page_bytes {
            self.scratch = Vec::new();
        }
    }

    /// How many lines are kept.
    #[inline]
    pub(crate) fn len(&self) -> usize {
        (self.next - self.oldest) as usize
    }

    /// Packs the line `cells` as the newest. The cells from `occupied` on
    /// are default ones; with `extras` false none of them has an extra.
    #[cfg(test)]
    pub(crate) fn push(&mut self, cells: &[Cell], occupied: usize, extras: bool) {
        self.push_marked(cells, occupied, extras, 0);
    }

    pub(crate) fn push_marked(&mut self, cells: &[Cell], occupied: usize, extras: bool, mark: u8) {
        // A line with no extras is packed straight into the page, if the page has room for the
        // most it can take. Any other is packed aside first, and goes into the page if it fits,
        // or else into a new one: so that the end of a page is used too.
        if !extras {
            let bound = plain_bound(occupied.min(cells.len()));
            if let Some(page) = self.pages.back_mut()
                && page.bytes.capacity() - page.bytes.len() >= bound
            {
                page.starts.push(page.bytes.len() as u32);
                self.packer.pack_marked(cells, occupied, false, mark, &mut page.bytes);
                self.next += 1;
                return;
            }
        }
        self.scratch.clear();
        self.packer.pack_marked(cells, occupied, extras, mark, &mut self.scratch);
        let len = self.scratch.len();
        let fits = self
            .pages
            .back()
            .is_some_and(|page| page.bytes.len() + len <= page.bytes.capacity().max(self.page_bytes));
        if !fits {
            self.new_page(len);
        }
        let page = self.pages.back_mut().expect("a page with room");
        page.starts.push(page.bytes.len() as u32);
        page.bytes.extend_from_slice(&self.scratch);
        self.next += 1;
    }

    /// Starts a page for lines from the next on, with room for a record of
    /// `len` bytes at least.
    fn new_page(&mut self, len: usize) {
        let mut page = match self.spare.take() {
            Some(page) if page.bytes.capacity() >= len => page,
            _ => Page {
                bytes: Vec::with_capacity(self.page_bytes.max(len)),
                starts: Vec::new(),
                first: 0,
            },
        };
        page.bytes.clear();
        page.starts.clear();
        page.first = self.next;
        self.pages.push_back(page);
    }

    /// The line `back` lines older than the newest.
    #[inline]
    pub(crate) fn get(&self, back: usize) -> Record<'_> {
        debug_assert!(back < self.len());
        let number = self.next - 1 - back as u64;
        // The newest lines are the ones read most: look from that end.
        let mut index = self.pages.len() - 1;
        while self.pages[index].first > number {
            index -= 1;
        }
        let page = &self.pages[index];
        let start = page.starts[(number - page.first) as usize] as usize;
        Record::at(&page.bytes[start..])
    }

    /// Forgets the `count` oldest lines.
    pub(crate) fn drop_oldest(&mut self, count: usize) {
        self.oldest = (self.oldest + count as u64).min(self.next);
        while self.pages.front().is_some_and(|page| page.end() <= self.oldest) {
            let page = self.pages.pop_front().expect("the page looked at");
            self.recycle(page);
        }
    }

    /// Forgets every line.
    pub(crate) fn clear(&mut self) {
        self.drop_oldest(self.len());
        debug_assert!(self.pages.is_empty());
    }

    fn recycle(&mut self, page: Page) {
        // Only a page of the usual size is kept: one made larger for a long
        // record would hold on to its memory for nothing.
        if self.spare.is_none() && page.bytes.capacity() <= self.page_bytes {
            self.spare = Some(page);
        }
    }

    /// The bytes the pages hold, for tests and measurements.
    pub(crate) fn bytes(&self) -> usize {
        self.pages.iter().map(|page| page.bytes.capacity() + page.starts.capacity() * 4).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, columns: usize) -> Vec<Cell> {
        let mut cells: Vec<Cell> = (0..columns).map(|_| Cell::default()).collect();
        for (at, c) in text.chars().enumerate() {
            cells[at].set_c(c);
        }
        cells
    }

    fn text(record: Record<'_>) -> String {
        String::from_utf8(record.text().to_vec()).unwrap()
    }

    #[test]
    fn lines_come_back_newest_first_across_pages() {
        // Pages of 64 bytes hold two short lines each.
        let mut pages = Pages::new(64);
        for i in 0..100 {
            pages.push(&line(&format!("line {i}"), 20), 20, false);
        }
        assert_eq!(pages.len(), 100);
        for back in 0..100 {
            assert_eq!(text(pages.get(back)), format!("line {}", 99 - back));
        }

        // The oldest go, a page at a time in memory and to the line in count.
        pages.drop_oldest(31);
        assert_eq!(pages.len(), 69);
        assert_eq!(text(pages.get(68)), "line 31");
        assert_eq!(text(pages.get(0)), "line 99");


        pages.clear();
        assert_eq!(pages.len(), 0);
        pages.push(&line("after", 20), 20, false);
        assert_eq!(text(pages.get(0)), "after");
    }

    #[test]
    fn a_line_longer_than_a_page_gets_a_page_of_its_own() {
        let mut pages = Pages::new(64);
        let long: String = "x".repeat(500);
        pages.push(&line("before", 600), 600, false);
        pages.push(&line(&long, 600), 600, false);
        pages.push(&line("after", 600), 600, false);
        assert_eq!(text(pages.get(1)), long);
        assert_eq!(text(pages.get(0)), "after");
        assert_eq!(text(pages.get(2)), "before");
        let cells: Vec<_> = pages.get(1).cells(600).map(|cell| cell.c).collect();
        assert_eq!(cells.len(), 600);
        assert!(cells[..500].iter().all(|&c| c == 'x') && cells[500..].iter().all(|&c| c == ' '));
    }

    #[test]
    fn a_full_scrollback_asks_for_no_new_pages() {
        let mut pages = Pages::new(4096);
        for i in 0..10_000 {
            pages.push(&line(&format!("a line of text {i}"), 80), 80, false);
            if pages.len() > 1000 {
                pages.drop_oldest(1);
            }
        }
        // As many pages as a thousand lines take, give or take the one the
        // oldest line is in, and no more however many lines go through.
        let mut most = 0;
        for i in 0..10_000 {
            pages.push(&line(&format!("a line of text {i}"), 80), 80, false);
            pages.drop_oldest(1);
            most = most.max(pages.bytes());
        }
        let page = 4096 + 4 * 128;
        assert!(most <= 1000 * 60 + 2 * page, "{most} bytes for 1000 lines");
        for i in 0..10_000 {
            pages.push(&line(&format!("a line of text {i}"), 80), 80, false);
            pages.drop_oldest(1);
            assert!(pages.bytes() <= most);
        }
        assert_eq!(pages.len(), 1000);
    }
}
