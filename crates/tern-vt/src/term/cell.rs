use std::cell::UnsafeCell;
use std::fmt;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use bitflags::bitflags;

use crate::ansi::{self, Color, NamedColor, Rgb};

bitflags! {
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Flags: u16 {
        const INVERSE                  = 1 << 0;
        const BOLD                     = 1 << 1;
        const ITALIC                   = 1 << 2;
        const UNDERLINE                = 1 << 3;
        const WRAPLINE                 = 1 << 4;
        const WIDE_CHAR                = 1 << 5;
        const WIDE_CHAR_SPACER         = 1 << 6;
        const DIM                      = 1 << 7;
        const HIDDEN                   = 1 << 8;
        const STRIKEOUT                = 1 << 9;
        const LEADING_WIDE_CHAR_SPACER = 1 << 10;
        const DOUBLE_UNDERLINE         = 1 << 11;
        const UNDERCURL                = 1 << 12;
        const DOTTED_UNDERLINE         = 1 << 13;
        const DASHED_UNDERLINE         = 1 << 14;
        const ALL_UNDERLINES           = Self::UNDERLINE.bits()
                                       | Self::DOUBLE_UNDERLINE.bits()
                                       | Self::UNDERCURL.bits()
                                       | Self::DOTTED_UNDERLINE.bits()
                                       | Self::DASHED_UNDERLINE.bits();
    }
}

static NEXT_HYPERLINK_ID: AtomicU32 = AtomicU32::new(0);

#[derive(PartialEq, Eq, Hash)]
struct HyperlinkInner {
    id: String,
    uri: String,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Hyperlink {
    inner: Arc<HyperlinkInner>,
}

impl Hyperlink {
    pub fn new<T: ToString>(id: Option<T>, uri: String) -> Self {
        let id = match id {
            Some(id) => id.to_string(),
            None => {
                let n = NEXT_HYPERLINK_ID.fetch_add(1, Ordering::Relaxed);
                format!("{n}_tern")
            },
        };
        Self { inner: Arc::new(HyperlinkInner { id, uri }) }
    }

    #[inline]
    pub fn id(&self) -> &str {
        &self.inner.id
    }

    #[inline]
    pub fn uri(&self) -> &str {
        &self.inner.uri
    }
}

impl fmt::Debug for Hyperlink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hyperlink")
            .field("id", &self.inner.id)
            .field("uri", &self.inner.uri)
            .finish()
    }
}

impl From<ansi::Hyperlink> for Hyperlink {
    fn from(value: ansi::Hyperlink) -> Self {
        Self::new(value.id, value.uri)
    }
}

#[derive(Default, Debug, Clone, Eq, PartialEq)]
pub struct CellExtra {
    pub(crate) zerowidth: Vec<char>,
    pub(crate) underline_color: Option<Color>,
    pub(crate) hyperlink: Option<Hyperlink>,
}

/// The most characters of no width one cell keeps. Real text needs a few;
/// more than this is noise, and is dropped rather than kept without end.
pub const MAX_ZEROWIDTH: usize = 32;

const CHAR_MASK: u32 = 0x001f_ffff;
const EXTRA_HI_SHIFT: u32 = 21;
const MAX_EXTRA_ID: u32 = (1 << 27) - 1;
const BUCKETS: usize = 22;
const FIRST_BUCKET_SHIFT: u32 = 6;

struct Slot {
    refs: AtomicU32,
    extra: UnsafeCell<MaybeUninit<CellExtra>>,
}

struct AllocState {
    next_id: u32,
    free: Vec<u32>,
    live: usize,
}

static EXTRA_BUCKETS: [AtomicPtr<Slot>; BUCKETS] =
    [const { AtomicPtr::new(std::ptr::null_mut()) }; BUCKETS];

static EXTRA_ALLOC: Mutex<AllocState> = Mutex::new(AllocState {
    next_id: 1,
    free: Vec::new(),
    live: 0,
});

#[inline]
const fn bucket_and_index(id: u32) -> (usize, usize) {
    let shifted = id + ((1 << FIRST_BUCKET_SHIFT) - 1);
    let log2 = 31 - shifted.leading_zeros();
    let bucket = (log2 - FIRST_BUCKET_SHIFT) as usize;
    let index = (shifted ^ (1 << log2)) as usize;
    (bucket, index)
}

#[inline]
const fn bucket_len(bucket: usize) -> usize {
    1 << (bucket as u32 + FIRST_BUCKET_SHIFT)
}

#[inline]
fn slot_ref(id: u32) -> &'static Slot {
    debug_assert!(id != 0 && id <= MAX_EXTRA_ID);
    let (bucket, index) = bucket_and_index(id);
    let base = EXTRA_BUCKETS[bucket].load(Ordering::Acquire);
    debug_assert!(!base.is_null());
    // SAFETY: each bucket is allocated with `bucket_len(bucket)` slots before any ID in that
    // bucket is handed out, and buckets are never moved or deallocated.
    unsafe { &*base.add(index) }
}

fn alloc_extra(extra: CellExtra) -> u32 {
    let mut state = EXTRA_ALLOC.lock().unwrap_or_else(PoisonError::into_inner);
    let id = if let Some(id) = state.free.pop() {
        id
    } else {
        let id = state.next_id;
        assert!(id <= MAX_EXTRA_ID, "CellExtra table exhausted");
        let (bucket, index) = bucket_and_index(id);
        if index == 0 {
            let len = bucket_len(bucket);
            let mut slots = Vec::with_capacity(len);
            slots.resize_with(len, || Slot {
                refs: AtomicU32::new(0),
                extra: UnsafeCell::new(MaybeUninit::uninit()),
            });
            let ptr = Box::into_raw(slots.into_boxed_slice()).cast::<Slot>();
            EXTRA_BUCKETS[bucket].store(ptr, Ordering::Release);
        }
        state.next_id = id + 1;
        id
    };
    state.live += 1;
    drop(state);

    let slot = slot_ref(id);
    debug_assert_eq!(slot.refs.load(Ordering::Relaxed), 0);
    // SAFETY: `id` has refs == 0 and is not visible to any other holder until returned.
    unsafe {
        (*slot.extra.get()).write(extra);
    }
    slot.refs.store(1, Ordering::Release);
    id
}

#[inline]
fn retain_extra(id: u32) {
    let prev = slot_ref(id).refs.fetch_add(1, Ordering::Relaxed);
    assert!((1..u32::MAX).contains(&prev), "invalid CellExtra refcount");
}

fn release_extra(id: u32) {
    let slot = slot_ref(id);
    let prev = slot.refs.fetch_sub(1, Ordering::AcqRel);
    debug_assert!(prev >= 1);
    if prev != 1 {
        return;
    }
    // SAFETY: `AcqRel` observed refs drop from 1 to 0, synchronizing with all prior holders;
    // `id` is not returned to `free` until after `dropped` is read out.
    let dropped = unsafe { (*slot.extra.get()).assume_init_read() };
    drop(dropped);
    let mut state = EXTRA_ALLOC.lock().unwrap_or_else(PoisonError::into_inner);
    state.live -= 1;
    state.free.push(id);
}

pub fn live_extras() -> usize {
    EXTRA_ALLOC.lock().unwrap_or_else(PoisonError::into_inner).live
}

pub struct ExtraShare(u32);

impl Drop for ExtraShare {
    #[inline]
    fn drop(&mut self) {
        if self.0 != 0 {
            release_extra(self.0);
        }
    }
}

const NAMED_COLORS: [NamedColor; 19] = [
    NamedColor::Black,
    NamedColor::Red,
    NamedColor::Green,
    NamedColor::Yellow,
    NamedColor::Blue,
    NamedColor::Magenta,
    NamedColor::Cyan,
    NamedColor::White,
    NamedColor::BrightBlack,
    NamedColor::BrightRed,
    NamedColor::BrightGreen,
    NamedColor::BrightYellow,
    NamedColor::BrightBlue,
    NamedColor::BrightMagenta,
    NamedColor::BrightCyan,
    NamedColor::BrightWhite,
    NamedColor::Foreground,
    NamedColor::Background,
    NamedColor::Cursor,
];

#[inline]
pub(crate) const fn color_word(color: Color) -> u32 {
    match color {
        Color::Named(named) => {
            let code = named as u32;
            if code < 16 { code } else { code - 240 }
        },
        Color::Indexed(idx) => (1 << 24) | (idx as u32),
        Color::Spec(Rgb { r, g, b }) => {
            (2 << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
        },
    }
}

#[inline]
pub(crate) const fn word_color(word: u32) -> Color {
    match word >> 24 {
        0 => {
            let idx = (word as usize) & 0xff;
            let clamped = if idx < NAMED_COLORS.len() { idx } else { 16 };
            Color::Named(NAMED_COLORS[clamped])
        },
        1 => Color::Indexed(word as u8),
        _ => Color::Spec(Rgb {
            r: (word >> 16) as u8,
            g: (word >> 8) as u8,
            b: word as u8,
        }),
    }
}

pub(crate) const DEFAULT_FG_WORD: u32 = color_word(Color::Named(NamedColor::Foreground));
pub(crate) const DEFAULT_BG_WORD: u32 = color_word(Color::Named(NamedColor::Background));

const NON_EMPTY_FLAGS: Flags = Flags::INVERSE
    .union(Flags::ALL_UNDERLINES)
    .union(Flags::STRIKEOUT)
    .union(Flags::WRAPLINE)
    .union(Flags::WIDE_CHAR_SPACER)
    .union(Flags::LEADING_WIDE_CHAR_SPACER);

/// Compact 16-byte terminal cell:
/// - word 0: low 21 bits = `char`, high 11 bits = upper 11 bits of 27-bit extra handle
/// - word 1: `fg` color word
/// - word 2: `bg` color word
/// - word 3: low 16 bits = `Flags`, high 16 bits = lower 16 bits of 27-bit extra handle
#[repr(C)]
pub struct Cell {
    c_and_extra: u32,
    fg: u32,
    bg: u32,
    pub flags: Flags,
    extra_lo: u16,
}

const _: () = assert!(std::mem::size_of::<Cell>() == 16);
const _: () = assert!(std::mem::align_of::<Cell>() == 4);

impl Default for Cell {
    #[inline]
    fn default() -> Self {
        Self::from_words(' ', DEFAULT_FG_WORD, DEFAULT_BG_WORD, Flags::empty())
    }
}

impl Cell {
    #[inline]
    pub const fn new(c: char, fg: Color, bg: Color, flags: Flags) -> Self {
        Self::from_words(c, color_word(fg), color_word(bg), flags)
    }

    #[inline]
    pub const fn styled(c: char, template: &Self) -> Self {
        Self::from_words(c, template.fg, template.bg, template.flags)
    }

    #[inline]
    pub(crate) const fn from_words(c: char, fg: u32, bg: u32, flags: Flags) -> Self {
        Self { c_and_extra: c as u32, fg, bg, flags, extra_lo: 0 }
    }

    #[inline]
    pub(crate) const fn fg_word(&self) -> u32 {
        self.fg
    }

    #[inline]
    pub(crate) const fn bg_word(&self) -> u32 {
        self.bg
    }

    #[inline]
    pub const fn is_default(&self) -> bool {
        self.c_and_extra == (' ' as u32)
            && self.fg == DEFAULT_FG_WORD
            && self.bg == DEFAULT_BG_WORD
            && self.flags.is_empty()
            && self.extra_lo == 0
    }

    #[inline]
    pub fn is_blank(&self) -> bool {
        let c = self.c();
        (c == ' ' || c == '\t')
            && self.fg == DEFAULT_FG_WORD
            && self.bg == DEFAULT_BG_WORD
            && !self.flags.intersects(NON_EMPTY_FLAGS)
            && self.zerowidth().is_none_or(<[char]>::is_empty)
    }

    #[inline]
    pub const fn fg(&self) -> Color {
        word_color(self.fg)
    }

    #[inline]
    pub const fn bg(&self) -> Color {
        word_color(self.bg)
    }

    #[inline]
    pub fn set_fg(&mut self, fg: Color) {
        self.fg = color_word(fg);
    }

    #[inline]
    pub fn set_bg(&mut self, bg: Color) {
        self.bg = color_word(bg);
    }

    #[inline]
    pub fn set_style(&mut self, style: &Self) {
        self.fg = style.fg;
        self.bg = style.bg;
        self.flags = style.flags;
    }

    #[inline]
    pub const fn erased(&self) -> Self {
        Self::from_words(' ', DEFAULT_FG_WORD, self.bg, Flags::empty())
    }

    #[inline]
    pub const fn c(&self) -> char {
        // SAFETY: the low 21 bits of `c_and_extra` are only ever populated from `char as u32`.
        unsafe { char::from_u32_unchecked(self.c_and_extra & CHAR_MASK) }
    }

    #[inline]
    pub fn set_c(&mut self, c: char) {
        self.c_and_extra = (self.c_and_extra & !CHAR_MASK) | (c as u32);
    }

    #[inline]
    pub const fn has_extra(&self) -> bool {
        self.extra_lo != 0 || (self.c_and_extra & !CHAR_MASK) != 0
    }

    #[inline]
    const fn extra_id(&self) -> u32 {
        (self.extra_lo as u32) | ((self.c_and_extra >> EXTRA_HI_SHIFT) << 16)
    }

    #[inline]
    fn store_extra_id(&mut self, id: u32) {
        debug_assert!(id <= MAX_EXTRA_ID);
        self.extra_lo = id as u16;
        self.c_and_extra = (self.c_and_extra & CHAR_MASK) | ((id >> 16) << EXTRA_HI_SHIFT);
    }

    #[cold]
    fn drop_extra_cold(&mut self) {
        let id = self.extra_id();
        if id != 0 {
            self.store_extra_id(0);
            release_extra(id);
        }
    }

    #[inline]
    pub(crate) fn extra_parts(&self) -> Option<&CellExtra> {
        let id = self.extra_id();
        if id == 0 {
            return None;
        }
        let slot = slot_ref(id);
        // SAFETY: `self` holds a live reference (refs >= 1), and shared entries are immutable.
        Some(unsafe { (*slot.extra.get()).assume_init_ref() })
    }

    pub(crate) fn set_extra(&mut self, extra: CellExtra) {
        let old_id = self.extra_id();
        if old_id != 0 {
            let slot = slot_ref(old_id);
            if slot.refs.load(Ordering::Acquire) == 1 {
                // SAFETY: `Acquire` synchronizes with prior drops and `refs == 1` with `&mut self`
                // guarantees exclusive access to `slot.extra`.
                unsafe {
                    *(*slot.extra.get()).assume_init_mut() = extra;
                }
                return;
            }
            release_extra(old_id);
        }
        let new_id = alloc_extra(extra);
        self.store_extra_id(new_id);
    }

    pub(crate) fn extra_mut(&mut self) -> &mut CellExtra {
        let id = self.extra_id();
        if id == 0 {
            let new_id = alloc_extra(CellExtra::default());
            self.store_extra_id(new_id);
            let slot = slot_ref(new_id);
            // SAFETY: freshly allocated with refs == 1, exclusively owned by `self`.
            return unsafe { (*slot.extra.get()).assume_init_mut() };
        }
        let slot = slot_ref(id);
        if slot.refs.load(Ordering::Acquire) == 1 {
            // SAFETY: `Acquire` synchronizes with prior drops and `refs == 1` with `&mut self`
            // guarantees exclusive access to `slot.extra`.
            return unsafe { (*slot.extra.get()).assume_init_mut() };
        }
        // SAFETY: `self` holds a valid share and `slot.extra` is immutable while `refs > 1`.
        let cloned = unsafe { (*slot.extra.get()).assume_init_ref().clone() };
        release_extra(id);
        let new_id = alloc_extra(cloned);
        self.store_extra_id(new_id);
        let new_slot = slot_ref(new_id);
        // SAFETY: freshly allocated with refs == 1, exclusively owned by `self`.
        unsafe { (*new_slot.extra.get()).assume_init_mut() }
    }

    #[inline]
    pub fn share_extra(&self) -> ExtraShare {
        let id = self.extra_id();
        if id != 0 {
            retain_extra(id);
        }
        ExtraShare(id)
    }

    #[inline]
    pub fn put_extra(&mut self, share: ExtraShare) {
        let new_id = share.0;
        std::mem::forget(share);
        let old_id = self.extra_id();
        if old_id == new_id {
            if new_id != 0 {
                release_extra(new_id);
            }
            return;
        }
        if old_id != 0 {
            release_extra(old_id);
        }
        self.store_extra_id(new_id);
    }

    #[inline]
    pub fn zerowidth(&self) -> Option<&[char]> {
        self.extra_parts().map(|extra| extra.zerowidth.as_slice())
    }

    /// Adds a character of no width, unless the cell already has
    /// [MAX_ZEROWIDTH] of them.
    pub fn push_zerowidth(&mut self, character: char) {
        if self.zerowidth().is_some_and(|chars| chars.len() >= MAX_ZEROWIDTH) {
            return;
        }
        self.extra_mut().zerowidth.push(character);
    }

    #[inline]
    pub fn clear_wide(&mut self) {
        self.flags.remove(Flags::WIDE_CHAR);
        if self.has_extra() {
            self.extra_mut().zerowidth.clear();
        }
        self.set_c(' ');
    }

    pub fn set_underline_color(&mut self, color: Option<Color>) {
        match color {
            None => {
                let Some(extra) = self.extra_parts() else { return };
                if extra.zerowidth.is_empty() && extra.hyperlink.is_none() {
                    self.drop_extra_cold();
                } else if extra.underline_color.is_some() {
                    self.extra_mut().underline_color = None;
                }
            },
            Some(c) => {
                if self.underline_color() != Some(c) {
                    self.extra_mut().underline_color = Some(c);
                }
            },
        }
    }

    #[inline]
    pub fn underline_color(&self) -> Option<Color> {
        self.extra_parts().and_then(|extra| extra.underline_color)
    }

    pub fn set_hyperlink(&mut self, hyperlink: Option<Hyperlink>) {
        match hyperlink {
            None => {
                let Some(extra) = self.extra_parts() else { return };
                if extra.zerowidth.is_empty() && extra.underline_color.is_none() {
                    self.drop_extra_cold();
                } else if extra.hyperlink.is_some() {
                    self.extra_mut().hyperlink = None;
                }
            },
            Some(link) => {
                if self.hyperlink() != Some(&link) {
                    self.extra_mut().hyperlink = Some(link);
                }
            },
        }
    }

    #[inline]
    pub fn hyperlink(&self) -> Option<&Hyperlink> {
        self.extra_parts().and_then(|extra| extra.hyperlink.as_ref())
    }
}

impl From<Color> for Cell {
    #[inline]
    fn from(bg: Color) -> Self {
        Self::from_words(' ', DEFAULT_FG_WORD, color_word(bg), Flags::empty())
    }
}

impl Clone for Cell {
    #[inline]
    fn clone(&self) -> Self {
        let id = self.extra_id();
        if id != 0 {
            retain_extra(id);
        }
        Self {
            c_and_extra: self.c_and_extra,
            fg: self.fg,
            bg: self.bg,
            flags: self.flags,
            extra_lo: self.extra_lo,
        }
    }
}

impl Drop for Cell {
    #[inline]
    fn drop(&mut self) {
        if self.has_extra() {
            self.drop_extra_cold();
        }
    }
}

impl PartialEq for Cell {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.c() == other.c()
            && self.fg == other.fg
            && self.bg == other.bg
            && self.flags == other.flags
            && (self.extra_id() == other.extra_id() || self.extra_parts() == other.extra_parts())
    }
}

impl Eq for Cell {}

impl fmt::Debug for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let zerowidth =
            self.zerowidth().map(|zw| zw.iter().collect::<String>()).unwrap_or_default();
        f.debug_struct("Cell")
            .field("c", &self.c())
            .field("fg", &self.fg())
            .field("bg", &self.bg())
            .field("flags", &self.flags)
            .field("zerowidth", &zerowidth)
            .field("underline", &self.underline_color())
            .field("link", &self.hyperlink())
            .field("extra", &self.has_extra())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_words_roundtrip_uniquely() {
        let mut seen = std::collections::HashSet::new();
        for named in NAMED_COLORS {
            let c = Color::Named(named);
            let w = color_word(c);
            assert!(seen.insert(w), "duplicate word for {named:?}");
            assert_eq!(word_color(w), c);
        }
        assert_eq!(DEFAULT_FG_WORD, 16);
        assert_eq!(DEFAULT_BG_WORD, 17);

        for idx in 0..=u8::MAX {
            let c = Color::Indexed(idx);
            let w = color_word(c);
            assert!(seen.insert(w), "duplicate word for Indexed({idx})");
            assert_eq!(word_color(w), c);
        }

        for (r, g, b) in [(0, 0, 0), (255, 255, 255), (12, 34, 56), (16, 17, 18)] {
            let c = Color::Spec(Rgb { r, g, b });
            let w = color_word(c);
            assert!(seen.insert(w));
            assert_eq!(word_color(w), c);
        }
    }

    #[test]
    fn bucket_coordinates_cover_all_27_bit_ids() {
        assert_eq!(bucket_and_index(1), (0, 0));
        assert_eq!(bucket_and_index(64), (0, 63));
        assert_eq!(bucket_and_index(65), (1, 0));
        assert_eq!(bucket_and_index(192), (1, 127));
        assert_eq!(bucket_and_index(193), (2, 0));

        let (last_bucket, last_idx) = bucket_and_index(MAX_EXTRA_ID);
        assert!(last_bucket < BUCKETS);
        assert!(last_idx < bucket_len(last_bucket));
    }

    #[test]
    fn extra_handle_packs_across_both_words() {
        let mut cell = Cell::new('\u{1f600}', Color::Indexed(9), Color::Indexed(2), Flags::BOLD);
        for id in [0, 1, 0xffff, 0x1_0000, 0x05a5_f00d, MAX_EXTRA_ID] {
            cell.store_extra_id(id);
            assert_eq!(cell.extra_id(), id);
            assert_eq!(cell.c(), '\u{1f600}');
            assert_eq!(cell.has_extra(), id != 0);
            cell.set_c('Z');
            assert_eq!(cell.extra_id(), id);
            assert_eq!(cell.c(), 'Z');
            cell.set_c('\u{1f600}');
        }
        cell.store_extra_id(0);
    }

    #[test]
    fn copy_on_write_and_extra_cleanup() {
        let mut template = Cell::default();
        let link = Hyperlink::new(None::<String>, "https://example.org".into());
        template.set_hyperlink(Some(link.clone()));
        assert!(template.has_extra());
        assert!(live_extras() >= 1);

        let mut a = Cell::styled('a', &template);
        a.put_extra(template.share_extra());
        assert_eq!(a.extra_id(), template.extra_id());
        assert_eq!(a.hyperlink(), Some(&link));

        let mut b = a.clone();
        assert_eq!(b.extra_id(), a.extra_id());
        b.push_zerowidth('\u{301}');
        assert_ne!(b.extra_id(), a.extra_id());
        assert_eq!(a.zerowidth(), Some(&[][..]));
        assert_eq!(b.zerowidth(), Some(&['\u{301}'][..]));
        assert_eq!(b.hyperlink(), Some(&link));

        b.set_hyperlink(None);
        assert!(b.has_extra());
        assert_eq!(b.zerowidth(), Some(&['\u{301}'][..]));

        b.clear_wide();
        assert!(b.has_extra());
        assert_eq!(b.c(), ' ');
        assert_eq!(b.zerowidth(), Some(&[][..]));

        b.set_underline_color(Some(Color::Indexed(42)));
        assert_eq!(b.underline_color(), Some(Color::Indexed(42)));
        b.set_underline_color(None);
        assert!(!b.has_extra());

        template.set_hyperlink(None);
        assert!(!template.has_extra());
    }

    #[test]
    fn blank_and_default_detection() {
        let mut cell = Cell::default();
        assert!(cell.is_default());
        assert!(cell.is_blank());

        cell.set_c('\t');
        assert!(!cell.is_default());
        assert!(cell.is_blank());

        cell.flags.insert(Flags::BOLD);
        assert!(cell.is_blank());

        cell.flags.insert(Flags::UNDERLINE);
        assert!(!cell.is_blank());
        cell.flags = Flags::empty();
        cell.set_c(' ');
        assert!(cell.is_default());

        cell.push_zerowidth('\u{308}');
        assert!(!cell.is_blank());
    }
}
