//! Colors: how they are written, and how a theme and the colors set
//! directly combine.

use tern_types::{Rgb, Theme};

/// The colors one config or theme file sets; None where it leaves a color
/// alone.
#[derive(Clone, Debug)]
pub(crate) struct Colors {
    fg: Option<Rgb>,
    bg: Option<Rgb>,
    cursor: Option<Rgb>,
    cursor_text: Option<Rgb>,
    selection_bg: Option<Rgb>,
    selection_fg: Option<Rgb>,
    palette: [Option<Rgb>; 256],
}

impl Default for Colors {
    fn default() -> Colors {
        Colors {
            fg: None,
            bg: None,
            cursor: None,
            cursor_text: None,
            selection_bg: None,
            selection_fg: None,
            palette: [None; 256],
        }
    }
}

impl Colors {
    /// Applies one setting if its key is a color key, and returns None
    /// otherwise. An empty value unsets the color (for palette: every entry).
    pub(crate) fn set(&mut self, key: &str, value: &str) -> Option<Result<(), String>> {
        let slot = match key {
            "background" => &mut self.bg,
            "foreground" => &mut self.fg,
            "cursor-color" => &mut self.cursor,
            "cursor-text" => &mut self.cursor_text,
            "selection-background" => &mut self.selection_bg,
            "selection-foreground" => &mut self.selection_fg,
            "palette" => return Some(self.set_palette(value)),
            _ => return None,
        };
        if value.is_empty() {
            *slot = None;
            return Some(Ok(()));
        }
        Some(parse_color(value).map(|color| *slot = Some(color)))
    }

    fn set_palette(&mut self, value: &str) -> Result<(), String> {
        if value.is_empty() {
            self.palette = [None; 256];
            return Ok(());
        }
        let (index, color) = value
            .split_once('=')
            .ok_or_else(|| format!("expected N=#rrggbb, got \"{value}\""))?;
        let index = index.trim();
        let index: u8 = index
            .parse()
            .map_err(|_| format!("expected an entry from 0 to 255, got \"{index}\""))?;
        self.palette[usize::from(index)] = Some(parse_color(color.trim())?);
        Ok(())
    }

    fn apply(&self, theme: &mut Theme) {
        if let Some(color) = self.fg {
            theme.fg = color;
        }
        if let Some(color) = self.bg {
            theme.bg = color;
        }
        for (slot, color) in [
            (&mut theme.cursor, self.cursor),
            (&mut theme.cursor_text, self.cursor_text),
            (&mut theme.selection_bg, self.selection_bg),
            (&mut theme.selection_fg, self.selection_fg),
        ] {
            if color.is_some() {
                *slot = color;
            }
        }
        for (slot, color) in theme.palette.iter_mut().zip(&self.palette) {
            if let Some(color) = color {
                *slot = *color;
            }
        }
    }
}

/// The colors in effect: the defaults, then the theme's, then those set
/// directly in the config.
pub(crate) fn compose(theme: Option<&Colors>, direct: &Colors, bold_is_bright: bool) -> Theme {
    let mut out = Theme::default();
    // A theme is a whole color scheme, and colors set directly replace the
    // default scheme's text colors. Either way, where nothing is said about
    // the cursor or the selection they follow the text's own colors rather
    // than the default scheme's, which were picked for a different
    // background.
    if theme.is_some() || direct.fg.is_some() || direct.bg.is_some() {
        out.cursor = None;
        out.cursor_text = None;
        out.selection_bg = None;
        out.selection_fg = None;
    }
    if let Some(theme) = theme {
        theme.apply(&mut out);
    }
    direct.apply(&mut out);
    out.bold_is_bright = bold_is_bright;
    out
}

/// A color written #rrggbb, rrggbb or #rgb, in either case.
pub(crate) fn parse_color(text: &str) -> Result<Rgb, String> {
    let error = || format!("expected a color as #rrggbb, rrggbb or #rgb, got \"{text}\"");
    let (hex, hash) = match text.strip_prefix('#') {
        Some(hex) => (hex, true),
        None => (text, false),
    };
    if !(hex.len() == 6 || (hash && hex.len() == 3)) {
        return Err(error());
    }
    // Every hex digit is one byte, so the length checked above is the digit
    // count once they all parse.
    let mut value = 0;
    for c in hex.chars() {
        value = value << 4 | c.to_digit(16).ok_or_else(error)?;
    }
    if hex.len() == 6 {
        return Ok(value);
    }
    // Each digit is doubled: #f80 is #ff8800.
    let (r, g, b) = (value >> 8, value >> 4 & 0xf, value & 0xf);
    Ok((r << 16 | g << 8 | b) * 0x11)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_types::xterm_256_palette;

    fn colors(lines: &[(&str, &str)]) -> Colors {
        let mut colors = Colors::default();
        for (key, value) in lines {
            colors
                .set(key, value)
                .expect("color key")
                .expect("good value");
        }
        colors
    }

    #[test]
    fn color_forms() {
        assert_eq!(parse_color("#1a2b3c"), Ok(0x1a2b3c));
        assert_eq!(parse_color("1a2b3c"), Ok(0x1a2b3c));
        assert_eq!(parse_color("#1A2B3C"), Ok(0x1a2b3c));
        assert_eq!(parse_color("#f80"), Ok(0xff8800));
        assert_eq!(parse_color("#FFF"), Ok(0xffffff));
        assert_eq!(parse_color("#000000"), Ok(0));
        for bad in [
            "",
            "#",
            "f80",
            "#12345",
            "#1234567",
            "12345",
            "#ggg",
            "#12345g",
            "red",
            "0x123456",
            "#\u{e9}\u{e9}\u{e9}",
            "#+12345",
            " #123456",
        ] {
            assert!(parse_color(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            parse_color("blue"),
            Err("expected a color as #rrggbb, rrggbb or #rgb, got \"blue\"".into())
        );
    }

    #[test]
    fn only_color_keys_are_colors() {
        let mut c = Colors::default();
        assert!(c.set("font-size", "12").is_none());
        assert!(c.set("bold-is-bright", "true").is_none());
        for key in [
            "background",
            "foreground",
            "cursor-color",
            "cursor-text",
            "selection-background",
            "selection-foreground",
        ] {
            assert_eq!(c.set(key, "#010203"), Some(Ok(())), "{key}");
            assert!(matches!(c.set(key, "nope"), Some(Err(_))), "{key}");
        }
    }

    #[test]
    fn palette_entries() {
        let c = colors(&[
            ("palette", "0=#111111"),
            ("palette", "255 = 222222"),
            ("palette", "17=#abc"),
        ]);
        let theme = compose(None, &c, false);
        assert_eq!(theme.palette[0], 0x111111);
        assert_eq!(theme.palette[255], 0x222222);
        assert_eq!(theme.palette[17], 0xaabbcc);
        assert_eq!(theme.palette[1], Theme::default().palette[1]);
        assert_eq!(theme.palette[200], xterm_256_palette()[200]);

        let mut c = Colors::default();
        for (value, message) in [
            (
                "256=#000000",
                "expected an entry from 0 to 255, got \"256\"",
            ),
            ("-1=#000000", "from 0 to 255"),
            ("x=#000000", "from 0 to 255"),
            ("=#000000", "from 0 to 255"),
            ("1", "expected N=#rrggbb, got \"1\""),
            ("1=", "expected a color"),
            ("1=#12", "expected a color"),
        ] {
            let error = c.set("palette", value).unwrap().expect_err(value);
            assert!(error.contains(message), "{value}: {error}");
        }

        // An empty value drops every entry set so far.
        let mut c = colors(&[("palette", "3=#333333")]);
        c.set("palette", "").unwrap().unwrap();
        assert_eq!(compose(None, &c, false), Theme::default());
    }

    #[test]
    fn direct_colors_override_the_theme() {
        let theme = colors(&[
            ("background", "#000001"),
            ("foreground", "#000002"),
            ("cursor-color", "#000003"),
            ("palette", "1=#000004"),
            ("palette", "2=#000005"),
        ]);
        let direct = colors(&[("background", "#ffffff"), ("palette", "2=#eeeeee")]);
        let out = compose(Some(&theme), &direct, true);
        assert_eq!(out.bg, 0xffffff);
        assert_eq!(out.fg, 0x000002);
        assert_eq!(out.cursor, Some(0x000003));
        assert_eq!(out.palette[1], 0x000004);
        assert_eq!(out.palette[2], 0xeeeeee);
        assert_eq!(out.palette[3], Theme::default().palette[3]);
        assert!(out.bold_is_bright);
    }

    #[test]
    fn a_theme_without_cursor_or_selection_colors_follows_the_text() {
        let theme = colors(&[("background", "#fafafa"), ("foreground", "#101010")]);
        let out = compose(Some(&theme), &Colors::default(), false);
        assert_eq!((out.bg, out.fg), (0xfafafa, 0x101010));
        assert_eq!(out.cursor, None);
        assert_eq!(out.cursor_text, None);
        assert_eq!(out.selection_bg, None);
        assert_eq!(out.selection_fg, None);
    }

    #[test]
    fn direct_text_colors_without_cursor_or_selection_colors_follow_the_text() {
        for set in [
            &[("background", "#fafafa")][..],
            &[("foreground", "#101010")],
        ] {
            let out = compose(None, &colors(set), false);
            assert_eq!(out.cursor, None, "{set:?}");
            assert_eq!(out.cursor_text, None, "{set:?}");
            assert_eq!(out.selection_bg, None, "{set:?}");
            assert_eq!(out.selection_fg, None, "{set:?}");
        }
        // Colors set alongside them are kept.
        let out = compose(
            None,
            &colors(&[("background", "#fafafa"), ("cursor-color", "#ff0000")]),
            false,
        );
        assert_eq!((out.cursor, out.selection_bg), (Some(0xff0000), None));
        // Palette entries alone leave the default scheme's cursor and selection.
        let out = compose(None, &colors(&[("palette", "1=#111111")]), false);
        assert_eq!(out.cursor, Theme::default().cursor);
        assert_eq!(out.selection_bg, Theme::default().selection_bg);
    }

    #[test]
    fn empty_value_unsets_a_color() {
        let mut c = colors(&[
            ("background", "#123456"),
            ("selection-foreground", "#654321"),
        ]);
        c.set("background", "").unwrap().unwrap();
        c.set("selection-foreground", "").unwrap().unwrap();
        assert_eq!(compose(None, &c, false), Theme::default());
    }

    #[test]
    fn nothing_set_is_the_default_theme() {
        assert_eq!(compose(None, &Colors::default(), false), Theme::default());
    }
}
