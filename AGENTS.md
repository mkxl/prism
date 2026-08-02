# AGENTS.md

## Project Summary

`prism` is a Rust 2024 terminal application for running multiple commands against one shared byte stream. It captures
stdin (or one `--input` file) into a temporary append-only store, replays the complete captured prefix to every command
run, and continues streaming new bytes to each run independently. Commands render in horizontally arranged PTY-backed
views and can reference shared single-line prompt editors.

Supported platforms are Linux and macOS. Windows is intentionally unsupported.

## Product Invariants

- Input is arbitrary bytes. Never assume UTF-8, lines, or finite input.
- Publish an input length only after the corresponding backing-file write succeeds.
- Every run starts replay at offset zero and receives each published byte exactly once.
- A blocked child stdin must not block capture or another view. Keep one independently blocking pump per run.
- Child fd 0 is a pipe, while fds 1 and 2 use the PTY slave. The PTY slave is also the controlling terminal.
- Commands execute directly through `PATH`; there is no implicit shell expansion or evaluation.
- Preserve close-on-exec flags on all unrelated pipe and PTY descriptors. Leaking a pipe writer into a child prevents
  stdin EOF and can hang views.
- Terminate the entire process group with SIGTERM, a roughly 100 ms grace period, then SIGKILL. Reap the direct child.
- Keep a process-group handle after the direct child exits because descendants may still exist and produce output.
- Every run and worker event has a monotonically increasing generation. Ignore output from stale generations.
- Invalid starred-editor syntax must leave the currently running view intact.
- Input EOF never exits the application.
- Child failures and nonzero exits are view-local and do not determine prism's final exit status.
- UI input and rendering use `/dev/tty`; stdin remains the data source. Child output must never be written to prism's
  stdout.
- Restore raw mode, alternate screen, cursor, mouse capture, and bracketed paste before reporting fatal errors.

## Template Semantics

- View specs receive one POSIX-like lexical split at startup using `shell-words`.
- `{name}` performs literal string substitution without changing argument boundaries.
- `{*name}` must occupy a complete token and is lexically split at runtime.
- `{{` and `}}` escape literal braces after view-spec tokenization.
- Editors are shared by name. `:<n>` claims a 1-based absolute editor display position.
- `=text` after the optional position initializes an editor. Repeated explicit defaults for one name must match.
- Repeated names may mix starred and unstarred uses because starring is usage-specific.
- Empty unstarred placeholders produce one empty argument; empty starred placeholders produce zero arguments.

## Source Map

- `src/main.rs`: runtime creation, top-level diagnostics, and exit status.
- `src/cli.rs`: Clap arguments, input validation/opening, and view-label parsing.
- `src/template.rs`: template compilation, editor discovery/ordering, expansion, and affected-view mapping.
- `src/editor.rs`: grapheme-aware single-line editing, paste normalization, and horizontal viewport state.
- `src/input_store.rs`: temporary backing store, capture thread, condition variable, and per-run replay pumps.
- `src/pty.rs`: hybrid PTY spawn, descriptor setup, interactive writes, resize, worker reader, and process cleanup.
- `src/terminal_model.rs`: bounded `vt100` model, scrollback, ANSI cell conversion, and cursor rendering.
- `src/view.rs`: per-view generation, process, terminal model, and run state.
- `src/keymap.rs`: all application-level shortcuts through `mkutils::KeyMapSession`.
- `src/focus.rs`: traversal across views and editors.
- `src/app.rs`: Tokio event loop, debounce, restarts, signals, input routing, and generation filtering.
- `src/ui.rs`: equal-width layout, editor blocks, focus styling, mouse areas, and too-small fallback.
- `src/terminal.rs`: `/dev/tty` Ratatui backend and RAII restoration.
- `src/event.rs`: immutable worker-to-app messages.

Keep terminal-emulator and UI state mutation on the main event-loop task. Worker threads should send immutable events
instead of sharing the terminal parser behind a lock.

## Keybindings

Application shortcuts must remain represented and dispatched through `mkutils` keybinding types. Do not replace them
with a direct monolithic match over Crossterm key events.

- `Tab`: focus next
- `Shift+Tab`: focus previous
- `Ctrl+]`: leave a view for one of its editors
- `Ctrl+Q`: orderly quit
- `Ctrl+R`: immediate manual restart
- `End`: return a focused view to live-follow mode; it remains an editor movement key when an editor is focused

Unmatched view keys, including `Ctrl+C`, are encoded and written to the PTY master. Mouse events remain local.

## Dependency Notes

The required `mkutils 0.1.193` crates.io release has publishing assumptions not satisfied by unpatched transitive
crates:

- Its key trie needs the pinned Crossterm revision's `KeyEvent: Ord` implementation.
- Its TUI feature uses `ChunkedSource` from the pinned Tree-sitter fork.

The `[patch.crates-io]` entries in `Cargo.toml` match the revisions used by `mkutils` itself. Do not remove or update
them without first proving that `mkutils` compiles and its keymap tests pass against replacement releases.

## Verification

The repository denies Rust warnings and Clippy `all`, `cargo`, `nursery`, and `pedantic` lints. Before finishing changes,
run:

```sh
cargo fmt --all --check
cargo test --all-targets
cargo clippy --all-targets --all-features
cargo build --release
git diff --check
```

Tests include real Unix PTY integration coverage. They verify fd TTY identity, `/dev/tty`, merged stdout/stderr, binary
replay, active-capture restart, isolated backpressure, ANSI parsing, interactive keys, resize, and process-group
termination. CI runs the suite on both Ubuntu and macOS via `.github/workflows/ci.yml`.

For a local end-to-end TUI smoke test without consuming the shell's current terminal, `script(1)` can allocate a pseudo
terminal. Ensure the command has piped data and send `Ctrl+Q` through the pseudo terminal.

## Change Guidance

- Prefer the smallest change that preserves the invariants above.
- Treat descriptor ownership and failure paths as lifecycle-critical. Any error after spawning must still kill and reap
  the child group.
- Avoid holding the input-store mutex while doing file or pipe I/O.
- Do not clear a view until command and editor validation succeeds.
- Allocate/invalidate the new generation before stopping an old run.
- Keep output visible after child exit and do not restart merely because new input arrived.
- Do not add an implicit shell, input-size cap, multiline editor, child mouse forwarding, or stdout export without a
  deliberate product-spec change.
