/// A key, independent of the modifiers held with it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    /// A key that types a character. Holds the key's base character: what the
    /// key produces with no modifiers, lowercase where that applies ('a' for
    /// the A key whether or not Shift is down, '1' for the 1 key, ' ' for the
    /// space bar).
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    /// Forward delete.
    Delete,
    /// F1 is F(1).
    F(u8),
    Keypad(KeypadKey),
    CapsLock,
    Menu,
    // The modifier keys themselves, which only the kitty keyboard protocol reports.
    ShiftLeft,
    ShiftRight,
    ControlLeft,
    ControlRight,
    AltLeft,
    AltRight,
    SuperLeft,
    SuperRight,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum KeypadKey {
    Digit(u8),
    Decimal,
    Divide,
    Multiply,
    Subtract,
    Add,
    Enter,
    Equal,
}

/// Modifier keys. The bit values are the kitty keyboard protocol's, so
/// "mods.0 + 1" is the modifier parameter of a CSI sequence.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Mods(pub u8);

impl Mods {
    pub const NONE: Mods = Mods(0);
    pub const SHIFT: Mods = Mods(1);
    /// Option on a Mac.
    pub const ALT: Mods = Mods(2);
    pub const CTRL: Mods = Mods(4);
    /// Command on a Mac.
    pub const SUPER: Mods = Mods(8);
    pub const CAPS_LOCK: Mods = Mods(64);
    pub const NUM_LOCK: Mods = Mods(128);

    #[inline]
    pub const fn contains(self, other: Mods) -> bool {
        self.0 & other.0 == other.0
    }

    #[inline]
    pub const fn union(self, other: Mods) -> Mods {
        Mods(self.0 | other.0)
    }

    #[inline]
    pub const fn without(self, other: Mods) -> Mods {
        Mods(self.0 & !other.0)
    }

    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn shift(self) -> bool {
        self.contains(Mods::SHIFT)
    }

    pub const fn alt(self) -> bool {
        self.contains(Mods::ALT)
    }

    pub const fn ctrl(self) -> bool {
        self.contains(Mods::CTRL)
    }

    pub const fn cmd(self) -> bool {
        self.contains(Mods::SUPER)
    }

    /// Shift, Alt, Ctrl and Super only: lock keys removed.
    pub const fn held(self) -> Mods {
        Mods(self.0 & 0b1111)
    }
}

impl std::ops::BitOr for Mods {
    type Output = Mods;
    fn bitor(self, rhs: Mods) -> Mods {
        self.union(rhs)
    }
}
