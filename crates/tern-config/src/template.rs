//! The commented example config the app writes when the user opens the
//! config and no file exists yet.

/// Every setting with its default, commented out, with a line or two about
/// each. Uncommenting any setting line leaves the configuration unchanged.
pub const DEFAULT_CONFIG_TEMPLATE: &str = r##"# Tern configuration
#
# Each setting is one line of "key = value". Lines starting with # are
# comments. A value may be wrapped in double quotes, which keeps spaces at its
# ends. A key with an empty value goes back to its default.
#
# Every setting is listed below with its default, commented out: remove the
# "# " in front of a line to change it. Settings given on the command line as
# --key=value override this file.
#
# Reload Configuration (in the Tern menu) applies a changed file to the open
# windows at once, except for the settings marked "new windows only" or "new
# panes only", which apply to what is opened afterwards.

# --- Font ---

# The font's family name, as Font Book shows it. Empty: the system's
# monospaced font, SF Mono.
# font-family =
# The size in points, 6 to 72.
# font-size = 13

# --- Colors ---

# A theme is a file of the color settings below. Give a theme name to use the
# file of that name in the themes folder next to this file or, when Ghostty is
# installed, one of Ghostty's themes; or give the absolute path of a theme
# file (~/ allowed). Colors set in this file override the theme's.
# theme =

# Colors are written #rrggbb, rrggbb or #rgb.
# background = #181a1f
# foreground = #dcdfe4
# cursor-color = #dcdfe4
# cursor-text = #181a1f
# selection-background = #3e4451
# selection-foreground =

# The 16 basic colors, one entry per line. Entries 16 to 255 default to the
# standard xterm colors and can be set the same way.
# palette = 0=#282c34
# palette = 1=#e06c75
# palette = 2=#98c379
# palette = 3=#e5c07b
# palette = 4=#61afef
# palette = 5=#c678dd
# palette = 6=#56b6c2
# palette = 7=#dcdfe4
# palette = 8=#5a6374
# palette = 9=#ef7d86
# palette = 10=#a9d48a
# palette = 11=#f0d08c
# palette = 12=#74bdf7
# palette = 13=#d38aea
# palette = 14=#67c7d3
# palette = 15=#ffffff

# Draw bold text in the bright versions of the first eight colors.
# bold-is-bright = false

# --- Cursor ---

# block, bar or underline.
# cursor-style = block
# cursor-style-blink = false

# --- Windows and panes ---

# Space between a pane's edges and its text, in points. In a window with one
# pane, what is left over when the space inside does not divide into whole
# cells is shared between the two sides, so the text sits centered.
# window-padding-x = 10
# window-padding-y = 10

# The size of a new window in cells: 10 to 1000 wide, 4 to 1000 high
# (new windows only).
# window-width = 100
# window-height = 30

# Where the first window opens, in points from the left and top edges of the
# main screen's visible area. A negative value counts from the right or
# bottom edge to the window's far edge. Empty: centered. (Read at launch.)
# window-position-x =
# window-position-y =

# How much to fade panes that do not have focus toward their background,
# from 0 (not at all) to 1.
# unfocused-split-dim = 0.25

# The color of the lines between panes. Empty: derived from the background.
# split-divider-color =

# Ask before closing a pane whose running program is not the shell.
# confirm-close = true

# Quit when the last window closes.
# quit-after-last-window-closed = false

# Save and restore open windows, tabs, split layouts and working directories in
# ~/.local/state/tern/session: default, always or never.
# window-save-state = default

# --- Terminal ---

# Lines of history kept for each pane, 0 to 1000000 (new panes only).
# scrollback-lines = 10000

# The command to run in new panes. Empty: your login shell (new panes only).
# shell =

# Where new tabs and splits start: home, inherit (the focused pane's
# directory) or an absolute path (~/ allowed) (new panes only).
# working-directory = inherit

# Command run when Command-clicking a file path (for example
# "zed {file}:{line}:{col}" or "code -g {file}:{line}:{col}"). Empty: open in
# macOS's default app, or reveal executables in the Finder.
# open-command =

# --- Keyboard and mouse ---

# Which Option keys act as Alt (Meta) instead of typing special characters:
# true (both), false (neither), left or right.
# option-as-alt = true

# Copy text to the clipboard as soon as it is selected.
# copy-on-select = false

# Hide the mouse pointer while typing.
# mouse-hide-while-typing = true

# --- Key bindings ---

# Each keybind line is <trigger>=<action>. It adds a binding, or replaces the
# one its trigger already has. "keybind = clear" removes every binding made
# before it, the defaults included. The action "unbind" removes one trigger's
# binding so the key reaches the program in the terminal; "ignore" makes the
# key do nothing at all.
#
# A trigger is modifiers and a key joined by +. The modifiers are cmd, ctrl,
# alt (or opt) and shift. The key is a single character or one of enter, tab,
# backspace, escape, space, up, down, left, right, home, end, page_up,
# page_down, insert, delete, f1 to f24, plus, minus, equal, comma, period,
# slash, backslash, semicolon, apostrophe, grave, left_bracket, right_bracket,
# and on the keypad kp_0 to kp_9, kp_decimal, kp_divide, kp_multiply,
# kp_subtract, kp_add, kp_enter and kp_equal. A key is named by what it types
# without Shift: cmd+shift+d is the D key with Command and Shift held.
#
# The actions:
#   new_window  new_tab  close_pane  close_tab  close_window
#   split:right (or down, left, up)
#   focus_split:left (or right, up, down, next, previous)
#   resize_split:up,10 (a direction and a distance in points)
#   equalize_splits  toggle_zoom
#   goto_tab:1 to goto_tab:9 (9 is the last tab)  next_tab  previous_tab
#   move_tab:1 (places to move the tab; negative moves it left)
#   toggle_tab_bar  merge_all_windows  move_tab_to_new_window  rename_tab
#   copy  paste  select_all  select_command_output  find  find_next
#   find_previous  clear
#   jump_prompt:previous (or next)
#   scroll:page_up (or page_down, top, bottom, line_up, line_down)
#   font_size:increase (or decrease, reset)
#   toggle_fullscreen  open_config  reload_config  quit  ignore
#   text:<text> sends the text, with the escapes \x1b (any byte in hex), \e,
#     \n, \r, \t and \\
#   esc:<text> sends ESC and then the text; csi:<text> sends ESC [ and then
#     the text
#
# The default bindings:
# keybind = cmd+n=new_window
# keybind = cmd+t=new_tab
# keybind = cmd+shift+t=toggle_tab_bar
# keybind = cmd+w=close_pane
# keybind = cmd+shift+w=close_tab
# keybind = cmd+d=split:right
# keybind = cmd+shift+d=split:down
# keybind = cmd+[=focus_split:previous
# keybind = cmd+]=focus_split:next
# keybind = cmd+alt+left=focus_split:left
# keybind = cmd+alt+right=focus_split:right
# keybind = cmd+alt+up=focus_split:up
# keybind = cmd+alt+down=focus_split:down
# keybind = cmd+ctrl+left=resize_split:left,10
# keybind = cmd+ctrl+right=resize_split:right,10
# keybind = cmd+ctrl+up=resize_split:up,10
# keybind = cmd+ctrl+down=resize_split:down,10
# keybind = cmd+ctrl+equal=equalize_splits
# keybind = cmd+shift+enter=toggle_zoom
# keybind = cmd+1=goto_tab:1
# keybind = cmd+2=goto_tab:2
# keybind = cmd+3=goto_tab:3
# keybind = cmd+4=goto_tab:4
# keybind = cmd+5=goto_tab:5
# keybind = cmd+6=goto_tab:6
# keybind = cmd+7=goto_tab:7
# keybind = cmd+8=goto_tab:8
# keybind = cmd+9=goto_tab:9
# keybind = cmd+shift+]=next_tab
# keybind = ctrl+tab=next_tab
# keybind = cmd+shift+[=previous_tab
# keybind = ctrl+shift+tab=previous_tab
# keybind = cmd+c=copy
# keybind = cmd+v=paste
# keybind = cmd+a=select_all
# keybind = cmd+shift+a=select_command_output
# keybind = cmd+f=find
# keybind = cmd+g=find_next
# keybind = cmd+shift+g=find_previous
# keybind = cmd+k=clear
# keybind = cmd+up=jump_prompt:previous
# keybind = cmd+down=jump_prompt:next
# keybind = shift+page_up=scroll:page_up
# keybind = shift+page_down=scroll:page_down
# keybind = cmd+home=scroll:top
# keybind = cmd+end=scroll:bottom
# keybind = cmd+equal=font_size:increase
# keybind = cmd+plus=font_size:increase
# keybind = cmd+shift+equal=font_size:increase
# keybind = cmd+minus=font_size:decrease
# keybind = cmd+0=font_size:reset
# keybind = cmd+ctrl+f=toggle_fullscreen
# keybind = cmd+comma=open_config
# keybind = cmd+shift+comma=reload_config
# keybind = cmd+q=quit
# keybind = cmd+left=text:\x01
# keybind = cmd+right=text:\x05
# keybind = cmd+backspace=text:\x15
# keybind = alt+left=esc:b
# keybind = alt+right=esc:f
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::Bindings;
    use crate::config::tests::{parse_clean, KEYS};
    use crate::Config;

    /// The setting lines of the template, uncommented; explanation lines
    /// are left out.
    fn setting_lines() -> Vec<&'static str> {
        DEFAULT_CONFIG_TEMPLATE
            .lines()
            .filter_map(|line| line.strip_prefix("# "))
            .filter(|line| {
                line.split_once('=')
                    .is_some_and(|(key, _)| KEYS.contains(&key.trim()))
            })
            .collect()
    }

    #[test]
    fn the_template_is_the_default_configuration() {
        assert_eq!(parse_clean(DEFAULT_CONFIG_TEMPLATE), Config::default());
        assert_eq!(
            Config::parse(DEFAULT_CONFIG_TEMPLATE),
            (Config::default(), Vec::new())
        );
    }

    #[test]
    fn uncommenting_every_setting_changes_nothing() {
        let uncommented = setting_lines().join("\n");
        assert_eq!(parse_clean(&uncommented), Config::default());
    }

    #[test]
    fn the_template_lists_every_key() {
        let lines = setting_lines();
        for key in KEYS {
            let listed = lines
                .iter()
                .any(|line| line.split_once('=').is_some_and(|(k, _)| k.trim() == *key));
            assert!(listed, "{key} is missing from the template");
        }
    }

    #[test]
    fn the_template_lists_the_default_bindings_in_order() {
        let listed: Vec<&str> = setting_lines()
            .into_iter()
            .filter_map(|line| line.strip_prefix("keybind = "))
            .collect();
        let defaults: Vec<String> = Bindings::default()
            .iter()
            .map(|(t, a)| format!("{t}={a}"))
            .collect();
        assert_eq!(listed, defaults);
    }

    #[test]
    fn the_template_lists_the_sixteen_default_base_colors() {
        let palette: Vec<&str> = setting_lines()
            .into_iter()
            .filter_map(|line| line.strip_prefix("palette = "))
            .collect();
        let defaults: Vec<String> = tern_types::Theme::default().palette[..16]
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{i}=#{c:06x}"))
            .collect();
        assert_eq!(palette, defaults);
    }

    #[test]
    fn every_line_is_a_comment() {
        for line in DEFAULT_CONFIG_TEMPLATE.lines() {
            assert!(
                line.is_empty() || line == "#" || line.starts_with("# "),
                "{line:?}"
            );
        }
    }
    #[test]
    fn the_template_names_every_key_name() {
        let words: Vec<&str> = DEFAULT_CONFIG_TEMPLATE
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .collect();
        for name in crate::bindings::KEY_NAMES.iter().map(|(name, _)| *name) {
            assert!(words.contains(&name), "{name} is missing from the template");
        }
    }
}
