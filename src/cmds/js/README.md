# JavaScript / TypeScript / Node

> Part of [`src/cmds/`](../README.md) — see also [docs/contributing/TECHNICAL.md](../../../docs/contributing/TECHNICAL.md)

## Specifics

- `utils::package_manager_exec()` auto-detects pnpm/yarn/npm -- JS modules should use this instead of hardcoding a package manager
- `lint_cmd.rs` is a cross-ecosystem router: detects Python projects and delegates to `mypy_cmd` or `ruff_cmd`
- `vitest_cmd.rs` uses the `parser/` module for structured output parsing
- `playwright_cmd.rs` uses the `parser/` module for test result extraction
- `yarn_cmd.rs` handles classic (`[1/4]` steps, `$` echo, `Done in`) and berry (`➤ YNxxxx` codes: `YN0000` dropped, errors and `YN0013` kept, the rest capped as warnings); the transitive `info All dependencies` tree is dropped
- `bun_cmd.rs` has two filters: a generic one (install/add/run/x, `+ pkg` list capped at `CAP_LIST`) and `filter_bun_test` (failures only with assertion, values and two frames, one summary line). Test result lines come as `(pass)`/`(fail)` without a TTY and `✓`/`✗` with one; both are handled
- Rewrite rules for yarn/bun/bunx sit *before* the JS tool rules in `rules.rs` so the last-match resolution sends `yarn vitest`, `bun x tsc`, `bunx prettier` to the tool filters; streaming scripts (`dev`, `start`, ...) are not rewritten

## Cross-command

- `lint_cmd` routes to `cmds/python/mypy_cmd` and `cmds/python/ruff_cmd` for Python projects
- `prettier_cmd` is also called by `cmds/system/format_cmd` as a format dispatcher target
