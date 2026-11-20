use std::fmt;
use std::ops::{
    Index, IndexMut, Range, RangeFrom, RangeFull, RangeInclusive, RangeTo, RangeToInclusive,
};

use crate::index::Column;
use crate::term::cell::{Cell, DEFAULT_BG_WORD, DEFAULT_FG_WORD, Flags};

pub const MARK_PROMPT: u8 = 1 << 0;
pub const MARK_OUTPUT: u8 = 1 << 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Meta {
    pub(crate) occ: u32,
    pub(crate) unclean: u32,
    pub(crate) mark: u32,
    _pad: u32,
}

const _: () = assert!(std::mem::size_of::<Meta>() == std::mem::size_of::<Cell>());
const _: () = assert!(std::mem::align_of::<Meta>() == std::mem::align_of::<Cell>());

#[repr(C)]
pub struct Row {
    pub(crate) meta: Meta,
    cells: [Cell],
}

impl Row {
    #[inline]
    pub(crate) unsafe fn from_raw<'a>(slot: *const Cell, columns: usize) -> &'a Row {
        // SAFETY: caller guarantees `slot` points to an initialized `Meta` followed by
        // `columns` initialized `Cell`s valid for `'a`.
        unsafe { &*(std::ptr::slice_from_raw_parts(slot, columns) as *const Row) }
    }

    #[inline]
    pub(crate) unsafe fn from_raw_mut<'a>(slot: *mut Cell, columns: usize) -> &'a mut Row {
        // SAFETY: caller guarantees `slot` points to an initialized `Meta` followed by
        // `columns` initialized `Cell`s uniquely borrowed for `'a`.
        unsafe { &mut *(std::ptr::slice_from_raw_parts_mut(slot, columns) as *mut Row) }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    #[inline]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    #[inline]
    pub(crate) fn occupied(&self) -> usize {
        (self.meta.occ as usize).min(self.len())
    }

    #[inline]
    pub(crate) fn unclean(&self) -> bool {
        self.meta.unclean != 0
    }

    #[inline]
    pub fn mark(&self) -> u8 {
        self.meta.mark as u8
    }

    #[inline]
    pub fn add_mark(&mut self, bits: u8) {
        self.meta.mark |= u32::from(bits);
    }

    #[inline]
    pub fn set_mark(&mut self, mark: u8) {
        self.meta.mark = u32::from(mark);
    }

    #[inline]
    pub(crate) fn occupy(&mut self, col: usize) {
        let clamped = col.min(self.len()) as u32;
        if clamped > self.meta.occ {
            self.meta.occ = clamped;
        }
    }

    pub fn reset(&mut self, template: &Cell) {
        let len = self.len();
        if len == 0 {
            self.meta.occ = 0;
            self.meta.unclean = 0;
            self.meta.mark = 0;
            return;
        }
        let old_occ = (self.meta.occ as usize).min(len);
        let end = if self.cells[len - 1].bg_word() == template.bg_word() { old_occ } else { len };
        if end > 0 {
            if self.unclean() {
                let blank = template.erased();
                self.cells[..end].fill(blank);
            } else {
                // SAFETY: `!self.unclean()` guarantees no cell in `self` holds a `CellExtra`.
                unsafe {
                    fill_erased(&mut self.cells[..end], template.bg_word());
                }
            }
        }
        self.meta.occ = if template.bg_word() == DEFAULT_BG_WORD { 0 } else { len as u32 };
        self.meta.unclean = 0;
        self.meta.mark = 0;
    }

    pub(crate) fn reset_after(&mut self, template: &Cell, keep: usize) -> usize {
        if self.unclean() {
            self.reset(template);
            return 0;
        }
        self.meta.mark = 0;
        let len = self.len();
        if len == 0 {
            self.meta.occ = 0;
            return 0;
        }
        let keep = keep.min(len);
        let old_occ = (self.meta.occ as usize).min(len);
        let full_bg = self.cells[len - 1].bg_word() != template.bg_word();
        let end = if full_bg { len } else { old_occ };
        if keep < end {
            // SAFETY: `!self.unclean()` guarantees no cell in `self` holds a `CellExtra`.
            unsafe {
                fill_erased(&mut self.cells[keep..end], template.bg_word());
            }
        }
        self.meta.occ = if (keep < len && template.bg_word() != DEFAULT_BG_WORD)
            || (full_bg && keep == len)
        {
            len as u32
        } else {
            old_occ.min(keep) as u32
        };
        keep
    }

    pub fn erase_from(&mut self, col: usize, template: &Cell) {
        let len = self.len();
        let col = col.min(len);
        if col == 0 {
            self.reset(template);
            return;
        }
        if self.unclean() {
            let old_occ = (self.meta.occ as usize).min(len);
            let full_bg = self.cells[len - 1].bg_word() != template.bg_word();
            let end = if full_bg { len } else { old_occ };
            if col < end {
                let blank = template.erased();
                self.cells[col..end].fill(blank);
            }
            self.meta.occ = if template.bg_word() == DEFAULT_BG_WORD {
                old_occ.min(col) as u32
            } else {
                len as u32
            };
        } else {
            self.reset_after(template, col);
        }
    }

    #[inline]
    pub fn is_clear(&self) -> bool {
        let occ = self.occupied();
        occ == 0 || self.cells[..occ].iter().all(Cell::is_blank)
    }

    #[inline]
    pub(crate) fn prefetch(&self) {
        #[cfg(target_arch = "aarch64")]
        {
            let base = (&raw const self.meta).cast::<u8>();
            let bytes = (self.len() + 1) * std::mem::size_of::<Cell>();
            let mut offset = 0;
            while offset < bytes {
                // SAFETY: `prfm` is an architectural cache hint that never faults or alters state,
                // and `base.add(offset)` lies within the live row allocation.
                unsafe {
                    let ptr = base.add(offset);
                    std::arch::asm!(
                        "prfm pstl1keep, [{ptr}]",
                        ptr = in(reg) ptr,
                        options(nostack, preserves_flags),
                    );
                }
                offset += 64;
            }
        }
    }

    #[inline]
    pub fn write_ascii(&mut self, col: usize, bytes: &[u8], style: &Cell) {
        if bytes.is_empty() {
            return;
        }
        let end = col + bytes.len();
        self.occupy(end);
        let unclean = self.unclean();
        let has_extra = style.has_extra();
        if unclean || has_extra {
            if has_extra {
                self.meta.unclean = 1;
            }
            for (cell, &byte) in self.cells[col..end].iter_mut().zip(bytes) {
                *cell = style.clone();
                cell.set_c(byte as char);
            }
            return;
        }
        let dst = &mut self.cells[col..end];
        // SAFETY: `!unclean` and `!has_extra`, so neither existing cells nor `style`
        // own a `CellExtra` entry, and `dst.len() == bytes.len()`.
        unsafe {
            expand_ascii(dst, bytes, style.fg_word(), style.bg_word(), style.flags.bits() as u32);
        }
    }

    #[inline]
    pub(crate) fn write_tab(&mut self, col: usize, tab: char) {
        let cell = &mut self.cells[col];
        if cell.c() == ' '
            && !cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            cell.set_c(tab);
            self.occupy(col + 1);
        }
    }

    #[inline]
    pub(crate) fn text<'a>(&'a mut self, col: usize, style: &Cell) -> TextWriter<'a> {
        let clean = !self.unclean() && !style.has_extra();
        if style.has_extra() {
            self.meta.unclean = 1;
        }
        TextWriter {
            row: self,
            start: col,
            at: col,
            fg: style.fg_word(),
            bg: style.bg_word(),
            flags: style.flags,
            style_extra: if style.has_extra() { Some(style.clone()) } else { None },
            clean,
        }
    }

    #[inline]
    pub(crate) fn set_wrapline(&mut self) {
        let len = self.len();
        self.cells[len - 1].flags.insert(Flags::WRAPLINE);
        self.meta.occ = len as u32;
    }
}

pub(crate) struct TextWriter<'a> {
    row: &'a mut Row,
    start: usize,
    at: usize,
    fg: u32,
    bg: u32,
    flags: Flags,
    style_extra: Option<Cell>,
    clean: bool,
}

impl TextWriter<'_> {
    #[inline]
    pub(crate) fn flags(&self, col: usize) -> Flags {
        self.row.cells[col].flags
    }

    #[inline]
    pub(crate) fn push(&mut self, c: char, wide: bool) {
        let at = self.at;
        if self.clean {
            if wide {
                let first = [c as u32, self.fg, self.bg, (self.flags | Flags::WIDE_CHAR).bits() as u32];
                let spacer = [
                    ' ' as u32,
                    self.fg,
                    self.bg,
                    (self.flags | Flags::WIDE_CHAR_SPACER).bits() as u32,
                ];
                let _ = &mut self.row.cells[at..at + 2];
                // SAFETY: `self.clean` guarantees no `CellExtra` in `self.row`, and bounds of
                // `at..at + 2` were checked above.
                unsafe {
                    let ptr = self.row.cells.as_mut_ptr().add(at).cast::<[u32; 4]>();
                    ptr.write(first);
                    ptr.add(1).write(spacer);
                }
                self.at = at + 2;
            } else {
                let words = [c as u32, self.fg, self.bg, self.flags.bits() as u32];
                let cell = &mut self.row.cells[at];
                // SAFETY: `self.clean` guarantees `cell` holds no `CellExtra`.
                unsafe {
                    (cell as *mut Cell).cast::<[u32; 4]>().write(words);
                }
                self.at = at + 1;
            }
        } else if let Some(template) = &self.style_extra {
            if wide {
                let mut first = template.clone();
                first.set_c(c);
                first.flags.insert(Flags::WIDE_CHAR);
                let mut spacer = template.clone();
                spacer.set_c(' ');
                spacer.flags.insert(Flags::WIDE_CHAR_SPACER);
                self.row.cells[at] = first;
                self.row.cells[at + 1] = spacer;
                self.at = at + 2;
            } else {
                let mut single = template.clone();
                single.set_c(c);
                self.row.cells[at] = single;
                self.at = at + 1;
            }
        } else if wide {
            self.row.cells[at] = Cell::from_words(c, self.fg, self.bg, self.flags | Flags::WIDE_CHAR);
            self.row.cells[at + 1] =
                Cell::from_words(' ', self.fg, self.bg, self.flags | Flags::WIDE_CHAR_SPACER);
            self.at = at + 2;
        } else {
            self.row.cells[at] = Cell::from_words(c, self.fg, self.bg, self.flags);
            self.at = at + 1;
        }
    }
}

impl Drop for TextWriter<'_> {
    #[inline]
    fn drop(&mut self) {
        if self.at > self.start {
            self.row.occupy(self.at);
        }
    }
}

#[inline]
unsafe fn fill_erased(cells: &mut [Cell], bg: u32) {
    let len = cells.len();
    let mut i = 0;

    #[cfg(target_arch = "aarch64")]
    // SAFETY: `cells` is a contiguous slice of 16-byte `Cell`s with no live `CellExtra`.
    unsafe {
        use std::arch::aarch64::{uint32x4x4_t, vdupq_n_u32, vst4q_u32};

        let c = vdupq_n_u32(' ' as u32);
        let fg = vdupq_n_u32(DEFAULT_FG_WORD);
        let bg_vec = vdupq_n_u32(bg);
        let fl = vdupq_n_u32(0);
        let quad = uint32x4x4_t(c, fg, bg_vec, fl);
        let out = cells.as_mut_ptr().cast::<u32>();
        while i + 4 <= len {
            vst4q_u32(out.add(i * 4), quad);
            i += 4;
        }
    }

    let words = [' ' as u32, DEFAULT_FG_WORD, bg, 0];
    // SAFETY: `i..len` is in bounds of `cells` and each `Cell` is 16 bytes aligned to 4.
    unsafe {
        let base = cells.as_mut_ptr().cast::<[u32; 4]>();
        while i < len {
            base.add(i).write(words);
            i += 1;
        }
    }
}

#[inline]
unsafe fn expand_ascii(dst: &mut [Cell], bytes: &[u8], fg: u32, bg: u32, flags: u32) {
    debug_assert_eq!(dst.len(), bytes.len());
    let len = bytes.len();
    let mut i = 0;

    #[cfg(target_arch = "aarch64")]
    // SAFETY: `dst` and `bytes` both have length `len`, and `dst` holds no live `CellExtra`.
    unsafe {
        use std::arch::aarch64::{
            uint32x4x4_t, vdup_n_u32, vdupq_n_u32, vget_high_u16, vget_high_u8, vget_low_u16,
            vget_low_u8, vld1q_u8, vmovl_u16, vmovl_u8, vreinterpret_u8_u32, vst4q_u32,
        };

        let fg_vec = vdupq_n_u32(fg);
        let bg_vec = vdupq_n_u32(bg);
        let fl_vec = vdupq_n_u32(flags);
        let src = bytes.as_ptr();
        let out = dst.as_mut_ptr().cast::<u32>();

        while i + 16 <= len {
            let b16 = vld1q_u8(src.add(i));
            let lo16 = vmovl_u8(vget_low_u8(b16));
            let hi16 = vmovl_u8(vget_high_u8(b16));
            let c0 = vmovl_u16(vget_low_u16(lo16));
            let c1 = vmovl_u16(vget_high_u16(lo16));
            let c2 = vmovl_u16(vget_low_u16(hi16));
            let c3 = vmovl_u16(vget_high_u16(hi16));
            vst4q_u32(out.add(i * 4), uint32x4x4_t(c0, fg_vec, bg_vec, fl_vec));
            vst4q_u32(out.add((i + 4) * 4), uint32x4x4_t(c1, fg_vec, bg_vec, fl_vec));
            vst4q_u32(out.add((i + 8) * 4), uint32x4x4_t(c2, fg_vec, bg_vec, fl_vec));
            vst4q_u32(out.add((i + 12) * 4), uint32x4x4_t(c3, fg_vec, bg_vec, fl_vec));
            i += 16;
        }

        while i + 4 <= len {
            let word = u32::from_le_bytes([
                *src.add(i),
                *src.add(i + 1),
                *src.add(i + 2),
                *src.add(i + 3),
            ]);
            let b8 = vreinterpret_u8_u32(vdup_n_u32(word));
            let c0 = vmovl_u16(vget_low_u16(vmovl_u8(b8)));
            vst4q_u32(out.add(i * 4), uint32x4x4_t(c0, fg_vec, bg_vec, fl_vec));
            i += 4;
        }
    }

    // SAFETY: `i..len` is in bounds of `dst` and `bytes`.
    unsafe {
        let base = dst.as_mut_ptr().cast::<[u32; 4]>();
        while i < len {
            base.add(i).write([*bytes.get_unchecked(i) as u32, fg, bg, flags]);
            i += 1;
        }
    }
}

impl Index<Column> for Row {
    type Output = Cell;

    #[inline]
    fn index(&self, index: Column) -> &Cell {
        &self.cells[index.0]
    }
}

impl IndexMut<Column> for Row {
    #[inline]
    fn index_mut(&mut self, index: Column) -> &mut Cell {
        self.occupy(index.0 + 1);
        self.meta.unclean = 1;
        &mut self.cells[index.0]
    }
}

impl Index<Range<Column>> for Row {
    type Output = [Cell];

    #[inline]
    fn index(&self, index: Range<Column>) -> &[Cell] {
        &self.cells[index.start.0..index.end.0]
    }
}

impl IndexMut<Range<Column>> for Row {
    #[inline]
    fn index_mut(&mut self, index: Range<Column>) -> &mut [Cell] {
        self.occupy(index.end.0);
        self.meta.unclean = 1;
        &mut self.cells[index.start.0..index.end.0]
    }
}

impl Index<RangeTo<Column>> for Row {
    type Output = [Cell];

    #[inline]
    fn index(&self, index: RangeTo<Column>) -> &[Cell] {
        &self.cells[..index.end.0]
    }
}

impl IndexMut<RangeTo<Column>> for Row {
    #[inline]
    fn index_mut(&mut self, index: RangeTo<Column>) -> &mut [Cell] {
        self.occupy(index.end.0);
        self.meta.unclean = 1;
        &mut self.cells[..index.end.0]
    }
}

impl Index<RangeFrom<Column>> for Row {
    type Output = [Cell];

    #[inline]
    fn index(&self, index: RangeFrom<Column>) -> &[Cell] {
        &self.cells[index.start.0..]
    }
}

impl IndexMut<RangeFrom<Column>> for Row {
    #[inline]
    fn index_mut(&mut self, index: RangeFrom<Column>) -> &mut [Cell] {
        self.meta.occ = self.len() as u32;
        self.meta.unclean = 1;
        &mut self.cells[index.start.0..]
    }
}

impl Index<RangeFull> for Row {
    type Output = [Cell];

    #[inline]
    fn index(&self, _: RangeFull) -> &[Cell] {
        &self.cells
    }
}

impl IndexMut<RangeFull> for Row {
    #[inline]
    fn index_mut(&mut self, _: RangeFull) -> &mut [Cell] {
        self.meta.occ = self.len() as u32;
        self.meta.unclean = 1;
        &mut self.cells
    }
}

impl Index<RangeToInclusive<Column>> for Row {
    type Output = [Cell];

    #[inline]
    fn index(&self, index: RangeToInclusive<Column>) -> &[Cell] {
        &self.cells[..=index.end.0]
    }
}

impl IndexMut<RangeToInclusive<Column>> for Row {
    #[inline]
    fn index_mut(&mut self, index: RangeToInclusive<Column>) -> &mut [Cell] {
        self.occupy(index.end.0 + 1);
        self.meta.unclean = 1;
        &mut self.cells[..=index.end.0]
    }
}

impl Index<RangeInclusive<Column>> for Row {
    type Output = [Cell];

    #[inline]
    fn index(&self, index: RangeInclusive<Column>) -> &[Cell] {
        &self.cells[index.start().0..=index.end().0]
    }
}

impl IndexMut<RangeInclusive<Column>> for Row {
    #[inline]
    fn index_mut(&mut self, index: RangeInclusive<Column>) -> &mut [Cell] {
        self.occupy(index.end().0 + 1);
        self.meta.unclean = 1;
        &mut self.cells[index.start().0..=index.end().0]
    }
}

impl<'a> IntoIterator for &'a Row {
    type Item = &'a Cell;
    type IntoIter = std::slice::Iter<'a, Cell>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.cells.iter()
    }
}

impl<'a> IntoIterator for &'a mut Row {
    type Item = &'a mut Cell;
    type IntoIter = std::slice::IterMut<'a, Cell>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.meta.occ = self.len() as u32;
        self.meta.unclean = 1;
        self.cells.iter_mut()
    }
}

impl PartialEq for Row {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.cells == other.cells
    }
}

impl Eq for Row {}

impl fmt::Debug for Row {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.cells.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::{Color, NamedColor};
    use crate::grid::slab::Slab;

    #[test]
    fn dst_layout_and_ascii_expansion() {
        let mut slab = Slab::new(1, 37);
        let row = slab.row_mut(0);
        assert_eq!(row.len(), 37);
        assert!(row.is_clear());
        assert_eq!(row.occupied(), 0);
        assert!(!row.unclean());

        let style = Cell::new(' ', Color::Indexed(12), Color::Indexed(34), Flags::BOLD);
        let msg = b"0123456789abcdefghijklmnopqrstuvwxyz!";
        row.write_ascii(0, msg, &style);
        assert_eq!(row.occupied(), 37);
        assert!(!row.unclean());
        assert!(!row.is_clear());
        for (i, &b) in msg.iter().enumerate() {
            let cell = &row[Column(i)];
            assert_eq!(cell.c(), b as char);
            assert_eq!(cell.fg(), Color::Indexed(12));
            assert_eq!(cell.bg(), Color::Indexed(34));
            assert_eq!(cell.flags, Flags::BOLD);
        }

        row.erase_from(10, &Cell::default());
        assert_eq!(row.occupied(), 10);
        assert_eq!(row[Column(9)].c(), '9');
        assert!(row[Column(10)].is_default());
        assert!(row[Column(36)].is_default());

        row.reset(&Cell::default());
        assert_eq!(row.occupied(), 0);
        assert!(row.is_clear());
    }

    #[test]
    fn reset_after_and_unclean_handling() {
        let mut slab = Slab::new(1, 20);
        let row = slab.row_mut(0);
        row[Column(3)].push_zerowidth('\u{301}');
        assert!(row.unclean());
        assert_eq!(row.reset_after(&Cell::default(), 5), 0);
        assert!(!row.unclean());
        assert_eq!(row.occupied(), 0);

        let mut text = row.text(2, &Cell::default());
        text.push('A', false);
        text.push('\u{4e2d}', true);
        drop(text);
        assert_eq!(row.occupied(), 5);
        assert_eq!(row[Column(2)].c(), 'A');
        assert_eq!(row[Column(3)].c(), '\u{4e2d}');
        assert!(row[Column(3)].flags.contains(Flags::WIDE_CHAR));
        assert!(row[Column(4)].flags.contains(Flags::WIDE_CHAR_SPACER));

        let colored = Cell::from(Color::Named(NamedColor::Blue));
        assert_eq!(row.reset_after(&colored, 4), 4);
        assert_eq!(row.occupied(), 20);
        assert_eq!(row[Column(19)].bg(), Color::Named(NamedColor::Blue));
    }
}
