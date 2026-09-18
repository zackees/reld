//! Reld's only splitter for GNU-style `@file` response files.
//!
//! The grammar we implement here is LLVM's `cl::TokenizeGNUCommandLine`
//! (`llvm/lib/Support/CommandLine.cpp`), which is what lld's ELF driver applies to `@file`
//! arguments via `cl::ExpandResponseFiles` (`lld/ELF/DriverUtils.cpp`). GNU ld's response-file
//! handling (libiberty's `buildargv`) accepts the same quoting, so a single tokenizer here is
//! enough for every ELF `@file` reader.
//!
//! The rules, matching LLVM exactly:
//!
//! * Separators are exactly space, tab, CR and LF (LLVM's `isWhitespace`), not Unicode
//!   whitespace in general.
//! * A backslash outside quotes takes the next character literally. A trailing lone backslash
//!   (with nothing after it) is kept as a literal `\`.
//! * A `'` or `"` opens a quoted run anywhere within a token, closed by a matching quote
//!   character. Inside the quotes, a backslash also escapes the next character, with a trailing
//!   lone backslash kept as a literal `\`.
//! * Quoted and unquoted pieces concatenate into a single token, e.g. `foo"bar baz"` becomes
//!   `foobar baz`.
//! * An unterminated quote simply ends the input; the token collected so far is kept rather than
//!   producing an error, matching LLVM's behaviour.
//! * Empty tokens (e.g. `''` or `""`) are dropped.
//! * A leading UTF-8 BOM (U+FEFF) is skipped, as LLVM's `ExpandResponseFile` does.

/// Tokenizes the contents of a GNU-style `@file` response file, following LLVM's
/// `cl::TokenizeGNUCommandLine`.
pub(crate) fn tokenize_gnu(src: &str) -> Vec<String> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);

    let mut out = Vec::new();
    let mut token = String::new();
    let mut chars = src.chars();

    while let Some(c) = chars.next() {
        match c {
            '\\' => token.push(chars.next().unwrap_or('\\')),
            '"' | '\'' => loop {
                match chars.next() {
                    None => break,
                    Some(q) if q == c => break,
                    Some('\\') => token.push(chars.next().unwrap_or('\\')),
                    Some(o) => token.push(o),
                }
            },
            ' ' | '\t' | '\r' | '\n' => {
                if !token.is_empty() {
                    out.push(std::mem::take(&mut token));
                }
            }
            other => token.push(other),
        }
    }

    if !token.is_empty() {
        out.push(token);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::tokenize_gnu;

    #[test]
    fn tokenize_gnu_follows_llvm_gnu_quoting() {
        let cases: &[(&str, &[&str])] = &[
            ("", &[]),
            ("''", &[]),
            ("\"\"", &[]),
            (r#""foo" "bar""#, &["foo", "bar"]),
            (r#""foo\"" "\"b\"ar""#, &["foo\"", "\"b\"ar"]),
            ("   foo  bar      ", &["foo", "bar"]),
            ("'foo''bar'", &["foobar"]),
            ("'foo' 'bar' baz", &["foo", "bar", "baz"]),
            ("foo\nbar", &["foo", "bar"]),
            ("foo\tbar\r\nbaz", &["foo", "bar", "baz"]),
            (r#"'foo' "bar" baz"#, &["foo", "bar", "baz"]),
            ("'foo bar'", &["foo bar"]),
            ("'foo \"  bar'", &["foo \"  bar"]),
            (r#"foo"bar baz""#, &["foobar baz"]),
            (r"a\ b", &["a b"]),
            (r"'it\'s'", &["it's"]),
            (r"foo\", &[r"foo\"]),
            ("'foo", &["foo"]),
            ("foo\"", &["foo"]),
            ("\u{feff}foo", &["foo"]),
            ("/t/a.o\n/t\"\\/b.\"o", &["/t/a.o", "/t/b.o"]),
            (r"\foo\bar", &["foobar"]),
            ("a\u{a0}b", &["a\u{a0}b"]),
        ];

        for (input, expected) in cases {
            assert_eq!(tokenize_gnu(input), *expected, "input: {input:?}");
        }
    }
}
