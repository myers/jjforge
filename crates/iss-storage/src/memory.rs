//! Persistent memories on the `issues` bookmark — spec v2.2 §10.
//!
//! Memories are short declarative facts (operational rules, codebase
//! folklore, architectural decisions) keyed by a kebab-case slug. They
//! ride the `issues` bookmark via `iss push` / `iss pull` like the
//! per-issue records do, surfacing at session start via
//! `iss show roadmap --include-memories`.
//!
//! ## On-disk layout
//!
//! - File family: `memories/<key>.json`. One file per memory.
//! - Schema: `{ "key", "value", "created_at", "updated_at" }`. See
//!   [`crate::Memory`].
//!
//! ## Op trailers
//!
//! Memory mutations land as single-op commits on the `issues`
//! bookmark with one of two trailer shapes:
//!
//! ```text
//! Jjf-Op: set-memory
//! Jjf-At: <rfc3339-nano>
//! Jjf-Memory-Key: <kebab-slug>
//! Jjf-Memory-Value: <single-line value>
//! ```
//!
//! ```text
//! Jjf-Op: unset-memory
//! Jjf-At: <rfc3339-nano>
//! Jjf-Memory-Key: <kebab-slug>
//! ```
//!
//! These stanzas don't carry `Jjf-Issue:`, so the per-issue trailer
//! parser drops them silently (no matching issue id). The memory
//! trailers and the per-issue trailers coexist on the same bookmark
//! without interfering.
//!
//! ## Slugification
//!
//! [`slugify`] mirrors beads' shape: lowercase, non-alphanumeric → `-`,
//! first ~8 hyphen-separated tokens, capped at 60 chars. Used when the
//! operator runs `iss remember "<insight>"` without `--key` — the value
//! gets slugified to derive the key.

/// Maximum memory-value length the inline trailer can carry without
/// risking a folded git-trailer continuation. Long values are still
/// stored in full in the on-disk JSON file; only the inline
/// `Jjf-Memory-Value:` trailer is truncated. Picked at 200 chars to
/// match git's recommended trailer-value brevity convention.
const TRAILER_VALUE_TRUNC: usize = 200;

/// Convert a free-text insight to a kebab-case slug suitable for use as
/// a memory key. Port of beads' `slugify()` from
/// `reference/beads/cmd/bd/memory.go:23-44`.
///
/// Rules:
/// - Lowercase.
/// - Non-alphanumeric runs collapse to a single `-`.
/// - Strip leading/trailing `-`.
/// - Take only the first 8 hyphen-separated tokens.
/// - Cap total length at 60 chars; trim any trailing `-` left after
///   truncation.
///
/// Returns an empty string if the input contains no alphanumerics
/// (caller's job to surface "could not auto-slug, pass --key" in that
/// case).
pub fn slugify(s: &str) -> String {
    let lower = s.to_lowercase();
    // Replace non-alphanumeric runs with a single `-`.
    let mut out = String::with_capacity(lower.len());
    let mut in_run = false;
    for c in lower.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('-');
            in_run = true;
        }
    }
    // Trim leading/trailing hyphens.
    let trimmed: &str = out.trim_matches('-');
    // First 8 hyphen-separated tokens.
    let parts: Vec<&str> = trimmed.split('-').take(8).collect();
    let mut slug = parts.join("-");
    if slug.len() > 60 {
        slug.truncate(60);
        // Don't end on a hyphen post-truncation.
        while slug.ends_with('-') {
            slug.pop();
        }
    }
    slug
}

/// Render a `set-memory` trailer stanza. The value is single-lined
/// (newlines replaced with spaces) and truncated to
/// [`TRAILER_VALUE_TRUNC`] chars — the on-disk JSON file holds the
/// untouched full value.
pub(crate) fn build_set_memory_commit_message(
    summary: &str,
    key: &str,
    value: &str,
    jjf_at: &str,
) -> String {
    let mut s = String::new();
    s.push_str(summary);
    s.push_str("\n\n");
    s.push_str("Jjf-Op: set-memory\n");
    s.push_str("Jjf-At: ");
    s.push_str(jjf_at);
    s.push('\n');
    s.push_str("Jjf-Memory-Key: ");
    s.push_str(key);
    s.push('\n');
    s.push_str("Jjf-Memory-Value: ");
    // Collapse every newline shape — `\r\n`, `\r`, `\n` — into a
    // single space so no embedded line break can split this trailer
    // value into a separate trailer line (`qa-trailer-injection`,
    // issue `a902492`). Order matters: `\r\n` first, then bare `\r`,
    // then bare `\n`. The on-disk JSON file still holds the
    // untouched value.
    let one_line = value
        .replace("\r\n", " ")
        .replace('\r', " ")
        .replace('\n', " ");
    let truncated: String = if one_line.chars().count() > TRAILER_VALUE_TRUNC {
        let mut t: String = one_line.chars().take(TRAILER_VALUE_TRUNC).collect();
        t.push_str("...");
        t
    } else {
        one_line
    };
    s.push_str(&truncated);
    s.push('\n');
    s
}

/// Render an `unset-memory` trailer stanza.
pub(crate) fn build_unset_memory_commit_message(
    summary: &str,
    key: &str,
    jjf_at: &str,
) -> String {
    let mut s = String::new();
    s.push_str(summary);
    s.push_str("\n\n");
    s.push_str("Jjf-Op: unset-memory\n");
    s.push_str("Jjf-At: ");
    s.push_str(jjf_at);
    s.push('\n');
    s.push_str("Jjf-Memory-Key: ");
    s.push_str(key);
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(
            slugify("always run tests with -race flag"),
            "always-run-tests-with-race-flag"
        );
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("!!!"), "");
    }

    #[test]
    fn slugify_caps_eight_tokens() {
        assert_eq!(
            slugify("one two three four five six seven eight nine ten"),
            "one-two-three-four-five-six-seven-eight"
        );
    }

    #[test]
    fn slugify_caps_60_chars() {
        // 70-char input collapses to a single hyphen-joined string,
        // capped at 60 with no trailing hyphen.
        let s = "x".repeat(70);
        let slug = slugify(&s);
        assert!(slug.len() <= 60, "got len={}, slug={}", slug.len(), slug);
        assert!(!slug.ends_with('-'));
    }

    #[test]
    fn slugify_caps_60_chars_with_hyphens() {
        // 9 tokens of 8 chars each → first 8 tokens = 71 chars, capped
        // at 60. Make sure the cap doesn't leave a trailing hyphen.
        // "aaaaaaaa-bbbbbbbb-cccccccc-dddddddd-eeeeeeee-ffffffff-gggggggg-hhhhhhhh"
        let s = "aaaaaaaa bbbbbbbb cccccccc dddddddd eeeeeeee ffffffff gggggggg hhhhhhhh";
        let slug = slugify(s);
        assert!(slug.len() <= 60, "got len={}, slug={}", slug.len(), slug);
        assert!(!slug.ends_with('-'));
    }

    #[test]
    fn set_memory_trailer_shape() {
        let msg = build_set_memory_commit_message(
            "iss: memory dolt-phantoms - set",
            "dolt-phantoms",
            "Dolt phantom DBs hide in three places",
            "2026-06-22T12:34:56.123456789Z",
        );
        let expected = "\
iss: memory dolt-phantoms - set

Jjf-Op: set-memory
Jjf-At: 2026-06-22T12:34:56.123456789Z
Jjf-Memory-Key: dolt-phantoms
Jjf-Memory-Value: Dolt phantom DBs hide in three places
";
        assert_eq!(msg, expected);
    }

    #[test]
    fn unset_memory_trailer_shape() {
        let msg = build_unset_memory_commit_message(
            "iss: memory dolt-phantoms - unset",
            "dolt-phantoms",
            "2026-06-22T12:34:56.123456789Z",
        );
        let expected = "\
iss: memory dolt-phantoms - unset

Jjf-Op: unset-memory
Jjf-At: 2026-06-22T12:34:56.123456789Z
Jjf-Memory-Key: dolt-phantoms
";
        assert_eq!(msg, expected);
    }

    #[test]
    fn set_memory_trailer_one_lines_value() {
        let msg = build_set_memory_commit_message(
            "iss: memory k - set",
            "k",
            "line1\nline2\nline3",
            "2026-06-22T12:34:56.123456789Z",
        );
        assert!(msg.contains("Jjf-Memory-Value: line1 line2 line3\n"));
    }

    #[test]
    fn set_memory_trailer_truncates_long_value() {
        let big = "a".repeat(300);
        let msg = build_set_memory_commit_message(
            "iss: memory k - set",
            "k",
            &big,
            "2026-06-22T12:34:56.123456789Z",
        );
        assert!(msg.contains("aaa..."));
        // The truncation cap: <=200 + 3 dots.
        let trailer_line = msg
            .lines()
            .find(|l| l.starts_with("Jjf-Memory-Value:"))
            .unwrap();
        // strip "Jjf-Memory-Value: " prefix.
        let val = trailer_line.trim_start_matches("Jjf-Memory-Value: ");
        assert!(val.ends_with("..."));
        // 200 a's + "..." == 203
        assert_eq!(val.len(), 203);
    }
}
