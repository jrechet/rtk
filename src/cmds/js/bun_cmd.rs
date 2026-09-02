//! Filters bun output — install/add progress and lockfile chatter, `bun run`
//! script echoes, and `bun test` (failures with their assertion, one summary).

use crate::core::runner::{self, RunOptions};
use crate::core::truncate::CAP_LIST;
use crate::core::utils::{fallback_tail, resolved_command, strip_ansi};
use anyhow::Result;
use regex::Regex;
use std::sync::LazyLock;

/// `bun install v1.3.11 (af24e281)`, `bun test v1.3.11 (af24e281)`.
static BUN_BANNER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^bun(?: [a-z-]+)? v\d+\.\d+").unwrap());
/// `[0.52ms] migrated lockfile from yarn.lock`, `[3.00ms] done`.
static BUN_TIMING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[\d+(?:\.\d+)?m?s\] ").unwrap());
/// ` 5 pass`, ` 1 fail`, ` 1 skip`, ` 1 todo`.
static BUN_COUNT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\d+) (pass|fail|skip|todo)$").unwrap());
/// `Ran 9 tests across 3 files. [28.00ms]`
static BUN_RAN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Ran (\d+) tests? across (\d+) files?\. \[(.+)\]$").unwrap()
});
/// `src/bad.test.ts:` — a file header in bun test output.
static BUN_FILE_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\S+\.[cm]?[jt]sx?):$").unwrap());
/// `2 | test("wrong sum", ...)` — source excerpt lines, and the caret under them.
static BUN_SOURCE_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+ \| ").unwrap());
static BUN_CARET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*\^+\s*$").unwrap());
/// `(fail) wrong sum [2.02ms]` / `✗ wrong sum [2.02ms]`
static BUN_FAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\(fail\)|✗) (.+?)(?: \[[\d.]+m?s\])?$").unwrap());
/// `(pass) ok one [0.10ms]`, `(skip) ...`, `(todo) ...`, `✓ ...`
static BUN_OTHER_RESULT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\((?:pass|skip|todo)\)|✓|»|↓) ").unwrap());
/// `      at <anonymous> (/path/file.ts:2:41)` → `at /path/file.ts:2:41`
static BUN_AT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*at (?:<anonymous> |\S+ )?\(?([^()]+?)\)?$").unwrap());

const MAX_FRAMES_PER_FAILURE: usize = 2;

/// Install lines that narrate progress rather than report a result.
const BUN_INSTALL_NOISE: &[&str] = &[
    "Resolving...",
    "Resolving dependencies",
    "Resolved, downloaded and extracted",
    "Saved lockfile",
];

pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("bun");
    cmd.args(args);
    let args_display = args.join(" ");
    if verbose > 0 {
        eprintln!("Running: bun {}", args_display);
    }
    let is_test = args.first().is_some_and(|a| a == "test");
    let filter: fn(&str, i32) -> String = if is_test {
        filter_bun_test
    } else {
        filter_bun_output
    };
    runner::run_filtered_with_exit(cmd, "bun", &args_display, filter, RunOptions::with_tee("bun"))
}

/// `bunx <tool>`: same generic filter, the tool's own output stays intact.
pub fn exec(args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("bunx");
    cmd.args(args);
    let args_display = args.join(" ");
    if verbose > 0 {
        eprintln!("Running: bunx {}", args_display);
    }
    runner::run_filtered_with_exit(
        cmd,
        "bunx",
        &args_display,
        filter_bun_output,
        RunOptions::with_tee("bunx"),
    )
}

/// Generic bun filter (install, add, remove, run, x): drop the version banner,
/// timings, lockfile chatter and the `$ script` echo; cap the `+ pkg@ver` list.
pub fn filter_bun_output(output: &str, exit_code: i32) -> String {
    let clean = strip_ansi(output);
    let mut kept: Vec<String> = Vec::new();
    let mut added = 0usize;

    for line in clean.lines() {
        let line = line.trim_end();
        let t = line.trim_start();
        if t.is_empty()
            || BUN_BANNER.is_match(t)
            || BUN_TIMING.is_match(t)
            || t.starts_with("$ ")
            || BUN_INSTALL_NOISE.iter().any(|noise| t.starts_with(noise))
        {
            continue;
        }
        if t.starts_with("+ ") {
            added += 1;
            if added > CAP_LIST {
                continue;
            }
        }
        kept.push(line.to_string());
    }
    if added > CAP_LIST {
        // Insert right after the last shown `+` row.
        let pos = kept.iter().rposition(|l| l.trim_start().starts_with("+ "));
        let note = format!("+{} more packages", added - CAP_LIST);
        match pos {
            Some(i) => kept.insert(i + 1, note),
            None => kept.push(note),
        }
    }

    if kept.is_empty() {
        return if exit_code == 0 {
            "ok".to_string()
        } else {
            fallback_tail(output, "bun", 20)
        };
    }
    kept.join("\n")
}

/// `/abs/path/src/a.test.ts:3:5` → `src/a.test.ts:3:5` when the frame is in the
/// file whose header was just printed; other frames keep their full path.
fn relative_to_file<'a>(location: &'a str, file: Option<&str>) -> &'a str {
    match file {
        Some(file) => match location.find(&format!("/{}:", file)) {
            Some(idx) => &location[idx + 1..],
            None => location,
        },
        None => location,
    }
}

/// `bun test`: failures only — each as `(fail) name` followed by its
/// assertion message, expected/received and up to two frames — under their
/// file header, then one summary line. Source excerpts and per-test timings
/// are dropped; passing tests never appear.
pub fn filter_bun_test(output: &str, exit_code: i32) -> String {
    let clean = strip_ansi(output);
    let mut out: Vec<String> = Vec::new();
    let mut current_file: Option<String> = None;
    let mut file_emitted = false;
    let mut pending: Vec<String> = Vec::new();
    let mut frames = 0usize;
    let mut counts: Vec<(String, usize)> = Vec::new();
    let mut ran: Option<String> = None;

    for line in clean.lines() {
        let line = line.trim_end();
        let t = line.trim_start();
        if t.is_empty() || BUN_BANNER.is_match(t) || t.ends_with("expect() calls") {
            continue;
        }
        if let Some(caps) = BUN_COUNT.captures(t) {
            counts.push((caps[2].to_string(), caps[1].parse().unwrap_or(0)));
            continue;
        }
        if let Some(caps) = BUN_RAN.captures(t) {
            ran = Some(format!("{} tests in {} files [{}]", &caps[1], &caps[2], &caps[3]));
            continue;
        }
        if let Some(caps) = BUN_FILE_HEADER.captures(t) {
            current_file = Some(caps[1].to_string());
            file_emitted = false;
            pending.clear();
            frames = 0;
            continue;
        }
        if BUN_SOURCE_LINE.is_match(t) || BUN_CARET.is_match(t) {
            continue;
        }
        if let Some(caps) = BUN_FAIL.captures(t) {
            if let (Some(file), false) = (&current_file, file_emitted) {
                out.push(format!("{}:", file));
                file_emitted = true;
            }
            out.push(format!("(fail) {}", &caps[1]));
            out.extend(pending.drain(..).map(|l| format!("  {}", l)));
            frames = 0;
            continue;
        }
        if BUN_OTHER_RESULT.is_match(t) {
            pending.clear();
            frames = 0;
            continue;
        }
        if let Some(caps) = BUN_AT.captures(t) {
            frames += 1;
            if frames <= MAX_FRAMES_PER_FAILURE {
                pending.push(format!("at {}", relative_to_file(&caps[1], current_file.as_deref())));
            }
            continue;
        }
        pending.push(t.to_string());
    }

    // Zero counts are dropped except `0 pass`: a run with failures shows them,
    // a run without shows none, and the exit code says the rest.
    let mut summary = Vec::new();
    for (kind, n) in &counts {
        if *n > 0 || kind == "pass" {
            summary.push(format!("{} {}", n, kind));
        }
    }
    let mut summary_line = summary.join(", ");
    if let Some(ran) = ran {
        if !summary_line.is_empty() {
            summary_line.push_str(" · ");
        }
        summary_line.push_str(&ran);
    }
    if !summary_line.is_empty() {
        out.push(summary_line);
    }

    if out.is_empty() {
        return if exit_code == 0 {
            "ok".to_string()
        } else {
            fallback_tail(output, "bun test", 30)
        };
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn savings(raw: &str, filtered: &str) -> f64 {
        100.0 - (filtered.len() as f64 / raw.len() as f64 * 100.0)
    }

    #[test]
    fn test_bun_test_all_pass_is_one_line() {
        let raw = include_str!("../../../tests/fixtures/bun_test_pass_raw.txt");
        let out = filter_bun_test(raw, 0);
        assert_eq!(
            out,
            "5 pass, 1 skip, 1 todo · 7 tests in 2 files [661.00ms]"
        );
        // Without a TTY bun prints nothing per passing test, so a green run is
        // already small (150 bytes here): only the banner and count lines go.
        assert!(savings(raw, &out) >= 50.0, "{:.1}%", savings(raw, &out));
    }

    #[test]
    fn test_bun_test_failures_keep_assertion_and_location() {
        let raw = include_str!("../../../tests/fixtures/bun_test_fail_raw.txt");
        let out = filter_bun_test(raw, 1);
        assert_eq!(
            out,
            [
                "src/bad.test.ts:",
                "(fail) wrong sum",
                "  error: expect(received).toBe(expected)",
                "  Expected: 3",
                "  Received: 2",
                "  at src/bad.test.ts:2:41",
                "(fail) throws",
                "  error: kaboom",
                "  at src/bad.test.ts:3:48",
                "5 pass, 1 skip, 1 todo, 2 fail · 9 tests in 3 files [28.00ms]",
            ]
            .join("\n")
        );
        // Source excerpts and the caret are the bulk of the raw output; the
        // assertion, values and location are what remains.
        assert!(savings(raw, &out) >= 50.0, "{:.1}%", savings(raw, &out));
    }

    #[test]
    fn test_bun_test_mixed_file_shows_only_the_failure() {
        let raw = include_str!("../../../tests/fixtures/bun_test_mixed_raw.txt");
        let out = filter_bun_test(raw, 1);
        assert!(out.starts_with("src/mixed.test.ts:\n(fail) bad one\n"), "{out}");
        assert!(!out.contains("ok one"), "{out}");
        assert!(out.ends_with("7 pass, 1 skip, 1 todo, 1 fail · 10 tests in 3 files [11.00ms]"));
        // A single short failure: the assertion block is most of what remains.
        assert!(savings(raw, &out) >= 55.0, "{:.1}%", savings(raw, &out));
    }

    #[test]
    fn test_bun_test_tty_glyphs_and_frame_cap() {
        let raw = "\
bun test v1.3.11 (af24e281)

src/a.test.ts:
✓ fine [0.10ms]
error: expected 1 to equal 2
      at <anonymous> (/p/src/a.test.ts:3:5)
      at run (/p/node_modules/x.js:10:1)
      at main (/p/node_modules/y.js:20:2)
✗ broken [0.30ms]

 1 pass
 1 fail
Ran 2 tests across 1 files. [5.00ms]
";
        assert_eq!(
            filter_bun_test(raw, 1),
            [
                "src/a.test.ts:",
                "(fail) broken",
                "  error: expected 1 to equal 2",
                "  at src/a.test.ts:3:5",
                "  at /p/node_modules/x.js:10:1",
                "1 pass, 1 fail · 2 tests in 1 files [5.00ms]",
            ]
            .join("\n")
        );
    }

    #[test]
    fn test_bun_test_crash_without_summary_shows_raw_tail() {
        let raw = "bun test v1.3.11 (af24e281)\n\nerror: Cannot find module 'zod'\n";
        let out = filter_bun_test(raw, 1);
        assert!(out.contains("Cannot find module"), "{out}");
    }

    #[test]
    fn test_bun_install_keeps_added_packages_and_summary() {
        let raw = include_str!("../../../tests/fixtures/bun_install_raw.txt");
        assert_eq!(
            filter_bun_output(raw, 0),
            "+ is-odd@3.0.1\n+ left-pad@1.3.0\n3 packages installed [70.00ms]"
        );
        let raw = include_str!("../../../tests/fixtures/bun_add_raw.txt");
        let out = filter_bun_output(raw, 0);
        assert_eq!(out, "installed is-odd@3.0.1\n2 packages installed [333.00ms]");
        assert!(savings(raw, &out) >= 60.0, "{:.1}%", savings(raw, &out));
    }

    #[test]
    fn test_bun_install_caps_long_package_list() {
        let mut raw = String::from("bun install v1.3.11 (af24e281)\n\n");
        for i in 0..30 {
            raw.push_str(&format!("+ pkg{}@1.0.0\n", i));
        }
        raw.push_str("\n30 packages installed [90.00ms]\n");
        let out = filter_bun_output(&raw, 0);
        assert_eq!(out.matches("+ pkg").count(), CAP_LIST);
        assert!(out.contains("+10 more packages\n30 packages installed"), "{out}");
    }

    #[test]
    fn test_bun_run_drops_echo_and_keeps_script_output() {
        let raw = include_str!("../../../tests/fixtures/bun_run_fail_raw.txt");
        assert_eq!(filter_bun_output(raw, 2), "boom");
        assert_eq!(filter_bun_output("$ node x.js\n", 0), "ok");
    }
}
