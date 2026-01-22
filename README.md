# gest

Jest-like terminal test runner for Go projects. gest watches your repo, runs
tests in parallel, and keeps a focused list of failures while still surfacing
subtests and panic output.

## Features

- TUI list with status colors and detailed output view
- Modes: all tests, only failing tests, selected tests with fuzzy typeahead
- Leaf-only subtest display (parents hidden)
- Package-aware file watching to rerun only what changed
- Panic output captured per test
- Repo-local cache in `.gest/state.json`

## Install

From the repo root:

```bash
cargo build --release
```

The binary will be at `target/release/gest`.

Release builds enable LTO, single codegen unit, and `panic=abort` for smaller
and faster binaries.

## Usage

Run inside a Go module. gest auto-detects the nearest `go.mod`.

```bash
gest
```

### CLI flags

- `--mode <all|failing|select>`: initial mode (default: `all`)
- `--pkg-concurrency <n>`: max parallel packages (default: CPU count)
- `--sequential`: set `--pkg-concurrency=1` and `go test -p=1`
- `--no-watch`: disable file watching
- `--no-test-cache`: disable Go test cache (`-count=1`)
- `--packages <regex>`: filter packages by import path
- `--debug`: enable debug logging (reserved)

## Keybindings

General list view:

- `a`: all mode
- `o`: only failing mode
- `p`: select mode
- `r`: rerun selected test
- `x`: remove selected test from failing/selected list
- `Enter`: toggle output pane
- `→`: open output pane
- `←`: close output pane
- `↑/↓`: move selection
- `q`: quit

Select mode:

- Type to filter (fuzzy)
- `Enter`/`Space`: toggle selection
- `p` or `Esc`: finish selection and run selected tests
- `↑/↓`: move selection

## Modes

- **All**: runs every package. Failing tests are shown first.
- **Only failing**: only failing tests re-run on file changes. Passing tests
  that were previously failing remain visible until removed.
- **Selected**: runs only chosen tests. File changes rerun selected tests.

## Notes

- Tests run with `go test -json` and use the Go cache by default.
- Use `--no-test-cache` to add `-count=1` and disable caching.
- Package-level harness output (`PASS`, `FAIL`, `ok ...`) is ignored.
- Panic output is attached to the test that emitted it.

## Release

See `docs/RELEASE.md` for the automated release process.

## Status

gest is early-stage and focused on fast local feedback loops. If you hit a case
where output attribution is wrong, please provide a `go test -json` snippet.
