//! Finding links in a pane: plain http and https URLs and existing local file
//! paths (`path`, `path:line`, `path:line:col`, `path(line,col)`) in a row's text.
//! (Links a program marked with OSC 8 come from the session itself.)

use std::path::{Path, PathBuf};

use tern_types::{flags, Frame};

/// A resolved local file or directory target with optional 1-based position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileTarget {
    pub path: PathBuf,
    pub line: Option<u32>,
    pub col: Option<u32>,
}

/// What Command-click opens under a cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClickTarget {
    Url(String),
    File(FileTarget),
}

/// Characters that end a URL written in running text.
fn ends_url(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(c, '<' | '>' | '"' | '`' | '{' | '}' | '|' | '\\' | '^')
}

/// The http or https URL in a row that covers the cell at col, if any.
pub fn url_at(frame: &Frame, row: u16, col: u16) -> Option<String> {
    if row >= frame.rows || col >= frame.cols {
        return None;
    }
    // The row's characters, each with the column it starts in; the second
    // half of a wide character belongs to the first.
    let mut chars: Vec<(char, u16)> = Vec::with_capacity(frame.cols as usize);
    let mut at = None;
    for (c, cell) in frame.row(row).iter().enumerate() {
        let c = c as u16;
        if cell.has(flags::WIDE_SPACER) {
            if c == col {
                at = chars.len().checked_sub(1);
            }
            continue;
        }
        if c == col {
            at = Some(chars.len());
        }
        chars.push((cell.char(), c));
    }
    let at = at?;
    if ends_url(chars[at].0) {
        return None;
    }
    let mut start = at;
    while start > 0 && !ends_url(chars[start - 1].0) {
        start -= 1;
    }
    let mut end = at + 1;
    while end < chars.len() && !ends_url(chars[end].0) {
        end += 1;
    }
    let word: String = chars[start..end].iter().map(|(c, _)| c).collect();
    // The last scheme that begins at or before the pointer.
    let offset = |i: usize| word[..i].chars().count();
    let begin = ["https://", "http://"]
        .iter()
        .flat_map(|scheme| word.match_indices(scheme).map(|(i, _)| i))
        .filter(|&i| offset(i) <= at - start)
        .max()?;
    let url = trim_url(&word[begin..]);
    let url_end = start + offset(begin) + url.chars().count();
    let scheme_len = if url.starts_with("https://") { 8 } else { 7 };
    (url.len() > scheme_len && at < url_end).then(|| url.to_string())
}

/// Leaves off punctuation that ends the sentence around a URL rather than
/// the URL itself, and closing brackets that have no opening one in it.
fn trim_url(url: &str) -> &str {
    let mut url = url;
    loop {
        let Some(last) = url.chars().last() else {
            return url;
        };
        let unbalanced = |open: char, close: char| {
            last == close && url.matches(open).count() < url.matches(close).count()
        };
        let trailing = matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*')
            || unbalanced('(', ')')
            || unbalanced('[', ']');
        if !trailing {
            return url;
        }
        url = &url[..url.len() - last.len_utf8()];
    }
}

fn ends_path_token(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(c, '<' | '>' | '"' | '\'' | '`' | '{' | '}' | '[' | ']' | '|')
}

/// Resolves an existing local file or directory path at `(row, col)` (including
/// optional `:line[:col]` or `(line[,col])` suffixes).
pub fn path_at(
    frame: &Frame,
    row: u16,
    col: u16,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Option<FileTarget> {
    if row >= frame.rows || col >= frame.cols {
        return None;
    }
    let mut chars: Vec<(char, u16)> = Vec::with_capacity(frame.cols as usize);
    let mut at = None;
    for (c, cell) in frame.row(row).iter().enumerate() {
        let c = c as u16;
        if cell.has(flags::WIDE_SPACER) {
            if c == col {
                at = chars.len().checked_sub(1);
            }
            continue;
        }
        if c == col {
            at = Some(chars.len());
        }
        chars.push((cell.char(), c));
    }
    let at = at?;
    if ends_path_token(chars[at].0) {
        return None;
    }
    let mut start = at;
    while start > 0 && !ends_path_token(chars[start - 1].0) {
        start -= 1;
    }
    let mut end = at + 1;
    while end < chars.len() && !ends_path_token(chars[end].0) {
        end += 1;
    }
    let raw: String = chars[start..end].iter().map(|(c, _)| c).collect();
    parse_and_resolve_path(&raw, cwd, home)
}

fn parse_and_resolve_path(
    raw: &str,
    cwd: Option<&Path>,
    home: Option<&Path>,
) -> Option<FileTarget> {
    let mut token = raw.trim_matches(|c| matches!(c, ',' | ';' | '!' | '?'));
    token = token.trim_end_matches('.');
    if let Some(inner) = token.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
        token = inner;
    } else if let Some(inner) = token.strip_prefix('(') {
        token = inner;
    }
    if token.is_empty() || token.contains("://") {
        return None;
    }

    let (base, line, col) = split_line_col(token);
    let base = base.trim_end_matches('.');
    if base.is_empty() || base == "." || base == ".." {
        return None;
    }

    let resolved = resolve_existing(base, cwd, home)?;
    Some(FileTarget {
        path: std::fs::canonicalize(&resolved).unwrap_or(resolved),
        line,
        col,
    })
}

fn split_line_col(token: &str) -> (&str, Option<u32>, Option<u32>) {
    // Handle `path(line,col)` or `path(line)`.
    if let Some(without_close) = token.strip_suffix(')')
        && let Some((head, inside)) = without_close.rsplit_once('(')
            && !head.is_empty() {
                if let Some((l, c)) = inside.split_once(',') {
                    if let (Ok(line), Ok(col)) = (l.trim().parse::<u32>(), c.trim().parse::<u32>())
                    {
                        return (head, Some(line), Some(col));
                    }
                } else if let Ok(line) = inside.trim().parse::<u32>() {
                    return (head, Some(line), None);
                }
            }

    // Handle `path:line:col:`, `path:line:col`, `path:line:`, `path:line`.
    let trimmed = token.strip_suffix(':').unwrap_or(token);
    if let Some((rest, last)) = trimmed.rsplit_once(':')
        && let Ok(n2) = last.parse::<u32>() {
            if let Some((head, prev)) = rest.rsplit_once(':')
                && !head.is_empty()
                    && let Ok(n1) = prev.parse::<u32>() {
                        return (head, Some(n1), Some(n2));
                    }
            if !rest.is_empty() {
                return (rest, Some(n2), None);
            }
        }

    (trimmed, None, None)
}

fn resolve_existing(base: &str, cwd: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    if base == "~" {
        let p = home?.to_path_buf();
        return p.exists().then_some(p);
    }
    if let Some(rel) = base.strip_prefix("~/") {
        let p = home?.join(rel);
        return p.exists().then_some(p);
    }
    let path = Path::new(base);
    if path.is_absolute() {
        return path.exists().then(|| path.to_path_buf());
    }
    let dir = cwd?;
    let direct = dir.join(path);
    if direct.exists() {
        return Some(direct);
    }
    // Support `git diff` a/ and b/ prefixes when run from repository root.
    if let Some(stripped) = base.strip_prefix("a/").or_else(|| base.strip_prefix("b/")) {
        let alt = dir.join(stripped);
        if alt.exists() {
            return Some(alt);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tern_types::Cell;

    /// A one-row frame showing text; wide characters take two cells.
    fn row(text: &str) -> Frame {
        let mut cells = Vec::new();
        for c in text.chars() {
            let mut cell = Cell::blank(0, 0);
            cell.ch = u32::from(c);
            let wide = matches!(c, '日' | '本');
            if wide {
                cell.flags |= flags::WIDE;
            }
            cells.push(cell);
            if wide {
                let mut spacer = Cell::blank(0, 0);
                spacer.flags |= flags::WIDE_SPACER;
                cells.push(spacer);
            }
        }
        let mut frame = Frame::default();
        frame.resize(cells.len() as u16, 1);
        frame.row_mut(0).copy_from_slice(&cells);
        frame
    }

    #[test]
    fn urls_are_found_under_the_pointer() {
        let f = row("see https://example.com/a?b=c#d for more");
        let url = Some("https://example.com/a?b=c#d".to_string());
        assert_eq!(url_at(&f, 0, 4), url, "first character");
        assert_eq!(url_at(&f, 0, 30), url, "last character");
        assert_eq!(url_at(&f, 0, 31), None, "the space after");
        assert_eq!(url_at(&f, 0, 2), None, "a word before");
        assert_eq!(url_at(&f, 0, 99), None, "past the row");
    }

    #[test]
    fn sentence_punctuation_and_brackets_stay_out() {
        let f = row("(at http://x.org/wiki/A_(b)), then.");
        assert_eq!(
            url_at(&f, 0, 10).as_deref(),
            Some("http://x.org/wiki/A_(b)")
        );
        let f = row("go to https://a.b/c.");
        assert_eq!(url_at(&f, 0, 8).as_deref(), Some("https://a.b/c"));
        assert_eq!(url_at(&f, 0, 19), None, "on the full stop");
        let f = row("<https://a.b/> \"http://c.d\"");
        assert_eq!(url_at(&f, 0, 3).as_deref(), Some("https://a.b/"));
        assert_eq!(url_at(&f, 0, 20).as_deref(), Some("http://c.d"));
    }

    #[test]
    fn only_web_urls_count() {
        assert_eq!(url_at(&row("ftp://x.y/z"), 0, 3), None);
        assert_eq!(url_at(&row("https://"), 0, 3), None, "a scheme alone");
        assert_eq!(
            url_at(&row("xhttps://a.b"), 0, 0),
            None,
            "before the scheme"
        );
        assert_eq!(
            url_at(&row("xhttps://a.b"), 0, 5).as_deref(),
            Some("https://a.b")
        );
    }

    #[test]
    fn wide_characters_keep_columns_right() {
        let f = row("日本 https://例え.jp/パス x");
        let url = Some("https://例え.jp/パス".to_string());
        assert_eq!(url_at(&f, 0, 1), None, "the spacer of a wide character");
        assert_eq!(url_at(&f, 0, 5), url);
        assert_eq!(url_at(&f, 0, 20), url);
    }

    #[test]
    fn resolves_paths_with_line_col_and_git_diff_prefixes() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let canon = std::fs::canonicalize(dir.join("src/main.rs")).unwrap();

        let f = row(" --> src/main.rs:42:9: error");
        assert_eq!(
            path_at(&f, 0, 8, Some(&dir), None),
            Some(FileTarget {
                path: canon.clone(),
                line: Some(42),
                col: Some(9),
            })
        );

        let f_diff = row("--- a/src/main.rs");
        assert_eq!(
            path_at(&f_diff, 0, 8, Some(&dir), None),
            Some(FileTarget {
                path: canon.clone(),
                line: None,
                col: None,
            })
        );

        let f_paren = row("at ~/src/main.rs(17,3).");
        assert_eq!(
            path_at(&f_paren, 0, 8, None, Some(&dir)),
            Some(FileTarget {
                path: canon,
                line: Some(17),
                col: Some(3),
            })
        );

        let f_missing = row("see src/does_not_exist.rs:10");
        assert_eq!(path_at(&f_missing, 0, 6, Some(&dir), None), None);
    }
}
