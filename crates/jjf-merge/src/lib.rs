//! `jjf-merge` — parse jj's content-conflict markers and resolve a
//! conflicted `bugs/<id>.json` file per the jjforge v1 merge policy.
//!
//! # Scope (v1, per ticket `e2e473b`)
//!
//! - Parse jj's textual conflict format: `<<<<<<<` / `+++++++` /
//!   `%%%%%%%` / `\\\\\\\` / `>>>>>>>`. Side A is the literal block
//!   under `+++++++`; side B is reconstructed by walking the unified
//!   diff hunk against the base (the lines under `%%%%%%%` …
//!   `\\\\\\\`).
//! - Apply per-field merge policy on the resolved JSON object:
//!   - Scalar fields (title, status, assignee, …): last-write-wins,
//!     with a deterministic tiebreaker driven by an explicit
//!     `prefer_side` hint. Author-timestamp ordering is a later
//!     ticket.
//!   - Arrays (labels, dependencies): set-union, deterministic sort.
//! - Comments live in a separate `bugs/<id>.comments.jsonl` file per
//!   `docs/storage-format.md` §4. Merging that file is **out of scope
//!   for v1** and tracked separately. This crate is happy to merge a
//!   record whose schema does not contain a `comments` field, which
//!   is the v1 storage shape.
//!
//! # Reference implementation
//!
//! `experiments/distributed-edit/test-followup-distance-and-recovery.sh`
//! contains a 30-line Python parser. This crate copies its parsing
//! decisions; it does not invent new constraints.

pub mod merge;
pub mod parser;

use serde_json::Value;

pub use merge::{MergeOptions, MergePolicy, Side};
pub use parser::{ConflictBlock, ParseError, parse_conflicts};

/// What went wrong end-to-end.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("parse error: {0}")]
    Parse(#[from] ParseError),
    #[error("merge produced invalid JSON on side {side:?}: {source}")]
    InvalidJsonSide {
        side: Side,
        #[source]
        source: serde_json::Error,
    },
    #[error("merge produced unmergeable shapes: {0}")]
    Unmergeable(String),
    #[error("serializing resolved record: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Resolve a conflicted `bugs/<id>.json` file end-to-end.
///
/// `text` is the raw file contents (jj conflict markers included).
/// Returns the resolved JSON, serialized pretty-printed with a
/// trailing newline to match the v1 storage-format writer rule.
///
/// If `text` contains no conflict markers, it is parsed as JSON and
/// re-serialized in canonical form (a cheap idempotency win).
pub fn resolve(text: &str, opts: &MergeOptions) -> Result<String, Error> {
    let blocks = parse_conflicts(text)?;
    if blocks.is_empty() {
        let v: Value = serde_json::from_str(text)?;
        return Ok(canonicalize(&v));
    }
    // For a `bugs/<id>.json` record we expect one block covering the
    // whole file (the file is one JSON object). If there is more than
    // one we still attempt — each block is resolved independently and
    // the chosen side reassembled with surrounding context.
    //
    // For v1 we accept the simpler shape: exactly one block, no
    // surrounding context. That mirrors the Python prototype and the
    // captured fixtures. A multi-block file would be a v2 concern.
    if blocks.len() > 1 {
        return Err(Error::Unmergeable(format!(
            "expected exactly one conflict block in bugs/<id>.json, found {}",
            blocks.len()
        )));
    }
    let block = &blocks[0];
    let side_a: Value =
        serde_json::from_str(block.side_a.trim_end_matches('\n')).map_err(|e| {
            Error::InvalidJsonSide {
                side: Side::A,
                source: e,
            }
        })?;
    let side_b: Value =
        serde_json::from_str(block.side_b.trim_end_matches('\n')).map_err(|e| {
            Error::InvalidJsonSide {
                side: Side::B,
                source: e,
            }
        })?;
    let merged = merge::merge_values(&side_a, &side_b, opts)?;
    Ok(canonicalize(&merged))
}

/// Pretty-print JSON with 2-space indentation and a trailing newline,
/// per `docs/storage-format.md` §3.
fn canonicalize(v: &Value) -> String {
    let mut out = serde_json::to_string_pretty(v).expect("pretty-print Value");
    out.push('\n');
    out
}

#[cfg(test)]
mod smoke_tests {
    use super::*;
    use serde_json::json;

    /// End-to-end: the canonical fixture from
    /// `experiments/distributed-edit/runs/followup.transcript.txt`
    /// scenario B — concurrent set-title from alice and bob.
    #[test]
    fn resolves_concurrent_set_title() {
        let text = include_str!("../tests/fixtures/concurrent_title.conflict");
        let opts = MergeOptions {
            prefer_side: Side::B,
            ..Default::default()
        };
        let resolved = resolve(text, &opts).unwrap();
        let v: Value = serde_json::from_str(&resolved).unwrap();
        assert_eq!(v["title"], json!("bob title"));
        assert_eq!(v["status"], json!("open"));
    }

    #[test]
    fn no_conflict_passes_through_canonical() {
        let resolved = resolve(
            "{\"title\":\"x\",\"status\":\"open\"}\n",
            &MergeOptions::default(),
        )
        .unwrap();
        // canonicalized = pretty + trailing newline
        assert!(resolved.ends_with('\n'));
        let v: Value = serde_json::from_str(&resolved).unwrap();
        assert_eq!(v["title"], json!("x"));
    }
}
