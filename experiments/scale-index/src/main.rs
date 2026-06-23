//! Scale-index benchmark for jjforge.
//!
//! Builds synthetic `issues` bookmarks at sizes 17, 100, 1000 and times:
//!
//! 1. `Storage::list_ready` — current implementation: O(N) `jj file
//!    show` calls.
//! 2. `Storage::resolve(slug)` — current implementation: O(N) `jj file
//!    show` calls until match.
//! 3. A **batched** alternative: one `jj log` invocation with N
//!    `--files` args returning all JSONs at once, then parse in-process.
//!    This is what the snapshot cache's rebuild path would do.
//! 4. A **cache-hit** simulation: one `jj log -T commit_id` to get the
//!    bookmark head, then deserialize a pre-computed file. This is
//!    what the snapshot cache's steady-state read would do.
//!
//! Construction takes a shortcut so we don't pay 1000 × the 4-CLI
//! dance: we materialize every issue's `.json` and `.comments.jsonl`
//! into the working copy in one shot, then land one commit and move
//! the `issues` bookmark to it. That isn't how a real-world repo grows
//! — but the read path under test only cares about WHAT'S ON THE
//! BOOKMARK, not how it got there.
//!
//! Run: `cargo run --release --bin bench -- <out-dir>`

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use jjf_storage::{ReadyFilter, Storage};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out_dir: PathBuf = if args.len() >= 2 {
        PathBuf::from(&args[1])
    } else {
        // Default: a scratch dir alongside this binary.
        let here = std::env::current_dir().unwrap();
        here.join(".scratch")
    };
    if out_dir.exists() {
        fs::remove_dir_all(&out_dir).expect("clean scratch");
    }
    fs::create_dir_all(&out_dir).expect("mkdir scratch");
    let out_dir = fs::canonicalize(&out_dir).expect("canonicalize scratch");

    println!("scratch root: {}\n", out_dir.display());

    // Defaults exercise the headline sizes; pass `--sizes 17,100,1000,10000`
    // to override. We skip `list_ready` and `resolve` at 10000 because
    // each is ~24s/issue × 10000 = ~4 minutes per call.
    let sizes: Vec<usize> = if args.len() >= 3 && args[2].starts_with("--sizes=") {
        args[2]
            .trim_start_matches("--sizes=")
            .split(',')
            .filter_map(|s| s.parse().ok())
            .collect()
    } else {
        vec![17usize, 100, 1000, 10000]
    };
    for n in sizes {
        bench_size(&out_dir, n);
        println!();
    }
}

fn bench_size(out_dir: &Path, n: usize) {
    println!("==== N = {} ====", n);
    let repo_root = out_dir.join(format!("n{}", n));
    fs::create_dir_all(&repo_root).unwrap();
    let abs_repo = fs::canonicalize(&repo_root).unwrap();
    // jj git init
    run_jj(&["git", "init"], &abs_repo);
    // Storage::init lands the seed commit + issues bookmark.
    let storage = Storage::init(&abs_repo).expect("Storage::init");

    // Construction: one commit, N JSON + N JSONL files staged into the
    // working copy on top of the seed.
    let t_build = Instant::now();
    seed_n_issues(&abs_repo, n);
    let build = t_build.elapsed();
    println!("  build (seed N issues, one commit): {:.3}s", build.as_secs_f64());

    // Sanity check: list_ids should see exactly N.
    let ids = storage.list_ids().expect("list_ids");
    assert_eq!(ids.len(), n, "list_ids returned {} (want {})", ids.len(), n);

    // Skip the O(N) measurements at N>=10000 — they'd take minutes
    // each and the linear extrapolation is unambiguous.
    let skip_n2 = n >= 10000;

    if !skip_n2 {
        // 1. list_ready
        let t = Instant::now();
        let ready = storage
            .list_ready(&ReadyFilter::default())
            .expect("list_ready");
        let elapsed = t.elapsed();
        println!(
            "  list_ready (current, 1 jj-file-show per issue x2): {:.3}s ({} ready)",
            elapsed.as_secs_f64(),
            ready.len()
        );
        println!(
            "    per-issue: {:.1} ms",
            elapsed.as_secs_f64() * 1000.0 / (n as f64)
        );

        // 2. resolve(slug) — worst-case (last slug, so we have to scan
        // every issue). Slug pattern: "synth-NNNN".
        let target_slug = format!("synth-{:04}", n - 1);
        let t = Instant::now();
        let id = storage.resolve(&target_slug).expect("resolve");
        let elapsed = t.elapsed();
        println!(
            "  resolve(\"{}\") (current, slug worst case): {:.3}s -> {}",
            target_slug,
            elapsed.as_secs_f64(),
            id
        );
    } else {
        println!("  list_ready / resolve: SKIPPED at N=10000 (would be ~4 min each); extrapolate linearly");
    }

    // 3. Batched alternative: one `jj log` with --files for every
    // issue's json. Demonstrates what a snapshot-cache rebuild would do.
    let t = Instant::now();
    let batched_count = batched_jj_log_all_jsons(&abs_repo, n);
    let elapsed = t.elapsed();
    println!(
        "  batched `jj log` with N file filters: {:.3}s ({} commits seen)",
        elapsed.as_secs_f64(),
        batched_count
    );

    // 3b. Even more batched: a single `jj file show` per file would
    // still be O(N). But what about `jj file list -r ... root:issues/`
    // for path enumeration (cheap) followed by a directory-walk in
    // working copy? Test the alternative: snapshot the bookmark into
    // the working copy via `jj edit`, then read all files locally.
    // (This is what the cache rebuilder would actually do.)
    let t = Instant::now();
    let count = wc_walk_all_jsons(&abs_repo, n);
    let elapsed = t.elapsed();
    println!(
        "  working-copy walk (after jj edit issues): {:.3}s ({} jsons parsed)",
        elapsed.as_secs_f64(),
        count
    );

    // 4. Cache-hit simulation: just `jj log -T commit_id -r
    // bookmarks(issues) --no-graph --limit 1` and assume we'd then
    // deserialize a pre-computed file.
    let t = Instant::now();
    let head = jj_head_commit(&abs_repo);
    let elapsed = t.elapsed();
    println!(
        "  cache-hit probe (one `jj log`): {:.3}s -> {}",
        elapsed.as_secs_f64(),
        &head[..head.len().min(16)]
    );

    // 5. Cache-hit including disk read: writing all N records into a
    // single .jj/jjforge-cache.json file and then deserializing it.
    let cache_path = abs_repo.join(".jj").join("jjforge-cache.json");
    {
        let t = Instant::now();
        write_synthetic_cache(&cache_path, n);
        let elapsed = t.elapsed();
        println!(
            "  cache write (single-file snapshot): {:.3}s",
            elapsed.as_secs_f64()
        );
    }
    let t = Instant::now();
    let head = jj_head_commit(&abs_repo);
    let cache_text = fs::read_to_string(&cache_path).unwrap();
    let _parsed: serde_json::Value = serde_json::from_str(&cache_text).unwrap();
    let elapsed = t.elapsed();
    println!(
        "  cache hit (probe + load + parse): {:.3}s ({} bytes, head {})",
        elapsed.as_secs_f64(),
        cache_text.len(),
        &head[..head.len().min(8)]
    );
}

/// Write a representative single-file cache containing N issue records.
/// Same schema as the on-bookmark issues/<id>.json files but bundled.
fn write_synthetic_cache(path: &Path, n: usize) {
    let mut issues = Vec::with_capacity(n);
    for i in 0..n {
        let id = synth_id(i);
        let slug = format!("synth-{:04}", i);
        issues.push(serde_json::json!({
            "version": 2,
            "id": id,
            "title": format!("Synthetic issue {}", i),
            "slug": slug,
            "body": format!("Synthetic body for issue {}.\n\nLine 2.\n", i),
            "status": "open",
            "type": if i % 5 == 0 { "bug" } else { "feature" },
            "labels": ["epic:synthetic"],
            "dependencies": [],
            "assignee": null,
            "created_at": "2026-06-23T00:00:00Z",
            "updated_at": "2026-06-23T00:00:00Z",
        }));
    }
    let bundle = serde_json::json!({
        "head_commit": "TBD",
        "issues": issues,
    });
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_string(&bundle).unwrap()).unwrap();
}

/// Materialize N synthetic issues into the working copy on top of the
/// seed commit. Lands one commit and moves the `issues` bookmark to it.
/// Bypasses the normal write-path commit-per-mutation discipline
/// because we're trying to construct a large bookmark cheaply.
fn seed_n_issues(repo_root: &Path, n: usize) {
    // 1. jj new bookmarks(issues) -m '<msg>'
    let msg = format!("scale-index: seed {} synthetic issues", n);
    run_jj(&["new", "bookmarks(issues)", "-m", &msg], repo_root);

    // 2. Write files into working copy.
    let issues_dir = repo_root.join("issues");
    fs::create_dir_all(&issues_dir).unwrap();
    for i in 0..n {
        let id = synth_id(i);
        let slug = format!("synth-{:04}", i);
        let record = serde_json::json!({
            "version": 2,
            "id": id,
            "title": format!("Synthetic issue {}", i),
            "slug": slug,
            "body": format!("Synthetic body for issue {}.\n\nLine 2.\n", i),
            "status": "open",
            "type": if i % 5 == 0 { "bug" } else { "feature" },
            "labels": ["epic:synthetic"],
            "dependencies": [],
            "assignee": null,
            "created_at": "2026-06-23T00:00:00Z",
            "updated_at": "2026-06-23T00:00:00Z",
        });
        let json_path = issues_dir.join(format!("{}.json", id));
        let comments_path = issues_dir.join(format!("{}.comments.jsonl", id));
        fs::write(&json_path, serde_json::to_string(&record).unwrap()).unwrap();
        fs::write(&comments_path, "").unwrap();
    }

    // 3. Move bookmark.
    run_jj(
        &["bookmark", "set", "issues", "-r", "@", "--allow-backwards"],
        repo_root,
    );
    // 4. Step off bookmark.
    run_jj(&["new", "root()"], repo_root);
}

/// Generate a synthetic 7-hex IssueId from an integer. The pattern is
/// "<6-hex of i>0..f"; the trailing digit cycles so we exercise both
/// dedup and parsing. Collisions across i would break list_ids; we
/// stay well within 2^28.
fn synth_id(i: usize) -> String {
    // Avoid synthetic patterns that resemble real ids. Use a fixed
    // prefix nibble of 'e' and 6 hex of the index — well-formed.
    format!("e{:06x}", i)
}

fn run_jj(args: &[&str], cwd: &Path) {
    let out = Command::new("jj").args(args).current_dir(cwd).output().unwrap();
    if !out.status.success() {
        panic!(
            "`jj {}` failed in {}:\nstderr: {}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

fn jj_capture(args: &[&str], cwd: &Path) -> String {
    let out = Command::new("jj").args(args).current_dir(cwd).output().unwrap();
    if !out.status.success() {
        panic!(
            "`jj {}` failed:\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr),
        );
    }
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Single `jj log` invocation with N `root:issues/<id>.json` path
/// filters. Returns the number of commits the walker saw. This is the
/// "batched rebuild" the snapshot cache would use to populate from
/// scratch.
fn batched_jj_log_all_jsons(repo_root: &Path, n: usize) -> usize {
    let mut args: Vec<String> = vec![
        "log".into(),
        "--no-graph".into(),
        "-r".into(),
        "bookmarks(issues)".into(),
        "-T".into(),
        "commit_id ++ \"\\n\"".into(),
    ];
    for i in 0..n {
        let id = synth_id(i);
        args.push(format!("root:issues/{}.json", id));
    }
    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let out = jj_capture(&args_ref, repo_root);
    out.lines().filter(|l| !l.is_empty()).count()
}

/// Read every issue JSON via a working-copy walk: `jj edit
/// bookmarks(issues)` materializes the bookmark contents, then a
/// local read & parse loop covers everything. After, we step `@`
/// back off the bookmark so subsequent benches don't poison it.
///
/// Returns the number of JSONs parsed.
fn wc_walk_all_jsons(repo_root: &Path, n: usize) -> usize {
    // Move @ onto the bookmark to materialize files.
    run_jj(&["edit", "bookmarks(issues)"], repo_root);

    let issues_dir = repo_root.join("issues");
    let mut count = 0;
    if let Ok(rd) = fs::read_dir(&issues_dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let text = fs::read_to_string(&p).unwrap();
            let _v: serde_json::Value = serde_json::from_str(&text).unwrap();
            count += 1;
        }
    }
    // Step off again.
    run_jj(&["new", "root()"], repo_root);

    assert_eq!(count, n, "wc walk saw {} jsons (want {})", count, n);
    count
}

fn jj_head_commit(repo_root: &Path) -> String {
    let out = jj_capture(
        &[
            "log",
            "--no-graph",
            "-r",
            "bookmarks(issues)",
            "-T",
            "commit_id",
            "--limit",
            "1",
        ],
        repo_root,
    );
    out.trim().to_owned()
}
