//! Pasted text and dropped files.

/// The bytes for pasting text.
///
/// With bracketed paste (mode 2004) the text goes between ESC[200~ and
/// ESC[201~, without its ESC and ETX bytes, so it can neither end the
/// bracket early nor interrupt the program. Without it, CR LF and LF become
/// CR, what Return sends, so each line is entered as if typed.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 12);
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
        // These bytes never occur inside a UTF-8 sequence, so dropping them
        // leaves the rest valid.
        out.extend(text.bytes().filter(|&b| b != 0x1b && b != 0x03));
        out.extend_from_slice(b"\x1b[201~");
    } else {
        let mut bytes = text.bytes().peekable();
        while let Some(b) = bytes.next() {
            match b {
                b'\r' => {
                    out.push(b'\r');
                    bytes.next_if_eq(&b'\n');
                }
                b'\n' => out.push(b'\r'),
                _ => out.push(b),
            }
        }
    }
    out
}

/// A path written so a shell reads it back as one word, unchanged.
///
/// Characters a POSIX shell treats specially get a backslash, as do = and ^
/// (zsh, the macOS default, expands a leading =word and may treat ^ as a
/// pattern). Newlines and other control characters are put in single
/// quotes instead: a backslash before a newline would join the lines. Other
/// characters, UTF-8 letters included, are left alone.
fn shell_escape(path: &str) -> String {
    if path.is_empty() {
        return "''".to_owned();
    }
    let mut out = String::with_capacity(path.len() + path.len() / 4);
    for c in path.chars() {
        match c {
            ' ' | '\t' | '\'' | '"' | '\\' | '(' | ')' | '[' | ']' | '{' | '}' | '$' | '&'
            | ';' | '|' | '<' | '>' | '*' | '?' | '!' | '#' | '~' | '`' | '=' | '^' => {
                out.push('\\');
                out.push(c);
            }
            _ if c.is_ascii_control() => {
                out.push('\'');
                out.push(c);
                out.push('\'');
            }
            _ => out.push(c),
        }
    }
    out
}

/// The text to type for files dropped on a terminal: each path escaped for
/// the shell, joined by single spaces.
pub fn drop_text<I>(paths: I) -> String
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let mut out = String::new();
    for (i, path) in paths.into_iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&shell_escape(path.as_ref()));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::show;

    #[test]
    fn bracketed_paste() {
        let rows = [
            ("hello", "\\x1b[200~hello\\x1b[201~"),
            ("", "\\x1b[200~\\x1b[201~"),
            // Line breaks stay as they are inside the bracket.
            ("a\nb\r\nc\rd", "\\x1b[200~a\\nb\\r\\nc\\rd\\x1b[201~"),
            // A paste cannot close the bracket or send Ctrl+C.
            (
                "a\x1b[201~rm -rf ~\n",
                "\\x1b[200~a[201~rm -rf ~\\n\\x1b[201~",
            ),
            ("stop\x03here\x1b", "\\x1b[200~stophere\\x1b[201~"),
            ("tab\tbell\x07", "\\x1b[200~tab\\tbell\\x07\\x1b[201~"),
            (
                "日本 é",
                "\\x1b[200~\\xe6\\x97\\xa5\\xe6\\x9c\\xac \\xc3\\xa9\\x1b[201~",
            ),
        ];
        for (text, want) in rows {
            assert_eq!(show(&paste_bytes(text, true)), want, "{text:?}");
        }
    }

    #[test]
    fn plain_paste() {
        let rows = [
            ("hello", "hello"),
            ("", ""),
            ("a\nb", "a\\rb"),
            ("a\r\nb", "a\\rb"),
            ("a\rb", "a\\rb"),
            ("\n\n", "\\r\\r"),
            ("\r\n\r\n", "\\r\\r"),
            ("\r\r\n", "\\r\\r"),
            ("\n\r", "\\r\\r"),
            ("end\r\n", "end\\r"),
            // Nothing else changes, escape sequences included.
            ("a\x1b[31mb\x03", "a\\x1b[31mb\\x03"),
            ("日本\n", "\\xe6\\x97\\xa5\\xe6\\x9c\\xac\\r"),
        ];
        for (text, want) in rows {
            assert_eq!(show(&paste_bytes(text, false)), want, "{text:?}");
        }
    }

    #[test]
    fn escaped_forms() {
        let rows = [
            ("plain", "plain"),
            ("/Users/me/file.txt", "/Users/me/file.txt"),
            ("a b", "a\\ b"),
            ("it's", "it\\'s"),
            ("$HOME", "\\$HOME"),
            ("~/x", "\\~/x"),
            ("a\nb", "a'\n'b"),
            ("tab\there", "tab\\\there"),
            ("日本語 ファイル", "日本語\\ ファイル"),
            ("-rf", "-rf"),
            ("", "''"),
        ];
        for (path, want) in rows {
            assert_eq!(shell_escape(path), want, "{path:?}");
        }
    }

    /// Names a dropped file could have, with every character a shell
    /// treats specially.
    fn awkward_names() -> Vec<String> {
        let mut names: Vec<String> = [
            "plain",
            "with space",
            "two  spaces",
            " leading space",
            "trailing space ",
            "tab\there",
            "it's",
            "say \"hi\"",
            "'quoted'",
            "back\\slash",
            "trailing backslash\\",
            "(paren)",
            "[bracket]",
            "{brace}",
            "{a,b}",
            "$HOME",
            "$(whoami)",
            "${PATH}",
            "`whoami`",
            "a&b",
            "a&&b",
            "a;b",
            "a|b",
            "<in>",
            ">out",
            "*",
            "*.txt",
            "?",
            "!bang",
            "!!",
            "#hash",
            "a#b",
            "~",
            "~root",
            "a~b",
            "=equals",
            "a=b",
            "^caret",
            "%percent",
            "100%",
            "a,b",
            "new\nline",
            "\n",
            "two\n\nlines",
            "carriage\rreturn",
            "ctrl\u{1}a",
            "escape\u{1b}[31m",
            "del\u{7f}",
            "-n",
            "--",
            "-rf",
            "--help",
            "-",
            "日本語 ファイル",
            "émoji 😀",
            "café",
            "no\u{a0}break",
            "Ω≈ç√",
            "/Users/someone/My Files/report (final) [v2].pdf",
            "",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        // Every printable ASCII character in one name.
        names.push((' '..='~').collect());
        // Every ASCII control character except NUL, which no path can hold.
        names.push(('\u{1}'..='\u{1f}').chain(['\u{7f}']).collect());
        names
    }

    /// Runs printf in the shell with the escaped words as its arguments.
    fn shell_prints(shell: &str, format: &str, words: &str) -> Vec<u8> {
        let script = format!("printf '{format}' {words}");
        let output = Command::new(shell)
            .args(if shell.ends_with("zsh") {
                &["-f", "-c"][..]
            } else {
                &["-c"][..]
            })
            .arg(&script)
            .output()
            .unwrap_or_else(|e| panic!("cannot run {shell}: {e}"));
        assert!(
            output.status.success(),
            "{shell} failed on {script:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    #[test]
    fn escaped_names_survive_the_shell() {
        for shell in ["/bin/sh", "/bin/zsh"] {
            for name in awkward_names() {
                let printed = shell_prints(shell, "%s", &shell_escape(&name));
                assert_eq!(
                    show(&printed),
                    show(name.as_bytes()),
                    "{shell} {name:?} as {:?}",
                    shell_escape(&name)
                );
            }
        }
    }

    #[test]
    fn dropped_files() {
        assert_eq!(drop_text(["/tmp/a b", "/tmp/c"]), "/tmp/a\\ b /tmp/c");
        assert_eq!(drop_text(["/tmp/one"]), "/tmp/one");
        assert_eq!(drop_text(Vec::<String>::new()), "");
        let names = awkward_names();
        // One word per path: printf repeats its format for each argument.
        for shell in ["/bin/sh", "/bin/zsh"] {
            let printed = shell_prints(shell, "%s\\0", &drop_text(&names));
            let words: Vec<&[u8]> = printed.split(|&b| b == 0).collect();
            let (last, words) = words.split_last().expect("split yields at least one piece");
            assert!(last.is_empty(), "{shell}: output must end with NUL");
            let want: Vec<&[u8]> = names.iter().map(String::as_bytes).collect();
            assert_eq!(words, want.as_slice(), "{shell}");
        }
    }
}
