//! mIRC control-code tokenizer.
//!
//! IRC clients have always smuggled formatting into message text with control
//! bytes. The frontend must never see raw control characters (and must never
//! be tempted to `innerHTML` a message), so the backend turns a message into a
//! list of styled [`Segment`]s and the UI only maps those to DOM nodes.
//!
//! Recognized codes:
//! - `\x02` bold, `\x1d` italic, `\x1f` underline, `\x16` reverse (all toggles)
//! - `\x0f` reset
//! - `\x03` colour: optional 1-2 digit foreground, optional `,` and 1-2 digit
//!   background; a bare `\x03` clears colour.

use serde::Serialize;

/// A run of text sharing one style. `color`/`bg` are mIRC palette indices
/// (0-15); the UI owns the index-to-colour table.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Segment {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub color: Option<u8>,
    pub bg: Option<u8>,
}

impl Segment {
    /// Whether this run carries no styling at all. Used by the tests and the
    /// frontend's plain-text fast path.
    #[allow(dead_code)]
    pub fn is_plain(&self) -> bool {
        !self.bold
            && !self.italic
            && !self.underline
            && !self.reverse
            && self.color.is_none()
            && self.bg.is_none()
    }
}

/// Tokenize `text` into styled segments. Adjacent control codes with no text
/// between them do not produce empty segments.
pub fn parse(text: &str) -> Vec<Segment> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Segment> = Vec::new();
    let mut cur = Segment::default();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\u{2}' => {
                flush(&mut out, &mut cur);
                cur.bold = !cur.bold;
                i += 1;
            }
            '\u{1d}' => {
                flush(&mut out, &mut cur);
                cur.italic = !cur.italic;
                i += 1;
            }
            '\u{1f}' => {
                flush(&mut out, &mut cur);
                cur.underline = !cur.underline;
                i += 1;
            }
            '\u{16}' => {
                flush(&mut out, &mut cur);
                cur.reverse = !cur.reverse;
                i += 1;
            }
            '\u{f}' => {
                flush(&mut out, &mut cur);
                cur.bold = false;
                cur.italic = false;
                cur.underline = false;
                cur.reverse = false;
                cur.color = None;
                cur.bg = None;
                i += 1;
            }
            '\u{3}' => {
                flush(&mut out, &mut cur);
                i += 1;
                let (fg, next) = take_num(&chars, i);
                match fg {
                    Some(f) => {
                        i = next;
                        cur.color = Some(f);
                        if chars.get(i) == Some(&',') {
                            let (bg, next_bg) = take_num(&chars, i + 1);
                            if let Some(b) = bg {
                                i = next_bg;
                                cur.bg = Some(b);
                            }
                        }
                    }
                    None => {
                        // A bare colour code is a reset, not an empty colour.
                        cur.color = None;
                        cur.bg = None;
                    }
                }
            }
            c => {
                cur.text.push(c);
                i += 1;
            }
        }
    }
    flush(&mut out, &mut cur);
    out
}

/// The visible text with every control code removed. Used for length checks,
/// nick highlighting, and anything that must not count formatting bytes.
pub fn plain(text: &str) -> String {
    parse(text).into_iter().map(|s| s.text).collect()
}

/// Read a colour number at `i`: two digits when the pair is a valid colour
/// (<= 15), otherwise a single digit so the leftover digit stays as text.
fn take_num(chars: &[char], i: usize) -> (Option<u8>, usize) {
    let d1 = chars.get(i).copied().filter(char::is_ascii_digit);
    let d2 = chars.get(i + 1).copied().filter(char::is_ascii_digit);
    match (d1, d2) {
        (Some(a), Some(b)) => {
            let two = (a as u8 - b'0') * 10 + (b as u8 - b'0');
            if two <= 15 {
                (Some(two), i + 2)
            } else {
                (Some(a as u8 - b'0'), i + 1)
            }
        }
        (Some(a), None) => (Some(a as u8 - b'0'), i + 1),
        _ => (None, i),
    }
}

fn flush(out: &mut Vec<Segment>, cur: &mut Segment) {
    if cur.text.is_empty() {
        return;
    }
    out.push(cur.clone());
    cur.text.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_one_unstyled_segment() {
        let segs = parse("hello world");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "hello world");
        assert!(segs[0].is_plain());
    }

    #[test]
    fn bold_underline_italic_reverse_toggle() {
        let segs = parse("a\u{2}b\u{2}c\u{1f}d\u{1f}e\u{1d}f\u{1d}g\u{16}h\u{16}i");
        let texts: Vec<_> = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["a", "b", "c", "d", "e", "f", "g", "h", "i"]);
        assert!(segs[1].bold && !segs[2].bold);
        assert!(segs[3].underline && !segs[4].underline);
        assert!(segs[5].italic && !segs[6].italic);
        assert!(segs[7].reverse && !segs[8].reverse);
    }

    #[test]
    fn reset_clears_every_style() {
        let segs = parse("\u{2}\u{1f}\u{3}4red\u{f}normal");
        assert_eq!(segs[0].text, "red");
        assert!(segs[0].bold && segs[0].underline);
        assert_eq!(segs[0].color, Some(4));
        assert!(segs[1].is_plain());
        assert_eq!(segs[1].text, "normal");
    }

    #[test]
    fn colour_foreground_only() {
        let segs = parse("\u{3}4red");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].color, Some(4));
        assert_eq!(segs[0].bg, None);
        assert_eq!(segs[0].text, "red");
    }

    #[test]
    fn colour_foreground_and_background() {
        let segs = parse("\u{3}4,8both");
        assert_eq!(segs[0].color, Some(4));
        assert_eq!(segs[0].bg, Some(8));
        assert_eq!(segs[0].text, "both");
    }

    #[test]
    fn two_digit_colour() {
        let segs = parse("\u{3}12light");
        assert_eq!(segs[0].color, Some(12));
        assert_eq!(segs[0].text, "light");
    }

    #[test]
    fn out_of_range_pair_falls_back_to_first_digit() {
        // `99` is not a colour, so it is colour 9 followed by the literal "9".
        let segs = parse("\u{3}99x");
        assert_eq!(segs[0].color, Some(9));
        assert_eq!(segs[0].text, "9x");
    }

    #[test]
    fn bare_colour_code_resets_colour() {
        let segs = parse("\u{3}4red\u{3}plain");
        assert_eq!(segs[0].color, Some(4));
        assert_eq!(segs[1].color, None);
        assert_eq!(segs[1].text, "plain");
    }

    #[test]
    fn plain_strips_all_codes() {
        assert_eq!(plain("\u{2}bo\u{2}ld \u{3}4c\u{f}"), "bold c");
    }

    #[test]
    fn control_codes_without_text_make_no_empty_segments() {
        let segs = parse("\u{2}\u{2}\u{2}text");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].text, "text");
        assert!(segs[0].bold);
    }
}
