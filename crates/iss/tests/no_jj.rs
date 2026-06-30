//! Acceptance test: full `jjf` lifecycle with `jj` absent from PATH.
//!
//! This test proves the jj divorce: every code path in the compiled
//! binary must work on a plain git repo without any jj executable
//! reachable on PATH. If any shipped code still spawned `jj`, this
//! test would fail because `jj` is unreachable on the scrubbed PATH.

use std::fs;
use std::path::PathBuf;

/// Scrub jj from PATH: keep only directories that do NOT contain a `jj`
/// executable. Filesystem binaries like `git` remain reachable because
/// their directories (e.g. `/usr/bin`) do not contain a `jj` file.
fn minimal_path_without_jj() -> String {
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !std::path::Path::new(d).join("jj").exists())
        .collect::<Vec<_>>()
        .join(":")
}

/// Ephemeral scratch directory outside the jjforge source tree.
/// Cleaned up at the end of the test.
fn make_scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join("jjf-no-jj-tests")
        .join(name);
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::create_dir_all(&dir).unwrap();
    fs::canonicalize(&dir).unwrap()
}

#[test]
fn full_lifecycle_without_jj_on_path() {
    let root = make_scratch("full_lifecycle");

    // Set up a plain git repo with a local identity — no jj involved.
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&root)
            .output()
            .expect("git")
    };
    let init_out = git(&["init"]);
    assert!(init_out.status.success(), "git init failed");
    git(&["config", "user.name", "Tester"]);
    git(&["config", "user.email", "t@example.com"]);

    let bin = env!("CARGO_BIN_EXE_iss");
    let scrubbed_path = minimal_path_without_jj();

    let run = |args: &[&str]| -> std::process::Output {
        std::process::Command::new(bin)
            .current_dir(&root)
            .args(args)
            .env("PATH", &scrubbed_path)
            // Isolate from dev's global git config; repo-local config
            // (set above) is the sole source of identity.
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("spawn jjf")
    };

    // `jjf init` plants the v3 sentinel ref on a plain git repo.
    let init_out = run(&["init"]);
    assert!(
        init_out.status.success(),
        "jjf init failed with jj absent: code={:?} stderr={}",
        init_out.status.code(),
        String::from_utf8_lossy(&init_out.stderr),
    );

    // `jjf new` creates an issue and commits it as a git ref.
    let new_out = run(&["--json", "new", "-t", "first issue", "-F", "-"]);
    assert!(
        new_out.status.success(),
        "jjf new failed with jj absent: code={:?} stderr={}",
        new_out.status.code(),
        String::from_utf8_lossy(&new_out.stderr),
    );
    let stdout = String::from_utf8_lossy(&new_out.stdout);
    let envelope: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("new --json must emit valid JSON");
    let id = envelope["id"]
        .as_str()
        .expect("envelope must have 'id'")
        .to_owned();
    assert_eq!(id.len(), 7, "id must be 7 hex chars: {id:?}");

    // `jjf ls` reads the issue back via git refs (no jj).
    let ls_out = run(&["ls", "--json"]);
    assert!(
        ls_out.status.success(),
        "jjf ls failed with jj absent: code={:?} stderr={}",
        ls_out.status.code(),
        String::from_utf8_lossy(&ls_out.stderr),
    );
    let ls_stdout = String::from_utf8_lossy(&ls_out.stdout);
    assert!(
        ls_stdout.contains("first issue"),
        "ls should show the created issue; got: {ls_stdout}"
    );

    // `jjf show <id>` also resolves without jj.
    let show_out = run(&["show", &id]);
    assert!(
        show_out.status.success(),
        "jjf show failed with jj absent: code={:?} stderr={}",
        show_out.status.code(),
        String::from_utf8_lossy(&show_out.stderr),
    );
    let show_stdout = String::from_utf8_lossy(&show_out.stdout);
    assert!(
        show_stdout.contains("first issue"),
        "show should contain the issue title; got: {show_stdout}"
    );

    // Clean up.
    let _ = fs::remove_dir_all(&root);
}
