//! Filters `python -m unittest` output — failures with their location and
//! assertion, one summary line; dots, verbose `... ok` rows and traceback
//! plumbing are dropped.

use crate::core::runner::{self, RunOptions};
use crate::core::utils::{fallback_tail, resolved_command, strip_ansi, tool_exists};
use anyhow::Result;
use regex::Regex;
use std::sync::LazyLock;

/// `FEF...s.` progress row (non-verbose mode).
static PROGRESS_ROW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[.FEsxu]+$").unwrap());
/// `test_add (tests.test_math.MathTests.test_add) ... ok` (verbose mode).
static VERBOSE_ROW: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\S+ \([^)]*\) \.\.\. (ok|FAIL|ERROR|skipped.*|expected failure|unexpected success)$")
        .unwrap()
});
/// `FAIL: test_dict (tests.test_bad.BadTests.test_dict)`
static BLOCK_HEADER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(FAIL|ERROR): (.+)$").unwrap());
/// `  File "/p/tests/test_bad.py", line 14, in test_dict`
static FRAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*File "([^"]+)", line (\d+), in (\S+)$"#).unwrap());
/// `Ran 8 tests in 0.001s`
static RAN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^Ran (\d+) tests? in ([\d.]+s)$").unwrap());
/// `OK`, `OK (skipped=1)`, `FAILED (failures=2, errors=1)`
static VERDICT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(OK|FAILED)(?: \((.+)\))?$").unwrap());

const MAX_DETAIL_LINES: usize = 12;

pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    let python = if tool_exists("python3") {
        "python3"
    } else {
        "python"
    };
    let mut cmd = resolved_command(python);
    cmd.args(["-m", "unittest"]);
    cmd.args(args);
    let args_display = args.join(" ");
    if verbose > 0 {
        eprintln!("Running: {} -m unittest {}", python, args_display);
    }
    let root = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned());
    runner::run_filtered_with_exit(
        cmd,
        "unittest",
        &args_display,
        move |output, exit_code| filter_unittest_output_with_root(output, exit_code, root.as_deref()),
        RunOptions::with_tee("unittest"),
    )
}

struct Failure {
    kind: String,
    name: String,
    lines: Vec<String>,
}

/// Keep, per failure, the test frame and the raising frame (`path:line in fn`
/// plus the source line), then the exception message and any assertion diff
/// (`- `/`+ ` rows, capped); end with `FAILED: failures=2, errors=1 · 8 tests
/// in 0.001s` or `ok 5 tests in 0.000s`.
/// `root` is the working directory: frame paths under it are shown relative.
pub fn filter_unittest_output_with_root(output: &str, exit_code: i32, root: Option<&str>) -> String {
    let clean = strip_ansi(output);
    let mut failures: Vec<Failure> = Vec::new();
    let mut current: Option<Failure> = None;
    let mut ran: Option<(String, String)> = None;
    let mut verdict: Option<(String, Option<String>)> = None;
    let mut other: Vec<String> = Vec::new();

    for line in clean.lines() {
        let t = line.trim_end();
        let tt = t.trim();
        if tt.is_empty() {
            continue;
        }
        if let Some(caps) = RAN.captures(tt) {
            if let Some(f) = current.take() {
                failures.push(f);
            }
            ran = Some((caps[1].to_string(), caps[2].to_string()));
            continue;
        }
        if let Some(caps) = VERDICT.captures(tt) {
            verdict = Some((caps[1].to_string(), caps.get(2).map(|m| m.as_str().to_string())));
            continue;
        }
        if tt.chars().all(|c| c == '=') || tt.chars().all(|c| c == '-') {
            if tt.starts_with('=') {
                if let Some(f) = current.take() {
                    failures.push(f);
                }
            }
            continue;
        }
        if let Some(caps) = BLOCK_HEADER.captures(tt) {
            if let Some(f) = current.take() {
                failures.push(f);
            }
            current = Some(Failure {
                kind: caps[1].to_string(),
                name: dotted_test_id(&caps[2]),
                lines: Vec::new(),
            });
            continue;
        }
        if let Some(f) = current.as_mut() {
            f.lines.push(t.to_string());
            continue;
        }
        if PROGRESS_ROW.is_match(tt) || VERBOSE_ROW.is_match(tt) {
            continue;
        }
        other.push(tt.to_string());
    }
    if let Some(f) = current.take() {
        failures.push(f);
    }

    let mut out: Vec<String> = other;
    for f in &failures {
        out.push(format!("{} {}", f.kind, f.name));
        out.extend(
            render_failure_body(&f.lines, &f.name, root)
                .into_iter()
                .map(|l| format!("  {}", l)),
        );
    }

    match (&verdict, &ran) {
        (Some((status, detail)), Some((count, time))) => {
            let tests = format!("{} tests in {}", count, time);
            out.push(if status == "OK" {
                match detail {
                    Some(d) => format!("ok {} ({})", tests, d),
                    None => format!("ok {}", tests),
                }
            } else {
                match detail {
                    Some(d) => format!("FAILED: {} · {}", d, tests),
                    None => format!("FAILED · {}", tests),
                }
            });
        }
        (Some((status, detail)), None) => out.push(match detail {
            Some(d) => format!("{} ({})", status, d),
            None => status.clone(),
        }),
        _ => {}
    }

    if out.is_empty() {
        return if exit_code == 0 {
            "ok".to_string()
        } else {
            fallback_tail(output, "unittest", 30)
        };
    }
    out.join("\n")
}

/// `test_dict (tests.test_bad.BadTests.test_dict)` → `tests.test_bad.BadTests.test_dict`;
/// the pre-3.11 form `test_dict (tests.test_bad.BadTests)` gets the method appended.
fn dotted_test_id(header: &str) -> String {
    static NAME_AND_ID: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(\S+) \((\S+)\)$").unwrap());
    match NAME_AND_ID.captures(header) {
        Some(caps) => {
            let (short, id) = (&caps[1], &caps[2]);
            if id.ends_with(&format!(".{}", short)) {
                id.to_string()
            } else {
                format!("{}.{}", id, short)
            }
        }
        None => header.to_string(),
    }
}

/// Show a frame path relative to the working directory, or from the test
/// package's top directory (`…/tests/test_bad.py` → `tests/test_bad.py`).
fn short_path(path: &str, test_id: &str, root: Option<&str>) -> String {
    if let Some(root) = root {
        if let Some(rest) = path.strip_prefix(root) {
            if let Some(rest) = rest.strip_prefix('/') {
                return rest.to_string();
            }
        }
    }
    if let Some(top) = test_id.split('.').next() {
        let marker = format!("/{}/", top);
        if let Some(idx) = path.find(&marker) {
            return path[idx + 1..].to_string();
        }
    }
    path.to_string()
}

/// Body of one FAIL/ERROR block: `Traceback` dropped, frames reduced to the
/// first (the test) and the last (where it raised) with their source line,
/// then the message and diff rows, minus the `?` caret rows. Diff rows are
/// dropped when the message already prints both sides (`a != b`).
fn render_failure_body(lines: &[String], test_id: &str, root: Option<&str>) -> Vec<String> {
    let test_fn = test_id.rsplit('.').next().unwrap_or("");
    let mut frames: Vec<(String, Option<String>)> = Vec::new();
    let mut tail: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim();
        if t == "Traceback (most recent call last):" {
            i += 1;
            continue;
        }
        if let Some(caps) = FRAME.captures(t) {
            let path = short_path(&caps[1], test_id, root);
            let loc = if &caps[3] == test_fn {
                format!("{}:{}", path, &caps[2])
            } else {
                format!("{}:{} in {}", path, &caps[2], &caps[3])
            };
            let mut source = None;
            if let Some(next) = lines.get(i + 1) {
                let nt = next.trim();
                if !FRAME.is_match(nt) && !nt.is_empty() && !is_marker_row(nt) {
                    source = Some(nt.to_string());
                    i += 1;
                }
            }
            frames.push((loc, source));
            i += 1;
            continue;
        }
        if is_marker_row(t) {
            i += 1;
            continue;
        }
        if frames.is_empty() && tail.is_empty() && t.starts_with('?') {
            i += 1;
            continue;
        }
        if t.starts_with("? ") || t == "?" {
            i += 1;
            continue;
        }
        tail.push(t.to_string());
        i += 1;
    }

    let mut out = Vec::new();
    let render = |(loc, source): &(String, Option<String>)| match source {
        Some(src) => format!("{}: {}", loc, src),
        None => loc.clone(),
    };
    match frames.len() {
        0 => {}
        1 => out.push(render(&frames[0])),
        n => {
            out.push(render(&frames[0]));
            out.push(render(&frames[n - 1]));
        }
    }
    if tail.iter().any(|l| l.contains(" != ")) {
        tail.retain(|l| !(l.starts_with("- ") || l.starts_with("+ ")));
    }
    let shown = tail.len().min(MAX_DETAIL_LINES);
    out.extend(tail.iter().take(shown).cloned());
    if tail.len() > shown {
        out.push(format!("+{} more lines", tail.len() - shown));
    }
    out
}

/// `~~^~~` / `^^^^` position markers under a source line.
fn is_marker_row(t: &str) -> bool {
    !t.is_empty() && t.chars().all(|c| matches!(c, '~' | '^' | ' '))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn savings(raw: &str, filtered: &str) -> f64 {
        100.0 - (filtered.len() as f64 / raw.len() as f64 * 100.0)
    }

    #[test]
    fn test_unittest_pass_is_one_line() {
        let raw = include_str!("../../../tests/fixtures/unittest_pass_raw.txt");
        let out = filter_unittest_output_with_root(raw, 0, None);
        assert_eq!(out, "ok 5 tests in 0.000s (skipped=1)");
        let raw_v = include_str!("../../../tests/fixtures/unittest_pass_v_raw.txt");
        assert_eq!(filter_unittest_output_with_root(raw_v, 0, None), "ok 5 tests in 0.000s (skipped=1)");
        assert!(savings(raw_v, &out) >= 60.0, "{:.1}%", savings(raw_v, &out));
    }

    #[test]
    fn test_unittest_failures_keep_location_and_assertion() {
        let raw = include_str!("../../../tests/fixtures/unittest_fail_raw.txt");
        let out = filter_unittest_output_with_root(raw, 1, None);
        let expected = [
            "ERROR tests.test_bad.BadTests.test_raises",
            "  tests/test_bad.py:11: helper(1)",
            "  tests/test_bad.py:4 in helper: return x / 0",
            "  ZeroDivisionError: division by zero",
            "FAIL tests.test_bad.BadTests.test_dict",
            "  tests/test_bad.py:14: self.assertEqual({\"a\": 1, \"b\": [1, 2]}, {\"a\": 1, \"b\": [1, 3]})",
            "  AssertionError: {'a': 1, 'b': [1, 2]} != {'a': 1, 'b': [1, 3]}",
            "FAIL tests.test_bad.BadTests.test_wrong_sum",
            "  tests/test_bad.py:8: self.assertEqual(1 + 1, 3, \"sum should be three\")",
            "  AssertionError: 2 != 3 : sum should be three",
            "FAILED: failures=2, errors=1, skipped=1 · 8 tests in 0.001s",
        ]
        .join("\n");
        assert_eq!(out, expected);
        assert!(savings(raw, &out) >= 60.0, "{:.1}%", savings(raw, &out));
    }

    #[test]
    fn test_unittest_verbose_failures_drop_result_rows() {
        let raw = include_str!("../../../tests/fixtures/unittest_fail_v_raw.txt");
        let out = filter_unittest_output_with_root(raw, 1, None);
        assert!(!out.contains("... ok"), "{out}");
        assert!(!out.contains("... FAIL"), "{out}");
        assert!(out.starts_with("ERROR tests.test_bad.BadTests.test_raises"), "{out}");
        assert!(out.ends_with("FAILED: failures=2, errors=1, skipped=1 · 8 tests in 0.001s"));
        assert!(savings(raw, &out) >= 60.0, "{:.1}%", savings(raw, &out));
    }

    #[test]
    fn test_unittest_import_error_keeps_message() {
        let raw = "E\n======================================================================\nERROR: tests (unittest.loader._FailedTest.tests)\n----------------------------------------------------------------------\nImportError: Failed to import test module: tests\nTraceback (most recent call last):\n  File \"/usr/lib/python3.12/unittest/loader.py\", line 396, in _find_test_path\n    module = self._get_module_from_name(name)\nModuleNotFoundError: No module named 'zod'\n\n\n----------------------------------------------------------------------\nRan 1 test in 0.000s\n\nFAILED (errors=1)\n";
        let out = filter_unittest_output_with_root(raw, 1, None);
        assert!(out.contains("ImportError: Failed to import test module: tests"), "{out}");
        assert!(out.contains("ModuleNotFoundError: No module named 'zod'"), "{out}");
        assert!(out.ends_with("FAILED: errors=1 · 1 tests in 0.000s"), "{out}");
    }

    #[test]
    fn test_unittest_root_relative_paths_and_legacy_ids() {
        let raw = "F\n======================================================================\nFAIL: test_x (pkg.test_mod.T)\n----------------------------------------------------------------------\nTraceback (most recent call last):\n  File \"/work/pkg/test_mod.py\", line 3, in test_x\n    self.assertTrue(False)\nAssertionError: False is not true\n\n----------------------------------------------------------------------\nRan 1 test in 0.000s\n\nFAILED (failures=1)\n";
        let out = filter_unittest_output_with_root(raw, 1, Some("/work"));
        assert_eq!(
            out,
            "FAIL pkg.test_mod.T.test_x\n  pkg/test_mod.py:3: self.assertTrue(False)\n  AssertionError: False is not true\nFAILED: failures=1 · 1 tests in 0.000s"
        );
        assert_eq!(dotted_test_id("test_x (pkg.test_mod.T.test_x)"), "pkg.test_mod.T.test_x");
        assert_eq!(short_path("/opt/app/tests/a.py", "tests.a.T.t", None), "tests/a.py");
        assert_eq!(short_path("/usr/lib/python3.12/unittest/loader.py", "tests", None), "/usr/lib/python3.12/unittest/loader.py");
    }

    #[test]
    fn test_unittest_unparsed_failure_shows_tail() {
        let out = filter_unittest_output_with_root("python3: No module named foo\n", 1, None);
        assert_eq!(out, "python3: No module named foo");
        assert_eq!(filter_unittest_output_with_root("", 0, None), "ok");
    }
}
