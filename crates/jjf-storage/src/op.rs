//! `Jjf-Op:` operation vocabulary and trailer serialization.
//!
//! Each variant maps to one row of `docs/storage-format.md` §5.2 and
//! renders to a stanza like:
//!
//! ```text
//! Jjf-Op: set-status
//! Jjf-Issue: aa6600b
//! Jjf-Status: closed
//! ```
//!
//! The `merge` op is included so the trailer round-trips cleanly when
//! a reader encounters one — the merge driver crate is what actually
//! writes them.
//!
//! ## v1 → v2 forward compatibility
//!
//! v1 trailers used `Jjf-Bug:` for the issue id; v2 emits `Jjf-Issue:`.
//! The parser in [`crate::trailer`] tolerates both spellings on read,
//! so any existing repo data with the v1 trailer continues to op-replay.
//! This module only emits v2 (`Jjf-Issue:`).

use serde::{Deserialize, Serialize};

use crate::id::IssueId;
use crate::record::{DepKind, IssueType, Status};

/// The op vocabulary per spec §5.2.
///
/// `serde` is derived so the payload round-trips for callers that want
/// JSON-shaped op records (e.g. tests, the read path that reconstructs
/// the typed audit chain).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Op {
    Create {
        issue_id: IssueId,
        title: String,
        status: Status,
    },
    SetTitle {
        issue_id: IssueId,
        title: String,
    },
    SetStatus {
        issue_id: IssueId,
        status: Status,
    },
    SetBody {
        issue_id: IssueId,
        body_hash: String,
    },
    LabelAdd {
        issue_id: IssueId,
        label: String,
    },
    LabelRm {
        issue_id: IssueId,
        label: String,
    },
    /// Add a typed dependency edge. v2.4 (`agent-dep-types`) extended
    /// the v1 `dep-add` op with a `kind` field. The wire stanza is:
    ///
    /// ```text
    /// Jjf-Op: dep-add
    /// Jjf-Issue: <owner>
    /// Jjf-Dep: <target>
    /// Jjf-Dep-Kind: <blocks|parent-child|related|discovered-from>
    /// ```
    ///
    /// Backward compat: a v1 stanza with no `Jjf-Dep-Kind:` line
    /// reads as `kind: Blocks` (the only kind the v1 model had).
    DepAdd {
        issue_id: IssueId,
        dep: IssueId,
        #[serde(default)]
        kind: DepKind,
    },
    /// Remove a typed dependency edge. Symmetric to [`Op::DepAdd`] —
    /// the `kind` field is required to disambiguate when multiple
    /// kinds point at the same target. v1 stanzas without
    /// `Jjf-Dep-Kind:` read as `kind: Blocks`.
    DepRm {
        issue_id: IssueId,
        dep: IssueId,
        #[serde(default)]
        kind: DepKind,
    },
    SetAssignee {
        issue_id: IssueId,
        assignee: Option<String>,
    },
    /// Set the coarse `IssueType` (spec v2.1). The `kind` field is
    /// the new value; the wire spelling is the lowercase
    /// [`IssueType::as_str`].
    SetType {
        issue_id: IssueId,
        kind: IssueType,
    },
    /// Set the kebab-case slug (spec v2.1). `None` clears it.
    /// Validation (charset / length / hyphen rules) happens at the
    /// write boundary in [`crate::Storage`]; the trailer carries the
    /// chosen value verbatim or `""` for "no slug".
    SetSlug {
        issue_id: IssueId,
        slug: Option<String>,
    },
    CommentAdd {
        issue_id: IssueId,
        comment_id: IssueId,
    },
    Merge {
        issue_id: IssueId,
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
            Op::SetType { .. } => "set-type",
            Op::SetSlug { .. } => "set-slug",
            Op::CommentAdd { .. } => "comment-add",
            Op::Merge { .. } => "merge",
        }
    }

    /// The `Jjf-Issue:` value (every op carries one — spec §5.1).
    pub fn issue_id(&self) -> &IssueId {
        match self {
            Op::Create { issue_id, .. }
            | Op::SetTitle { issue_id, .. }
            | Op::SetStatus { issue_id, .. }
            | Op::SetBody { issue_id, .. }
            | Op::LabelAdd { issue_id, .. }
            | Op::LabelRm { issue_id, .. }
            | Op::DepAdd { issue_id, .. }
            | Op::DepRm { issue_id, .. }
            | Op::SetAssignee { issue_id, .. }
            | Op::SetType { issue_id, .. }
            | Op::SetSlug { issue_id, .. }
            | Op::CommentAdd { issue_id, .. }
            | Op::Merge { issue_id } => issue_id,
        }
    }

    /// Render one op stanza: the `Jjf-Op:` line, the `Jjf-Issue:` line,
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
        s.push_str("Jjf-Issue: ");
        s.push_str(self.issue_id().as_str());
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
            Op::DepAdd { dep, kind, .. } | Op::DepRm { dep, kind, .. } => {
                s.push_str("Jjf-Dep: ");
                s.push_str(dep.as_str());
                s.push('\n');
                s.push_str("Jjf-Dep-Kind: ");
                s.push_str(kind.as_str());
                s.push('\n');
            }
            Op::SetAssignee { assignee, .. } => {
                s.push_str("Jjf-Assignee: ");
                s.push_str(assignee.as_deref().unwrap_or(""));
                s.push('\n');
            }
            Op::SetType { kind, .. } => {
                s.push_str("Jjf-Type: ");
                s.push_str(kind.as_str());
                s.push('\n');
            }
            Op::SetSlug { slug, .. } => {
                s.push_str("Jjf-Slug: ");
                s.push_str(slug.as_deref().unwrap_or(""));
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
