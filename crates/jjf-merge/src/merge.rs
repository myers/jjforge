//! Per-field merge policy for jjforge v1 bug records.
//!
//! Inputs are two reconstructed JSON `Value`s (side A and side B); the
//! base is not required for v1's policy — last-write-wins per scalar
//! and set-union per array can both be expressed without a 3-way
//! diff.
//!
//! See `docs/storage-format.md` §3 for the record schema. Comments
//! live in a separate file (§4) and are intentionally not handled
//! here — they're a v2 ticket.

use serde_json::{Map, Value};

/// Which input "wins" the deterministic tiebreaker for scalar
/// last-write-wins fields.
///
/// v1 defers the author-timestamp tiebreaker. The caller (e.g. `jjf
/// sync`) is expected to inspect the two parent commits and pass
/// `Side::A` or `Side::B` based on whichever parent has the later
/// `author.timestamp` (per the ticket body). For the merge crate in
/// isolation we accept this as an explicit input.
///
/// Deterministic default: `B` (the "incoming" side in jj's conflict
/// block layout). Matches the Python prototype's chosen demonstration
/// in `experiments/distributed-edit/test-followup-distance-and-recovery.sh`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Side {
    A,
    #[default]
    B,
}

/// Per-field policy. v1 ships hard-coded defaults; the struct exists
/// so callers can override individual fields without forking the
/// crate.
#[derive(Debug, Clone)]
pub struct MergePolicy {
    /// Fields treated as scalars (last-write-wins).
    pub scalar_fields: Vec<String>,
    /// Fields treated as arrays (set-union, deterministic sort).
    pub array_fields: Vec<String>,
}

impl Default for MergePolicy {
    fn default() -> Self {
        // TODO(2026-06-29): the `metadata` field added in the
        // feat/issue-metadata PR is a per-key LWW map
        // (BTreeMap<String,String>). The v1 driver here has no policy
        // variant for that shape — only scalar_fields and array_fields
        // exist. The runtime merge path for v3 repos goes through
        // merge_ops.rs (which has a correct per-key reducer), so the
        // absence here is not a correctness issue today. If anyone
        // wires the v1 driver back into the write path, file a ticket
        // to add a `map_fields: Vec<String>` variant to MergePolicy
        // before doing so.
        Self {
            // Mirrors docs/storage-format.md §3.1 minus arrays and
            // minus `updated_at` (always taken from the winning
            // side via LWW; no special handling needed).
            scalar_fields: ["version", "id", "title", "body", "status",
                            "assignee", "created_at", "updated_at"]
                .iter().map(|s| s.to_string()).collect(),
            array_fields: ["labels", "dependencies"]
                .iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MergeOptions {
    pub policy: MergePolicy,
    /// Which side wins for last-write-wins fields when both sides
    /// differ.
    pub prefer_side: Side,
}

/// Merge two JSON objects per the policy.
///
/// Both inputs must be JSON objects (`Value::Object`); anything else
/// is rejected as `Error::Unmergeable`. Unknown fields (not listed in
/// `policy.scalar_fields` or `policy.array_fields`) fall through to
/// the same LWW rule as scalars — accept the side the
/// `prefer_side` hint chooses. This is conservative and lets the
/// schema grow without an immediate code change.
pub fn merge_values(
    a: &Value,
    b: &Value,
    opts: &MergeOptions,
) -> Result<Value, super::Error> {
    let (oa, ob) = match (a.as_object(), b.as_object()) {
        (Some(oa), Some(ob)) => (oa, ob),
        _ => {
            return Err(super::Error::Unmergeable(
                "both sides must be JSON objects".to_string(),
            ));
        }
    };

    let mut out = Map::new();
    // Walk a stable union of keys. We use the policy's declared
    // scalar+array fields as the canonical ordering so the output
    // matches docs/storage-format.md §3.3 (field-ordering rule).
    let mut written: Vec<String> = Vec::new();
    for k in opts
        .policy
        .scalar_fields
        .iter()
        .chain(opts.policy.array_fields.iter())
    {
        if !oa.contains_key(k) && !ob.contains_key(k) {
            continue;
        }
        let merged = merge_field(k, oa.get(k), ob.get(k), opts)?;
        out.insert(k.clone(), merged);
        written.push(k.clone());
    }

    // Any keys in the inputs that the policy didn't enumerate: keep
    // them, LWW. Stable alphabetical order so output is reproducible.
    let mut extras: Vec<&String> = oa
        .keys()
        .chain(ob.keys())
        .filter(|k| !written.contains(k))
        .collect();
    extras.sort();
    extras.dedup();
    for k in extras {
        let merged = merge_field(k, oa.get(k), ob.get(k), opts)?;
        out.insert(k.clone(), merged);
    }

    Ok(Value::Object(out))
}

fn merge_field(
    key: &str,
    a: Option<&Value>,
    b: Option<&Value>,
    opts: &MergeOptions,
) -> Result<Value, super::Error> {
    // Single-sided: the present side wins.
    let (a, b) = match (a, b) {
        (Some(a), Some(b)) => (a, b),
        (Some(a), None) => return Ok(a.clone()),
        (None, Some(b)) => return Ok(b.clone()),
        (None, None) => unreachable!(),
    };

    // Equal: idempotent.
    if a == b {
        return Ok(a.clone());
    }

    if opts.policy.array_fields.iter().any(|f| f == key) {
        return union_arrays(a, b);
    }

    // Scalar or unknown: LWW per prefer_side.
    Ok(match opts.prefer_side {
        Side::A => a.clone(),
        Side::B => b.clone(),
    })
}

fn union_arrays(a: &Value, b: &Value) -> Result<Value, super::Error> {
    let (aa, bb) = match (a.as_array(), b.as_array()) {
        (Some(aa), Some(bb)) => (aa, bb),
        _ => {
            return Err(super::Error::Unmergeable(
                "array field has non-array side".to_string(),
            ));
        }
    };
    // Dedup by stringified JSON — handles scalars (string IDs,
    // numbers) and structured array entries alike. Sort by the same
    // key so order is deterministic regardless of input order.
    let mut seen: Vec<(String, Value)> = Vec::new();
    for item in aa.iter().chain(bb.iter()) {
        let key = serde_json::to_string(item)?;
        if !seen.iter().any(|(k, _)| k == &key) {
            seen.push((key, item.clone()));
        }
    }
    seen.sort_by(|x, y| x.0.cmp(&y.0));
    Ok(Value::Array(seen.into_iter().map(|(_, v)| v).collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scalar_lww_prefers_side_b_by_default() {
        let a = json!({"title": "alice"});
        let b = json!({"title": "bob"});
        let merged = merge_values(&a, &b, &MergeOptions::default()).unwrap();
        assert_eq!(merged["title"], json!("bob"));
    }

    #[test]
    fn scalar_lww_prefers_side_a_when_asked() {
        let a = json!({"title": "alice"});
        let b = json!({"title": "bob"});
        let opts = MergeOptions {
            prefer_side: Side::A,
            ..Default::default()
        };
        let merged = merge_values(&a, &b, &opts).unwrap();
        assert_eq!(merged["title"], json!("alice"));
    }

    #[test]
    fn array_set_union_dedup_sort() {
        let a = json!({"labels": ["bug", "p1"]});
        let b = json!({"labels": ["p1", "regression"]});
        let merged = merge_values(&a, &b, &MergeOptions::default()).unwrap();
        assert_eq!(merged["labels"], json!(["bug", "p1", "regression"]));
    }

    #[test]
    fn dependencies_union() {
        let a = json!({"dependencies": ["aaa1111"]});
        let b = json!({"dependencies": ["bbb2222", "aaa1111"]});
        let merged = merge_values(&a, &b, &MergeOptions::default()).unwrap();
        assert_eq!(
            merged["dependencies"],
            json!(["aaa1111", "bbb2222"])
        );
    }

    #[test]
    fn equal_field_is_idempotent() {
        let a = json!({"title": "same", "status": "open"});
        let b = json!({"title": "same", "status": "open"});
        let merged = merge_values(&a, &b, &MergeOptions::default()).unwrap();
        assert_eq!(merged, json!({"title": "same", "status": "open"}));
    }

    #[test]
    fn one_sided_field_present_wins() {
        let a = json!({"title": "t", "assignee": "alice"});
        let b = json!({"title": "t"});
        let merged = merge_values(&a, &b, &MergeOptions::default()).unwrap();
        assert_eq!(merged["assignee"], json!("alice"));
    }

    #[test]
    fn field_ordering_matches_storage_spec() {
        let a = json!({"status": "open", "title": "t", "id": "abc1234"});
        let b = json!({"status": "closed", "title": "t", "id": "abc1234"});
        let merged = merge_values(&a, &b, &MergeOptions::default()).unwrap();
        let keys: Vec<&String> = merged.as_object().unwrap().keys().collect();
        // policy order is version, id, title, body, status, …
        let pos_id = keys.iter().position(|k| *k == "id").unwrap();
        let pos_title = keys.iter().position(|k| *k == "title").unwrap();
        let pos_status = keys.iter().position(|k| *k == "status").unwrap();
        assert!(pos_id < pos_title);
        assert!(pos_title < pos_status);
    }

    #[test]
    fn unknown_scalar_field_lww() {
        let a = json!({"future_field": 1});
        let b = json!({"future_field": 2});
        let merged = merge_values(&a, &b, &MergeOptions::default()).unwrap();
        assert_eq!(merged["future_field"], json!(2));
    }

    #[test]
    fn non_object_rejected() {
        let a = json!([1, 2]);
        let b = json!([3, 4]);
        let err = merge_values(&a, &b, &MergeOptions::default()).unwrap_err();
        assert!(matches!(err, super::super::Error::Unmergeable(_)));
    }
}
