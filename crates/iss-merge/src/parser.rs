//! Parser for jj's textual content-conflict markers.
//!
//! Format (jj 0.40; reference: the Python prototype in
//! `experiments/distributed-edit/test-followup-distance-and-recovery.sh`):
//!
//! ```text
//! <<<<<<< conflict N of M
//! +++++++ <change_id_short> <commit_id_short> "<desc>"
//! <side-A literal lines>
//! %%%%%%% diff from: <base_change_id> <base_commit_id> "<base desc>"
//! \\\\\\\        to: <side-B id> <side-B commit_id> "<side-B desc>"
//! <unified-diff hunk for side B vs base: ' ', '-', '+' prefixes>
//! >>>>>>> conflict N of M ends
//! ```
//!
//! Side A is the literal block after `+++++++`. Side B is
//! reconstructed by walking the diff hunk: lines prefixed with `' '`
//! and `'+'` go into side B; `'-'` lines were in the base only.

use std::fmt::Write;

/// One parsed `<<<<<<< … >>>>>>>` block.
///
/// Trailing newlines on `side_a` / `side_b` are preserved as the
/// parser found them, so a JSON record that ends with `\n` round-
/// trips byte-for-byte after re-emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictBlock {
    pub side_a: String,
    pub side_b: String,
    pub base: String,
    /// Bytes before the block in the source file (preserved verbatim).
    pub prefix: String,
    /// Bytes after the block (preserved verbatim).
    pub suffix: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("conflict block opened but never closed (looking for `>>>>>>> conflict … ends`)")]
    UnclosedBlock,
    #[error("conflict block missing `%%%%%%%` base-diff separator")]
    MissingBaseSeparator,
    #[error("conflict block missing `\\\\\\\\` side-B header line")]
    MissingSideBHeader,
    #[error("conflict block missing `+++++++` side-A header line")]
    MissingSideAHeader,
    #[error("unexpected diff prefix {0:?} (expected ' ', '+', or '-')")]
    BadDiffPrefix(char),
}

/// Parse all conflict blocks in `text`.
///
/// A block with no markers returns an empty Vec — the caller is
/// expected to treat that as "no conflicts to resolve".
pub fn parse_conflicts(text: &str) -> Result<Vec<ConflictBlock>, ParseError> {
    let mut blocks = Vec::new();
    let mut cursor = 0usize;

    while let Some(start_rel) = text[cursor..].find("<<<<<<< conflict ") {
        let start = cursor + start_rel;
        let prefix = text[cursor..start].to_string();

        // Find the end marker. We accept either `… ends\n` or just `… ends` at EOF.
        let after_start = &text[start..];
        let end_marker_rel = after_start
            .find(">>>>>>> conflict ")
            .ok_or(ParseError::UnclosedBlock)?;
        // Advance to the end of the line that starts at end_marker_rel.
        let line_end_rel = after_start[end_marker_rel..]
            .find('\n')
            .map(|n| end_marker_rel + n + 1)
            .unwrap_or(after_start.len());
        let block_text = &after_start[..line_end_rel];

        let parsed = parse_block(block_text)?;
        let suffix_start = start + line_end_rel;

        blocks.push(ConflictBlock {
            side_a: parsed.side_a,
            side_b: parsed.side_b,
            base: parsed.base,
            prefix,
            suffix: String::new(), // filled in below for the last block
        });

        cursor = suffix_start;
    }

    // Capture the trailing bytes after the last block as the final
    // block's suffix. (Earlier blocks' suffix stays empty; only the
    // last block carries the file tail. For v1 single-block files
    // this is fine.)
    if let Some(last) = blocks.last_mut() {
        last.suffix = text[cursor..].to_string();
    }

    Ok(blocks)
}

struct Parsed {
    side_a: String,
    side_b: String,
    base: String,
}

fn parse_block(block: &str) -> Result<Parsed, ParseError> {
    // Drop the first line (`<<<<<<< conflict N of M`) and the last
    // line (`>>>>>>> conflict N of M ends`).
    let mut lines = block.lines();
    let _open = lines.next().ok_or(ParseError::UnclosedBlock)?;

    // Collect everything up to (but not including) the close marker.
    let mut inner: Vec<&str> = Vec::new();
    for line in lines {
        if line.starts_with(">>>>>>> conflict ") {
            break;
        }
        inner.push(line);
    }

    // Walk inner looking for `+++++++`, `%%%%%%%`, `\\\\\\\` markers.
    let mut i = 0usize;
    if i >= inner.len() || !inner[i].starts_with("+++++++ ") {
        return Err(ParseError::MissingSideAHeader);
    }
    i += 1;

    // Side A literal lines until `%%%%%%%`.
    let side_a_start = i;
    while i < inner.len() && !inner[i].starts_with("%%%%%%% ") {
        i += 1;
    }
    if i == inner.len() {
        return Err(ParseError::MissingBaseSeparator);
    }
    let side_a_end = i;
    i += 1; // consume `%%%%%%% diff from: …`

    if i >= inner.len() || !inner[i].starts_with("\\\\\\\\") {
        return Err(ParseError::MissingSideBHeader);
    }
    i += 1; // consume `\\\\\\\        to: …`

    // Unified-diff hunk for side B vs base.
    let diff_start = i;
    let diff_end = inner.len();

    let side_a = join_with_trailing_newline(&inner[side_a_start..side_a_end]);
    let (base, side_b) = reconstruct_from_diff(&inner[diff_start..diff_end])?;

    Ok(Parsed {
        side_a,
        side_b,
        base,
    })
}

/// Apply the unified-diff hunk lines back to base + side-B literals.
fn reconstruct_from_diff(diff: &[&str]) -> Result<(String, String), ParseError> {
    let mut base = String::new();
    let mut side_b = String::new();

    for line in diff {
        // Empty lines inside the hunk would be ambiguous — jj prefixes
        // even empty content lines with a single space. So a truly
        // empty `line` is a no-op (skip).
        if line.is_empty() {
            continue;
        }
        let (prefix, body) = line.split_at(1);
        match prefix {
            " " => {
                writeln_buf(&mut base, body);
                writeln_buf(&mut side_b, body);
            }
            "-" => writeln_buf(&mut base, body),
            "+" => writeln_buf(&mut side_b, body),
            other => {
                return Err(ParseError::BadDiffPrefix(other.chars().next().unwrap_or('?')));
            }
        }
    }

    Ok((base, side_b))
}

fn join_with_trailing_newline(lines: &[&str]) -> String {
    let mut out = String::new();
    for line in lines {
        writeln_buf(&mut out, line);
    }
    out
}

fn writeln_buf(buf: &mut String, line: &str) {
    let _ = writeln!(buf, "{}", line);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the Python prototype's success path on the canonical
    /// fixture — the scenario-B conflict for `bb02.json`.
    #[test]
    fn parses_canonical_set_title_conflict() {
        let text = "\
<<<<<<< conflict 1 of 1
+++++++ rwyxzusv d38c05c6 \"jjf: bug bb02 - set-title alice\"
{\"title\":\"alice title\",\"status\":\"open\",\"comments\":[]}
%%%%%%% diff from: ltxqzulz d66ba4d3 \"jjf: bug bb02 - create\"
\\\\\\\\        to: rtlvvpks 27c53447 \"jjf: bug bb02 - set-title bob\"
-{\"title\":\"first\",\"status\":\"open\",\"comments\":[]}
+{\"title\":\"bob title\",\"status\":\"open\",\"comments\":[]}
>>>>>>> conflict 1 of 1 ends
";
        let blocks = parse_conflicts(text).unwrap();
        assert_eq!(blocks.len(), 1);
        let b = &blocks[0];
        assert_eq!(
            b.side_a,
            "{\"title\":\"alice title\",\"status\":\"open\",\"comments\":[]}\n"
        );
        assert_eq!(
            b.side_b,
            "{\"title\":\"bob title\",\"status\":\"open\",\"comments\":[]}\n"
        );
        assert_eq!(
            b.base,
            "{\"title\":\"first\",\"status\":\"open\",\"comments\":[]}\n"
        );
    }

    #[test]
    fn no_markers_returns_empty() {
        assert!(parse_conflicts("{\"title\":\"x\"}\n").unwrap().is_empty());
    }

    #[test]
    fn errors_on_unclosed() {
        let text = "<<<<<<< conflict 1 of 1\n+++++++ x y \"z\"\nfoo\n";
        let err = parse_conflicts(text).unwrap_err();
        assert!(matches!(err, ParseError::UnclosedBlock));
    }

    /// Multi-line side A (pretty-printed JSON) with a diff that
    /// touches one inner line — the realistic shape after
    /// docs/storage-format.md §3.3 (writers emit pretty-printed JSON).
    #[test]
    fn parses_multiline_pretty_record() {
        let text = "\
<<<<<<< conflict 1 of 1
+++++++ a b \"alice retitles\"
{
  \"title\": \"alice title\",
  \"status\": \"open\"
}
%%%%%%% diff from: c d \"create\"
\\\\\\\\        to: e f \"bob retitles\"
 {
-  \"title\": \"first\",
+  \"title\": \"bob title\",
   \"status\": \"open\"
 }
>>>>>>> conflict 1 of 1 ends
";
        let blocks = parse_conflicts(text).unwrap();
        assert_eq!(blocks.len(), 1);
        let b = &blocks[0];
        assert_eq!(
            b.side_a,
            "{\n  \"title\": \"alice title\",\n  \"status\": \"open\"\n}\n"
        );
        assert_eq!(
            b.side_b,
            "{\n  \"title\": \"bob title\",\n  \"status\": \"open\"\n}\n"
        );
        assert_eq!(
            b.base,
            "{\n  \"title\": \"first\",\n  \"status\": \"open\"\n}\n"
        );
    }

    #[test]
    fn preserves_prefix_and_suffix() {
        let text = "leading\n\
<<<<<<< conflict 1 of 1
+++++++ a b \"x\"
A
%%%%%%% diff from: c d \"x\"
\\\\\\\\        to: e f \"x\"
-base
+B
>>>>>>> conflict 1 of 1 ends
trailing\n";
        let blocks = parse_conflicts(text).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].prefix, "leading\n");
        assert_eq!(blocks[0].suffix, "trailing\n");
    }
}
