//! Display-width aware string helpers.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate to at most `max` columns, ending in `…` when cut.
pub fn fit(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let w = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + w > max - 1 {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Fit to exactly `w` columns, padding on the right.
pub fn pad_right(s: &str, w: usize) -> String {
    let mut out = fit(s, w);
    let used = width(&out);
    out.push_str(&" ".repeat(w.saturating_sub(used)));
    out
}

/// Fit to exactly `w` columns, padding on the left.
pub fn pad_left(s: &str, w: usize) -> String {
    let fitted = fit(s, w);
    let used = width(&fitted);
    let mut out = " ".repeat(w.saturating_sub(used));
    out.push_str(&fitted);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_and_pads_by_display_width() {
        assert_eq!(fit("hello", 10), "hello");
        assert_eq!(fit("hello world", 6), "hello…");
        assert_eq!(fit("hello", 0), "");
        assert_eq!(fit("hello", 1), "…");
        assert_eq!(pad_right("ab", 4), "ab  ");
        assert_eq!(pad_left("ab", 4), "  ab");
        assert_eq!(pad_right("abcdef", 4), "abc…");
        assert_eq!(width(&pad_left("✓ done", 9)), 9);
        // Wide characters never overflow the budget.
        assert!(width(&fit("日本語テキスト", 5)) <= 5);
    }
}
