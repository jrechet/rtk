# Git and VCS

> Part of [`src/cmds/`](../README.md) — see also [docs/contributing/TECHNICAL.md](../../../docs/contributing/TECHNICAL.md)

## Specifics

- **git.rs** uses `trailing_var_arg = true` + `allow_hyphen_values = true` so native git flags (`--oneline`, `--cached`, etc.) pass through correctly
- Default `git status` uses `--porcelain -b` so the compact output never exceeds raw `git status` (an untracked directory collapses to a single line, matching git's default); branch/short-only flags reuse the compact path, other explicit args still pass through unchanged
- Global git options (`-C`, `--git-dir`, `--work-tree`, `--no-pager`) are prepended before the subcommand
- Exit code propagation is critical for CI/CD pipelines
- `git blame` injects `--date=short` and groups consecutive lines by commit (`compact_blame`): one `hash author date Lstart-end` header per run, code lines kept with their number, capped at `limits.read_max_lines` with a `git blame -L` resume hint. Machine formats (`--porcelain`, `-p`, `--incremental`) and header toggles (`-s`, `-n`, `-t`, `--date=`) pass through; any unparsed line falls back to raw
- Write subcommands (`switch`, `restore`, `merge`, `rebase`, `cherry-pick`, `revert`, `reset`, `tag`, `ls-files`) share small formatters: `format_history_op` (one `ok …` line with `diffstat_summary`, or `FAILED: … — N conflicts` plus the `CONFLICT`/`error` lines with git's `hint:` coaching dropped), `format_reset_output`, `format_tag_output`. `wants_terminal()` routes `-i/--interactive/-p/--patch/-e/--edit` to the raw passthrough so editors and prompts keep the terminal
- `git grep` is not in `git.rs`: `main.rs` routes it to `system/search.rs` as `Engine::GitGrep` (binary `git`, subcommand `grep`, git global options as prefix args). It forces `-n -H -I -z` and rewrites `path\0line\0content` to the shared `path\0line:content` shape; boolean expressions, context flags, `-p`/`-W`, layout flags and revision operands pass through
- **glab_cmd.rs** declares `-R`/`--repo` and `-g`/`--group` at the clap level; they are **appended** to the glab args (not prepended) so subcommand dispatch stays intact
- `has_output_flag()` short-circuits to passthrough when the user explicitly requests `-F` / `--output` / `--json` (avoids double JSON injection)
- `should_passthrough_view()` redirects `mr/issue view` to passthrough when `--web` or `--comments` is set
- JSON handlers use the local `run_glab_json<F>()` helper wrapping `runner::run_filtered` + `RunOptions::stdout_only().early_exit_on_failure().no_trailing_newline()`; on JSON parse error, falls back to the raw stdout (glab sometimes emits plain text for empty results)
- `ci status` uses text-keyword parsing (glab doesn't support `-F json` for this subcommand); when no English status keyword is recognized (non-English locale), returns raw verbatim
- `ci trace` uses ANSI-stripping + GitLab section-marker filtering + runner/git/artifact boilerplate removal; kept as text-only filter, not JSON
- `release list` falls back to raw output when the glab 1.82+ format doesn't match the legacy tab-delimited parser
- Pipeline / merge-status indicators use text tags (`[ok]`, `[fail]`, `[cancel]`, `[run]`, `[pend]`, `[skip]`, `[conflict]`) to match `gh_cmd.rs` and avoid multi-byte rendering quirks

## Cross-command

- `gh_cmd.rs` imports `compact_diff()` from `git.rs` for diff formatting; markdown helpers (`filter_markdown_body`, `filter_markdown_segment`) are defined in `gh_cmd.rs` itself
- `glab_cmd.rs` also uses `compact_diff()` from `git.rs` for `mr diff`; its `filter_markdown_body` is currently **duplicated** from `gh_cmd.rs` (shared-module refactor deferred)
- `diff_cmd.rs` is a standalone ultra-condensed diff (separate from `git diff`)

## glab vs gh JSON schema quick-ref

| Aspect | gh | glab |
|--------|----|------|
| Notation | `#42` | `!42` |
| States | `OPEN`/`MERGED`/`CLOSED` | `opened`/`merged`/`closed` |
| Author | `author.login` | `author.username` |
| URL field | `url` | `web_url` |
| Body field | `body` | `description` |
| Merge check | `mergeable` | `merge_status` (`can_be_merged` / `cannot_be_merged`) |
| CI status | `statusCheckRollup` | `head_pipeline.status` |
| Labels | `labels` (array of objects) | `labels` (array of strings) |
| Reviewers | `reviewRequests`/`reviews` | `reviewers` (array of objects with `username`) |
