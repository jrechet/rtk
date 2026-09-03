//! Filters yarn output (classic v1 and berry) — install/add progress steps,
//! run banners and script echoes, peer-dependency warning floods.

use crate::core::runner::{self, RunOptions};
use crate::core::truncate::CAP_WARNINGS;
use crate::core::utils::{fallback_tail, resolved_command, strip_ansi};
use anyhow::Result;
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

/// `yarn install v1.22.22`, `yarn run v1.22.22`, `yarn add v1.22.22`.
static YARN_BANNER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^yarn(?: [a-z-]+)? v\d+\.\d+").unwrap());
/// `[1/4] Resolving packages...`
static YARN_STEP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[\d+/\d+\] ").unwrap());
/// Tree rows of the `info Direct dependencies` / `info All dependencies` sections.
static YARN_TREE_ROW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[│├└─\s]+\S").unwrap());

/// Berry codes that report a failure rather than a warning.
const BERRY_ERROR_CODES: &[&str] = &["YN0001:", "YN0009:", "YN0018:", "YN0028:", "YN0041:"];

/// `info` lines that only narrate the run.
const YARN_INFO_NOISE: &[&str] = &[
    "Visit https://",
    "No lockfile found",
    "incompatible with this module",
    "optional dependency",
    "Lockfile not saved",
];

pub fn run(args: &[String], verbose: u8) -> Result<i32> {
    let mut cmd = resolved_command("yarn");
    cmd.args(args);
    let args_display = args.join(" ");
    if verbose > 0 {
        eprintln!("Running: yarn {}", args_display);
    }
    runner::run_filtered_with_exit(
        cmd,
        "yarn",
        &args_display,
        filter_yarn_output,
        RunOptions::with_tee("yarn"),
    )
}

/// Drop banners, progress steps, script echoes and `Done in`; keep results,
/// errors and the direct-dependency tree; collapse warnings to `CAP_WARNINGS`.
/// The transitive `info All dependencies` tree is dropped: it repeats the
/// direct tree plus everything below it.
pub fn filter_yarn_output(output: &str, exit_code: i32) -> String {
    let clean = strip_ansi(output);
    let mut kept: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut seen_warnings: HashSet<String> = HashSet::new();
    let mut in_all_deps = false;

    for line in clean.lines() {
        let line = line.trim_end();
        let t = line.trim_start();
        if t.is_empty() {
            continue;
        }
        if in_all_deps {
            if YARN_TREE_ROW.is_match(t) {
                continue;
            }
            in_all_deps = false;
        }
        if YARN_BANNER.is_match(t)
            || YARN_STEP.is_match(t)
            || t.starts_with("$ ")
            || t.starts_with("Done in ")
        {
            continue;
        }
        if let Some(rest) = t.strip_prefix("info ") {
            if rest.starts_with("All dependencies") {
                in_all_deps = true;
                continue;
            }
            // The direct tree rows speak for themselves; the header is noise.
            if rest.starts_with("Direct dependencies")
                || YARN_INFO_NOISE.iter().any(|noise| rest.contains(noise))
            {
                continue;
            }
            kept.push(line.to_string());
            continue;
        }
        // Every successful install/add saves the lockfile; the interesting
        // `success` lines are the ones that count dependencies.
        if t == "success Saved lockfile." || t == "success Already up-to-date." {
            continue;
        }
        if t.starts_with("warning ") {
            if seen_warnings.insert(t.to_string()) {
                warnings.push(t.to_string());
            }
            continue;
        }
        // Yarn berry: `➤ YN0000: ┌ Resolution step`, `➤ YN0002: │ peer warning`.
        if let Some(rest) = t.strip_prefix("➤ ") {
            if rest.starts_with("YN0000:") {
                continue; // step structure, timings, "Done"
            }
            if rest.starts_with("YN0013:")
                || rest.contains("Error")
                || BERRY_ERROR_CODES.iter().any(|code| rest.starts_with(code))
            {
                kept.push(line.to_string());
            } else if seen_warnings.insert(t.to_string()) {
                warnings.push(t.to_string());
            }
            continue;
        }
        kept.push(line.to_string());
    }

    if !warnings.is_empty() {
        let shown = warnings.len().min(CAP_WARNINGS);
        kept.extend(warnings.iter().take(shown).cloned());
        if warnings.len() > shown {
            kept.push(format!("+{} more warnings", warnings.len() - shown));
        }
    }

    if kept.is_empty() {
        return if exit_code == 0 {
            "ok".to_string()
        } else {
            fallback_tail(output, "yarn", 20)
        };
    }
    kept.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn savings(raw: &str, filtered: &str) -> f64 {
        100.0 - (filtered.len() as f64 / raw.len() as f64 * 100.0)
    }

    #[test]
    fn test_yarn_install_no_changes_is_ok() {
        let raw = include_str!("../../../tests/fixtures/yarn_install_raw.txt");
        assert_eq!(filter_yarn_output(raw, 0), "ok");
    }

    #[test]
    fn test_yarn_add_keeps_result_and_direct_tree() {
        let raw = include_str!("../../../tests/fixtures/yarn_add_raw.txt");
        let out = filter_yarn_output(raw, 0);
        assert_eq!(
            out,
            "success Saved 1 new dependency.\n└─ left-pad@1.3.0\nwarning left-pad@1.3.0: use String.prototype.padStart()"
        );
        assert!(savings(raw, &out) >= 60.0, "{:.1}%", savings(raw, &out));
    }

    #[test]
    fn test_yarn_run_failure_keeps_script_output_and_error() {
        let raw = include_str!("../../../tests/fixtures/yarn_run_fail_raw.txt");
        assert_eq!(
            filter_yarn_output(raw, 2),
            "boom\nerror Command failed with exit code 2."
        );
    }

    #[test]
    fn test_yarn_run_wrapping_a_test_runner_drops_only_yarn_noise() {
        let raw = include_str!("../../../tests/fixtures/yarn_test_raw.txt");
        let out = filter_yarn_output(raw, 0);
        assert!(!out.contains("yarn run v"), "{out}");
        assert!(!out.contains("$ bun test"), "{out}");
        assert!(!out.contains("Done in"), "{out}");
        assert!(out.contains("Ran 7 tests across 2 files."), "{out}");
    }

    #[test]
    fn test_yarn_peer_warnings_are_capped_and_deduplicated() {
        let mut raw = String::from("yarn install v1.22.22\n[1/4] Resolving packages...\n");
        for i in 0..25 {
            raw.push_str(&format!(
                "warning \"pkg{} > dep@1.0.0\" has unmet peer dependency \"react@*\".\n",
                i
            ));
        }
        raw.push_str("warning \"pkg0 > dep@1.0.0\" has unmet peer dependency \"react@*\".\n");
        raw.push_str("success Saved lockfile.\nDone in 3.21s.\n");
        let out = filter_yarn_output(&raw, 0);
        assert!(out.starts_with("warning \"pkg0 > dep@1.0.0\""), "{out}");
        assert_eq!(out.matches("unmet peer").count(), CAP_WARNINGS);
        assert!(out.ends_with("+15 more warnings"), "{out}");
        assert!(savings(&raw, &out) >= 60.0);
    }

    #[test]
    fn test_yarn_berry_keeps_errors_and_added_summary() {
        let raw = "\
➤ YN0000: · Yarn 4.1.0
➤ YN0000: ┌ Resolution step
➤ YN0002: │ demo@workspace:. doesn't provide react, requested by x
➤ YN0002: │ demo@workspace:. doesn't provide react, requested by y
➤ YN0000: └ Completed in 0s 412ms
➤ YN0000: ┌ Fetch step
➤ YN0013: │ 3 packages were added to the project (+ 120 KiB).
➤ YN0000: └ Completed
➤ YN0000: ┌ Link step
➤ YN0001: │ Error: EACCES: permission denied, mkdir '/x'
➤ YN0000: └ Completed
➤ YN0000: · Failed with errors in 1s 2ms
";
        let out = filter_yarn_output(raw, 1);
        assert_eq!(
            out,
            "➤ YN0013: │ 3 packages were added to the project (+ 120 KiB).\n\
➤ YN0001: │ Error: EACCES: permission denied, mkdir '/x'\n\
➤ YN0002: │ demo@workspace:. doesn't provide react, requested by x\n\
➤ YN0002: │ demo@workspace:. doesn't provide react, requested by y"
        );
    }

    #[test]
    fn test_yarn_failure_with_nothing_kept_shows_raw_tail() {
        let raw = "yarn install v1.22.22\n[1/4] Resolving packages...\n";
        assert_eq!(filter_yarn_output(raw, 1), raw.trim_end());
    }
}
