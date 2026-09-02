//! Generic safety net for commands RTK has no filter for.
//!
//! When an unknown command falls back to raw execution and stdout is not a
//! terminal (an agent is reading), the output is captured and compacted
//! losslessly — ANSI stripped, blank runs collapsed, consecutive duplicate
//! lines folded with a repeat count — then bounded: the head and tail are kept
//! and the middle is teed to disk with a recovery hint. Interactive terminal
//! use keeps the original streaming passthrough.

use crate::core::utils::strip_ansi;

/// Result of [`compact`].
#[derive(Debug, PartialEq, Eq)]
pub struct Compacted {
    pub text: String,
    /// True when lines were dropped to honour `max_chars`. The caller must then
    /// attach a recovery hint or fall back to the uncapped text.
    pub truncated: bool,
}

/// Compact `raw` without losing information, then cap it at `max_chars`
/// (0 = no cap) keeping the head and the tail.
pub fn compact(raw: &str, max_chars: usize) -> Compacted {
    let lines = fold_lines(&strip_ansi(raw));
    let total_chars: usize = lines.iter().map(|l| l.len() + 1).sum();
    if max_chars == 0 || total_chars <= max_chars {
        return Compacted {
            text: join(&lines),
            truncated: false,
        };
    }

    // Errors and summaries live at the end: give the tail a real share.
    let head_budget = max_chars * 6 / 10;
    let tail_budget = max_chars - head_budget;

    let mut head_end = 0;
    let mut used = 0;
    for line in &lines {
        if used + line.len() + 1 > head_budget {
            break;
        }
        used += line.len() + 1;
        head_end += 1;
    }
    let mut tail_start = lines.len();
    let mut used = 0;
    while tail_start > head_end {
        let line = &lines[tail_start - 1];
        if used + line.len() + 1 > tail_budget {
            break;
        }
        used += line.len() + 1;
        tail_start -= 1;
    }
    let omitted = tail_start - head_end;
    if omitted == 0 {
        return Compacted {
            text: join(&lines),
            truncated: false,
        };
    }

    let mut kept: Vec<String> = lines[..head_end].to_vec();
    kept.push(format!("[rtk: {} lines omitted]", omitted));
    kept.extend_from_slice(&lines[tail_start..]);
    Compacted {
        text: join(&kept),
        truncated: true,
    }
}

/// Keep only the last `\r`-redraw of each line, trim trailing whitespace,
/// collapse blank runs to one blank line, and fold consecutive identical lines
/// into `line (×N)`.
fn fold_lines(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut last: Option<String> = None;
    let mut repeats = 0usize;

    let flush = |out: &mut Vec<String>, last: &mut Option<String>, repeats: &mut usize| {
        if let Some(line) = last.take() {
            if *repeats > 1 {
                out.push(format!("{} (×{})", line, repeats));
            } else {
                out.push(line);
            }
        }
        *repeats = 0;
    };

    for line in text.lines() {
        // A carriage return redraws the line: keep what a terminal would show.
        let line = line.rsplit('\r').next().unwrap_or(line).trim_end();
        if line.is_empty() {
            flush(&mut out, &mut last, &mut repeats);
            if !matches!(out.last().map(String::as_str), Some("") | None) {
                out.push(String::new());
            }
            continue;
        }
        if last.as_deref() == Some(line) {
            repeats += 1;
            continue;
        }
        flush(&mut out, &mut last, &mut repeats);
        last = Some(line.to_string());
        repeats = 1;
    }
    flush(&mut out, &mut last, &mut repeats);
    while matches!(out.last().map(String::as_str), Some("")) {
        out.pop();
    }
    out
}

fn join(lines: &[String]) -> String {
    let mut text = lines.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compact_strips_ansi_and_collapses_blank_runs() {
        let raw = "\x1b[32mok\x1b[0m   \n\n\n\nnext\n\n";
        let out = compact(raw, 0);
        assert_eq!(out.text, "ok\n\nnext\n");
        assert!(!out.truncated);
    }

    #[test]
    fn test_compact_folds_consecutive_duplicates_only() {
        let raw = "a\na\na\nb\na\n";
        assert_eq!(compact(raw, 0).text, "a (×3)\nb\na\n");
    }

    #[test]
    fn test_compact_keeps_head_and_tail_when_capped() {
        let raw: String = (1..=1000).map(|i| format!("line {i:04}\n")).collect();
        let out = compact(&raw, 1000);
        assert!(out.truncated);
        assert!(out.text.starts_with("line 0001\n"));
        assert!(out.text.ends_with("line 1000\n"));
        assert!(out.text.contains("[rtk: "));
        assert!(out.text.contains(" lines omitted]"));
        assert!(out.text.len() <= 1000 + 40, "{}", out.text.len());
        // Roughly 60/40 head/tail split.
        let head = out.text.split("[rtk:").next().unwrap().lines().count();
        let tail = out.text.split("omitted]\n").nth(1).unwrap().lines().count();
        assert!(head > tail && tail > 0, "head={head} tail={tail}");
    }

    #[test]
    fn test_compact_keeps_last_carriage_return_redraw() {
        assert_eq!(compact("10%\r20%\r100%\nok\n", 0).text, "100%\nok\n");
    }

    #[test]
    fn test_compact_under_cap_is_untouched() {
        let raw = "one\ntwo\n";
        assert_eq!(
            compact(raw, 8000),
            Compacted {
                text: raw.to_string(),
                truncated: false
            }
        );
        assert_eq!(compact("", 10).text, "");
    }

    #[test]
    fn test_compact_savings_on_progress_noise() {
        let mut raw = String::new();
        for _ in 0..400 {
            raw.push_str("\x1b[2K\rDownloading... 45%\n");
        }
        raw.push_str("done\n");
        let out = compact(&raw, 0);
        assert_eq!(out.text, "Downloading... 45% (×400)\ndone\n");
        assert!(out.text.len() * 10 < raw.len());
    }
}
