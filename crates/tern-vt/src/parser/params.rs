//! The parameters of a control sequence: numbers separated by `;`, each of
//! which may be split further into subparameters by `:`, as in
//! `CSI 38:2::255:0:0 m`.

/// At most this many numbers, parameters and subparameters together, are
/// kept. A sequence that needs more is not carried out.
const MAX: usize = 32;

/// The numbers of a control sequence, kept as they are read.
#[derive(Default)]
pub(super) struct Params {
    /// Every number ended so far, parameters and subparameters in order:
    /// the first `len`.
    numbers: [u16; MAX],
    len: usize,
    /// Where each finished parameter ends in `numbers`: the first `count`.
    ends: [u8; MAX],
    count: usize,
    /// The number being read. Digits past what a u16 holds keep it at the
    /// largest it can be.
    current: u16,
}

impl Params {
    /// Forgets every number, for a new sequence.
    pub(super) fn clear(&mut self) {
        self.len = 0;
        self.count = 0;
        self.current = 0;
    }

    /// Whether no more numbers can be kept.
    pub(super) fn is_full(&self) -> bool {
        self.len == MAX
    }

    /// Adds a decimal digit to the number being read.
    pub(super) fn digit(&mut self, digit: u8) {
        self.current = self.current.saturating_mul(10).saturating_add(u16::from(digit));
    }

    /// Ends the number being read, which is zero if it has no digits: as a
    /// subparameter that more of the same parameter follow (`:`), or, with
    /// `last`, as the end of its parameter (`;` or the final byte). The
    /// caller makes sure there is room.
    pub(super) fn end_number(&mut self, last: bool) {
        self.numbers[self.len] = self.current;
        self.len += 1;
        self.current = 0;
        if last {
            // At most MAX, so it fits.
            self.ends[self.count] = self.len as u8;
            self.count += 1;
        }
    }

    /// The finished parameters, each with its subparameters: never empty.
    pub(super) fn iter(&self) -> Iter<'_> {
        Iter { params: self, next: 0, start: 0 }
    }
}

/// The parameters of a sequence one at a time, each as the numbers it is
/// made of.
pub(super) struct Iter<'a> {
    params: &'a Params,
    /// The parameter to give next, and where its numbers start.
    next: usize,
    start: usize,
}

impl<'a> Iterator for Iter<'a> {
    type Item = &'a [u16];

    #[inline]
    fn next(&mut self) -> Option<&'a [u16]> {
        if self.next == self.params.count {
            return None;
        }
        let end = usize::from(self.params.ends[self.next]);
        let numbers = &self.params.numbers[self.start..end];
        self.next += 1;
        self.start = end;
        Some(numbers)
    }
}
