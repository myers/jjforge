//! `Jjf-Op:` operation vocabulary and trailer serialization.
//!
//! Each variant maps to one row of `docs/storage-format.md` §5.2 and
//! renders to a stanza like:
//!
//! ```text
//! Jjf-Op: set-status
//! Jjf-Bug: aa6600b
//! Jjf-Status: closed
//! ```
//!
//! The `merge` op is included so the trailer round-trips cleanly when
//! a reader encounters one — the merge driver crate is what actually
//! writes them.

use serde::{Deserialize, Serialize};

use crate::id::BugId;
use crate::record::Status;

/// The op vocabulary per spec §5.2.
///
/// `serde` is derived so the payload round-trips for callers that want
/// JSON-shaped op records (e.g. tests, the upcoming read path that
/// reconstructs the typed audit chain).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Op {
    Create {
        bug_id: BugId,
        title: String,
        status: Status,
    },
    SetTitle {
        bug_id: BugId,
        title: String,
    },
    SetStatus {
        bug_id: BugId,
        status: Status,
    },
    SetBody {
        bug_id: BugId,
        body_hash: String,
    },
    LabelAdd {
        bug_id: BugId,
        label: String,
    },
    LabelRm {
        bug_id: BugId,
        label: String,
    },
    DepAdd {
        bug_id: BugId,
        dep: BugId,
    },
    DepRm {
        bug_id: BugId,
        dep: BugId,
    },
    SetAssignee {
        bug_id: BugId,
        assignee: Option<String>,
    },
    CommentAdd {
        bug_id: BugId,
        comment_id: BugId,
    },
    Merge {
        bug_id: BugId,
    },
}

impl Op {
    /// The op-type slug (the value after `Jjf-Op:`).
    pub fn op_type(&self) -> &'static str {
        match self {
            Op::Create { .. } => "create",
            Op::SetTitle { .. } => "set-title",
            Op::SetStatus { .. } => "set-status",
            Op::SetBody { .. } => "set-body",
            Op::LabelAdd { .. } => "label-add",
            Op::LabelRm { .. } => "label-rm",
            Op::DepAdd { .. } => "dep-add",
            Op::DepRm { .. } => "dep-rm",
            Op::SetAssignee { .. } => "set-assignee",
            Op::CommentAdd { .. } => "comment-add",
            Op::Merge { .. } => "merge",
        }
    }

    /// The `Jjf-Bug:` value (every op carries one — spec §5.1).
    pub fn bug_id(&self) -> &BugId {
        match self {
            Op::Create { bug_id, .. }
            | Op::SetTitle { bug_id, .. }
            | Op::SetStatus { bug_id, .. }
            | Op::SetBody { bug_id, .. }
            | Op::LabelAdd { bug_id, .. }
            | Op::LabelRm { bug_id, .. }
            | Op::DepAdd { bug_id, .. }
            | Op::DepRm { bug_id, .. }
            | Op::SetAssignee { bug_id, .. }
            | Op::CommentAdd { bug_id, .. }
            | Op::Merge { bug_id } => bug_id,
        }
    }

    /// Render one op stanza: the `Jjf-Op:` line, the `Jjf-Bug:` line,
    /// the `Jjf-At:` line, and any op-specific payload trailers, each
    /// terminated with `\n`.
    ///
    /// `jjf_at` is the RFC-3339-nano timestamp the writer stamps at the
    /// moment of the op. See spec §5 (`Jjf-At` is required on every
    /// stanza this writer emits; parsers tolerate absence for forward
    /// compatibility with older fixtures and pre-spec-bump data).
    pub fn to_trailer_block(&self, jjf_at: &str) -> String {
        let mut s = String::new();
        s.push_str("Jjf-Op: ");
        s.push_str(self.op_type());
        s.push('\n');
        s.push_str("Jjf-Bug: ");
        s.push_str(self.bug_id().as_str());
        s.push('\n');
        s.push_str("Jjf-At: ");
        s.push_str(jjf_at);
        s.push('\n');
        match self {
            Op::Create { title, status, .. } => {
                s.push_str("Jjf-Title: ");
                s.push_str(title);
                s.push('\n');
                s.push_str("Jjf-Status: ");
                s.push_str(status.as_str());
                s.push('\n');
            }
            Op::SetTitle { title, .. } => {
                s.push_str("Jjf-Title: ");
                s.push_str(title);
                s.push('\n');
            }
            Op::SetStatus { status, .. } => {
                s.push_str("Jjf-Status: ");
                s.push_str(status.as_str());
                s.push('\n');
            }
            Op::SetBody { body_hash, .. } => {
                s.push_str("Jjf-Body-Hash: ");
                s.push_str(body_hash);
                s.push('\n');
            }
            Op::LabelAdd { label, .. } | Op::LabelRm { label, .. } => {
                s.push_str("Jjf-Label: ");
                s.push_str(label);
                s.push('\n');
            }
            Op::DepAdd { dep, .. } | Op::DepRm { dep, .. } => {
                s.push_str("Jjf-Dep: ");
                s.push_str(dep.as_str());
                s.push('\n');
            }
            Op::SetAssignee { assignee, .. } => {
                s.push_str("Jjf-Assignee: ");
                s.push_str(assignee.as_deref().unwrap_or(""));
                s.push('\n');
            }
            Op::CommentAdd { comment_id, .. } => {
                s.push_str("Jjf-Comment-Id: ");
                s.push_str(comment_id.as_str());
                s.push('\n');
            }
            Op::Merge { .. } => {
                // No extra payload per spec §5.2.
            }
        }
        s
    }
}
