//! Small helpers shared by the commands that print text.

use std::fmt::Write as _;

/// Appends `text` and a newline.
pub(crate) fn line(out: &mut String, text: &str) {
    writeln!(out, "{text}").expect("writing to a String can't fail");
}

/// `1 test`, `2 tests`.
pub(crate) fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}
