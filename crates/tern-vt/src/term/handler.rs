//! The terminal as the parser's handler: each call of the parser goes to
//! the terminal's own method of the same name. SUB needs nothing from the
//! terminal: the parser cancels the sequence itself.

use crate::ansi::{
    Attr, CharsetIndex, ClearMode, CursorShape, CursorStyle, Hyperlink, KeyboardModes,
    KeyboardModesApplyBehavior, LineClearMode, Mode, PrivateMode, Rgb, StandardCharset,
    TabulationClearMode,
};
use crate::event::EventListener;
use crate::parser::Handler;
use crate::term::Term;

impl<T: EventListener> Handler for Term<T> {
    #[inline]
    fn set_title(&mut self, title: Option<String>) {
        Term::set_title(self, title);
    }

    #[inline]
    fn set_cursor_style(&mut self, style: Option<CursorStyle>) {
        Term::set_cursor_style(self, style);
    }

    #[inline]
    fn set_cursor_shape(&mut self, shape: CursorShape) {
        Term::set_cursor_shape(self, shape);
    }

    #[inline]
    fn input(&mut self, c: char) {
        Term::input(self, c);
    }

    #[inline]
    fn goto(&mut self, line: i32, column: usize) {
        Term::goto(self, line, column);
    }

    #[inline]
    fn goto_line(&mut self, line: i32) {
        Term::goto_line(self, line);
    }

    #[inline]
    fn goto_col(&mut self, column: usize) {
        Term::goto_col(self, column);
    }

    #[inline]
    fn insert_blank(&mut self, count: usize) {
        Term::insert_blank(self, count);
    }

    #[inline]
    fn move_up(&mut self, lines: usize) {
        Term::move_up(self, lines);
    }

    #[inline]
    fn move_down(&mut self, lines: usize) {
        Term::move_down(self, lines);
    }

    #[inline]
    fn identify_terminal(&mut self, intermediate: Option<char>) {
        Term::identify_terminal(self, intermediate);
    }

    #[inline]
    fn device_status(&mut self, report: usize) {
        Term::device_status(self, report);
    }

    #[inline]
    fn move_forward(&mut self, columns: usize) {
        Term::move_forward(self, columns);
    }

    #[inline]
    fn move_backward(&mut self, columns: usize) {
        Term::move_backward(self, columns);
    }

    #[inline]
    fn move_down_and_cr(&mut self, lines: usize) {
        Term::move_down_and_cr(self, lines);
    }

    #[inline]
    fn move_up_and_cr(&mut self, lines: usize) {
        Term::move_up_and_cr(self, lines);
    }

    #[inline]
    fn put_tab(&mut self, count: u16) {
        Term::put_tab(self, count);
    }

    #[inline]
    fn backspace(&mut self) {
        Term::backspace(self);
    }

    #[inline]
    fn carriage_return(&mut self) {
        Term::carriage_return(self);
    }

    #[inline]
    fn linefeed(&mut self) {
        Term::linefeed(self);
    }

    #[inline]
    fn bell(&mut self) {
        Term::bell(self);
    }

    #[inline]
    fn set_horizontal_tabstop(&mut self) {
        Term::set_horizontal_tabstop(self);
    }

    #[inline]
    fn scroll_up(&mut self, lines: usize) {
        Term::scroll_up(self, lines);
    }

    #[inline]
    fn scroll_down(&mut self, lines: usize) {
        Term::scroll_down(self, lines);
    }

    #[inline]
    fn insert_blank_lines(&mut self, count: usize) {
        Term::insert_blank_lines(self, count);
    }

    #[inline]
    fn delete_lines(&mut self, count: usize) {
        Term::delete_lines(self, count);
    }

    #[inline]
    fn erase_chars(&mut self, count: usize) {
        Term::erase_chars(self, count);
    }

    #[inline]
    fn delete_chars(&mut self, count: usize) {
        Term::delete_chars(self, count);
    }

    #[inline]
    fn move_backward_tabs(&mut self, count: u16) {
        Term::move_backward_tabs(self, count);
    }

    #[inline]
    fn move_forward_tabs(&mut self, count: u16) {
        Term::move_forward_tabs(self, count);
    }

    #[inline]
    fn save_cursor_position(&mut self) {
        Term::save_cursor_position(self);
    }

    #[inline]
    fn restore_cursor_position(&mut self) {
        Term::restore_cursor_position(self);
    }

    #[inline]
    fn clear_line(&mut self, mode: LineClearMode) {
        Term::clear_line(self, mode);
    }

    #[inline]
    fn clear_screen(&mut self, mode: ClearMode) {
        Term::clear_screen(self, mode);
    }

    #[inline]
    fn clear_tabs(&mut self, mode: TabulationClearMode) {
        Term::clear_tabs(self, mode);
    }

    #[inline]
    fn reset_state(&mut self) {
        Term::reset_state(self);
    }

    #[inline]
    fn reverse_index(&mut self) {
        Term::reverse_index(self);
    }

    #[inline]
    fn terminal_attribute(&mut self, attr: Attr) {
        Term::terminal_attribute(self, attr);
    }

    #[inline]
    fn set_mode(&mut self, mode: Mode) {
        Term::set_mode(self, mode);
    }

    #[inline]
    fn unset_mode(&mut self, mode: Mode) {
        Term::unset_mode(self, mode);
    }

    #[inline]
    fn report_mode(&mut self, mode: Mode) {
        Term::report_mode(self, mode);
    }

    #[inline]
    fn set_private_mode(&mut self, mode: PrivateMode) {
        Term::set_private_mode(self, mode);
    }

    #[inline]
    fn unset_private_mode(&mut self, mode: PrivateMode) {
        Term::unset_private_mode(self, mode);
    }

    #[inline]
    fn report_private_mode(&mut self, mode: PrivateMode) {
        Term::report_private_mode(self, mode);
    }

    #[inline]
    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        Term::set_scrolling_region(self, top, bottom);
    }

    #[inline]
    fn set_keypad_application_mode(&mut self) {
        Term::set_keypad_application_mode(self);
    }

    #[inline]
    fn unset_keypad_application_mode(&mut self) {
        Term::unset_keypad_application_mode(self);
    }

    #[inline]
    fn set_active_charset(&mut self, index: CharsetIndex) {
        Term::set_active_charset(self, index);
    }

    #[inline]
    fn configure_charset(&mut self, index: CharsetIndex, charset: StandardCharset) {
        Term::configure_charset(self, index, charset);
    }

    #[inline]
    fn set_color(&mut self, index: usize, color: Rgb) {
        Term::set_color(self, index, color);
    }

    #[inline]
    fn dynamic_color_sequence(&mut self, prefix: String, index: usize, terminator: &str) {
        Term::dynamic_color_sequence(self, prefix, index, terminator);
    }

    #[inline]
    fn reset_color(&mut self, index: usize) {
        Term::reset_color(self, index);
    }

    #[inline]
    fn clipboard_store(&mut self, clipboard: u8, base64: &[u8]) {
        Term::clipboard_store(self, clipboard, base64);
    }

    #[inline]
    fn clipboard_load(&mut self, clipboard: u8, terminator: &str) {
        Term::clipboard_load(self, clipboard, terminator);
    }

    #[inline]
    fn decaln(&mut self) {
        Term::decaln(self);
    }

    #[inline]
    fn push_title(&mut self) {
        Term::push_title(self);
    }

    #[inline]
    fn pop_title(&mut self) {
        Term::pop_title(self);
    }

    #[inline]
    fn text_area_size_pixels(&mut self) {
        Term::text_area_size_pixels(self);
    }

    #[inline]
    fn text_area_size_chars(&mut self) {
        Term::text_area_size_chars(self);
    }

    #[inline]
    fn set_hyperlink(&mut self, link: Option<Hyperlink>) {
        Term::set_hyperlink(self, link);
    }

    #[inline]
    fn report_keyboard_mode(&mut self) {
        Term::report_keyboard_mode(self);
    }

    #[inline]
    fn push_keyboard_mode(&mut self, modes: KeyboardModes) {
        Term::push_keyboard_mode(self, modes);
    }

    #[inline]
    fn pop_keyboard_modes(&mut self, count: u16) {
        Term::pop_keyboard_modes(self, count);
    }

    #[inline]
    fn set_keyboard_mode(&mut self, modes: KeyboardModes, apply: KeyboardModesApplyBehavior) {
        Term::set_keyboard_mode(self, modes, apply);
    }

    #[inline]
    fn set_working_directory(&mut self, uri: String) {
        Term::set_working_directory(self, &uri);
    }

    #[inline]
    fn prompt_mark(&mut self, kind: u8) {
        Term::prompt_mark(self, kind);
    }

    #[inline]
    fn notify(&mut self, title: Option<String>, body: String) {
        Term::notify(self, title, body);
    }

    #[inline]
    fn set_progress(&mut self, progress: crate::event::Progress) {
        Term::set_progress(self, progress);
    }
}
