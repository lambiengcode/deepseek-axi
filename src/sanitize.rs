//! Strips ANSI/C0 control sequences from model-generated or file-echoed text
//! before it reaches the terminal — a faithful port of `sanitize.js`, using
//! `memchr` to fast-forward to each ESC byte instead of a naive byte-by-byte
//! scan (§1 SIMD of RUST_MIGRATION_PLAN.md).

use memchr::memchr;

pub fn sanitize_for_terminal(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match memchr(0x1B, &bytes[i..]) {
            Some(offset) => {
                let esc_pos = i + offset;
                copy_stripped(&bytes[i..esc_pos], &mut out);
                i = skip_escape_sequence(bytes, esc_pos);
            }
            None => {
                copy_stripped(&bytes[i..], &mut out);
                break;
            }
        }
    }
    // Safe: every byte we ever drop is < 0x80 (ASCII control range), and no
    // byte of a valid multi-byte UTF-8 sequence is ever < 0x80, so this
    // never splits a multi-byte character.
    String::from_utf8(out).unwrap_or_default()
}

fn is_other_control(b: u8) -> bool {
    (0x00..=0x08).contains(&b) || b == 0x0B || b == 0x0C || (0x0E..=0x1F).contains(&b) || b == 0x7F
}

fn copy_stripped(chunk: &[u8], out: &mut Vec<u8>) {
    for &b in chunk {
        if !is_other_control(b) {
            out.push(b);
        }
    }
}

/// Mirrors the JS ANSI_ESCAPE regex: `\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])`.
fn skip_escape_sequence(bytes: &[u8], esc_pos: usize) -> usize {
    if esc_pos + 1 < bytes.len() && bytes[esc_pos + 1] == b'[' {
        let mut j = esc_pos + 2;
        while j < bytes.len() && (0x30..=0x3F).contains(&bytes[j]) {
            j += 1;
        }
        while j < bytes.len() && (0x20..=0x2F).contains(&bytes[j]) {
            j += 1;
        }
        if j < bytes.len() && (0x40..=0x7E).contains(&bytes[j]) {
            return j + 1;
        }
        return esc_pos + 1;
    }
    if esc_pos + 1 < bytes.len() {
        let next = bytes[esc_pos + 1];
        let is_two_char = (0x40..=0x5A).contains(&next) || next == 0x5C || next == 0x5F;
        if is_two_char {
            return esc_pos + 2;
        }
    }
    esc_pos + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_csi_escape_sequences() {
        assert_eq!(sanitize_for_terminal("\x1B[31mred\x1B[0m text"), "red text");
    }

    #[test]
    fn strips_a_bare_c0_control_character() {
        assert_eq!(sanitize_for_terminal("a\x07b"), "ab");
    }

    #[test]
    fn keeps_newlines_and_tabs() {
        assert_eq!(sanitize_for_terminal("line1\n\tindented"), "line1\n\tindented");
    }

    #[test]
    fn passes_through_plain_text_unchanged() {
        assert_eq!(sanitize_for_terminal("hello world"), "hello world");
    }

    #[test]
    fn passes_through_multibyte_utf8_unchanged() {
        assert_eq!(sanitize_for_terminal("héllo 世界"), "héllo 世界");
    }

    #[test]
    fn strips_bell_and_esc_together_like_the_model_injection_test() {
        // Same case verified live against the real API in the Node build's
        // terminal-injection fix.
        assert_eq!(sanitize_for_terminal("\x1B[31mRED\x1B[0m\x07DONE"), "REDDONE");
    }
}
