# AGENTS.md

## Project Summary

`prism` is a Rust 2024 terminal application for running multiple commands against one shared byte stream. It captures
stdin (or one `--input` file) into a temporary append-only store, replays the complete captured prefix to every command
run, and continues streaming new bytes to each run independently. Commands render in horizontally arranged terminal
views, use a PTY by default, can opt into pipe-backed output, can reference shared single-line prompt editors, and use
an embedded or explicitly supplied YAML keymap.

Supported platforms are Linux and macOS. Windows is intentionally unsupported.

## Product Invariants

- Input is arbitrary bytes. Never assume UTF-8, lines, or finite input.
- Publish an input length only after the corresponding backing-file write succeeds.
- Every run starts replay at offset zero and receives each published byte exactly once.
- A blocked child stdin must not block capture or another view. Keep one independently blocking pump per run.
- Child fd 0 is always a pipe. In the default mode, fds 1 and 2 use the PTY slave and that slave is the controlling
  terminal. In `[no-tty]` mode, fds 1 and 2 share one output pipe and the child has no controlling terminal.
- Both output modes merge stdout and stderr into one immutable event stream for the owning view. Child output must
  never be written to prism's stdout.
- Commands execute directly through `PATH`; there is no implicit shell expansion or evaluation.
- Preserve close-on-exec flags on all unrelated pipe and PTY descriptors. Leaking a pipe writer into a child prevents
  stdin EOF and can hang views.
- Terminate the entire process group with SIGTERM, a roughly 100 ms grace period, then SIGKILL. Reap the direct child.
- Keep a process-group handle after the direct child exits because descendants may still exist and produce output.
- Every run and worker event has a monotonically increasing generation. Ignore output from stale generations.
- Invalid starred-editor syntax must leave the currently running view intact.
- Input EOF never exits the application.
- Child failures and nonzero exits are view-local and do not determine prism's final exit status.
- UI input and rendering use `/dev/tty`; stdin remains the data source.
- Restore raw mode, alternate screen, cursor, mouse capture, and bracketed paste before reporting fatal errors.
- Parse and validate a requested keymap configuration before opening the input source or starting input capture.

## Template Semantics

- View specs receive one POSIX-like lexical split at startup using `shell-words`.
- `[no-tty]` is recognized only as the first complete token of a view spec and is removed before command compilation.
- A view may be prefixed with `LABEL=`. Explicit labels must start with an ASCII letter or underscore and continue with
  ASCII alphanumeric characters, underscores, or hyphens; duplicate explicit labels are rejected.
- An unlabeled view uses the basename of its first command token as its title.
- `{name}` performs literal string substitution without changing argument boundaries.
- `{*name}` must occupy a complete token and is lexically split at runtime.
- `{{` and `}}` escape literal braces after view-spec tokenization.
- Editors are shared by name. `:<n>` claims a 1-based absolute editor display position.
- `=text` after the optional position initializes an editor. Repeated explicit defaults for one name must match.
- Repeated names may mix starred and unstarred uses because starring is usage-specific.
- Empty unstarred placeholders produce one empty argument; empty starred placeholders produce zero arguments.

## View TTY Modes

- PTY mode is the default. The child receives `TERM=xterm-256color`, stdout and stderr use the PTY slave, `/dev/tty` is
  available, and unmatched focused-view keys are written to the PTY master.
- A view beginning with `[no-tty]` uses a shared stdout/stderr pipe. The child still starts in its own session and
  process group, but has no controlling terminal; interactive focused-view input and resize ioctls are no-ops.
- Application resize events immediately recalculate pane dimensions. By default, each terminal model and child PTY
  follows its pane's inner dimensions.
- `--lock-tty-size` locks each PTY-backed view to its first valid pane size. `--lock-tty-size=COLUMNSxROWS` instead uses
  one explicit nonzero size for every PTY-backed view. Pipe-backed views ignore this option and keep their local terminal
  models synchronized with their panes.

## Source Map

- `src/main.rs`: runtime creation, top-level diagnostics, and exit status.
- `src/cli.rs`: Clap arguments, keymap loading, input validation/opening, TTY-size parsing, and view-label parsing.
- `src/template.rs`: per-view TTY-mode parsing, title selection, template compilation, editor discovery/ordering,
  expansion, and affected-view mapping.
- `src/editor.rs`: grapheme-aware single-line editing, paste normalization, and horizontal viewport state.
- `src/input_store.rs`: temporary backing store, capture thread, condition variable, and per-run replay pumps.
- `src/pty.rs`: PTY or output-pipe spawn, descriptor setup, interactive writes, resize, worker reader, and process
  cleanup.
- `src/terminal_model.rs`: bounded `vt100` model, scrollback, ANSI cell conversion, and cursor rendering.
- `src/view.rs`: per-view generation, process, terminal model, TTY-size policy, and run state.
- `src/keymap.rs`: YAML keymap loading, action deserialization, and application shortcut normalization and dispatch through
  `mkutils::KeyMapSession`.
- `src/default-config.yaml`: embedded default application keymap.
- `src/focus.rs`: traversal across views and editors.
- `src/app.rs`: Tokio event loop, resize-event propagation, debounce, restarts, signals, input routing, and generation
  filtering. It also retains the latest key or mouse event for the optional debug display.
- `src/ui.rs`: equal-width layout and resize calculation, editor blocks, focus styling, mouse areas, optional bottom
  debug bar, and too-small fallback.
- `src/terminal.rs`: `/dev/tty` Ratatui backend and RAII restoration.
- `src/event.rs`: immutable worker-to-app messages.

Keep terminal-emulator and UI state mutation on the main event-loop task. Worker threads should send immutable events
instead of sharing the terminal parser behind a lock.

## Keybindings

Application shortcuts must remain represented and dispatched through `mkutils` keybinding types. Do not replace them
with a direct monolithic match over Crossterm key events.

`--config-file FILE` reads a top-level YAML `key_map` list in the same shape as `mkxl/ftg`. A supplied keymap fully
replaces `src/default-config.yaml`; it is not merged with the defaults. Binding actions deserialize from snake-case
`command` values. Key expressions use mkutils' lowercase names and `ctrl`, `shift`, `alt`, or `super` modifiers.

The embedded defaults are:

- `Tab`: focus next
- `Shift+Tab`: focus previous
- `Ctrl+]`: leave a view for one of its editors
- `Ctrl+Q`: orderly quit
- `Ctrl+R`: immediate manual restart
- `Ctrl+G`: toggle the bottom input-event debug bar
- `End`: return a focused view to live-follow mode; it remains an editor movement key when an editor is focused

Unmatched keys in a PTY-backed view, including `Ctrl+C`, are encoded and written to the PTY master. A `[no-tty]` view
has no interactive input channel, so these writes are ignored. Mouse events remain local.

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

Tests include real Unix PTY and pipe integration coverage. They verify fd TTY identity, PTY and no-TTY `/dev/tty`
behavior, merged stdout/stderr, binary replay, active-capture restart, isolated backpressure, ANSI parsing, interactive
keys, keymap configuration, application and child resize behavior, TTY-size locking, and process-group termination. CI
runs the suite on both Ubuntu and macOS via `.github/workflows/ci.yml`.

For a local end-to-end TUI smoke test without consuming the shell's current terminal, `script(1)` can allocate a pseudo
terminal. Ensure the command has piped data and send `Ctrl+Q` through the pseudo terminal.

## Documentation Maintenance

- Every code change made by an agent must update both `AGENTS.md` and `README.md` in the same change, even when the code
  change appears internal or does not alter the CLI.
- The updates must describe the resulting codebase, not merely record that files changed. Keep user-facing behavior and
  examples in `README.md`; keep architecture, invariants, source ownership, and agent workflow in `AGENTS.md`.
- Before finishing any code change, compare both documents with the implementation and remove or correct stale claims.

## Change Guidance

- Prefer the smallest change that preserves the invariants above.
- Treat descriptor ownership and failure paths as lifecycle-critical. Any error after spawning must still kill and reap
  the child group.
- Avoid holding the input-store mutex while doing file or pipe I/O.
- Do not clear a view until command and editor validation succeeds.
- Allocate/invalidate the new generation before stopping an old run.
- Keep output visible after child exit and do not restart merely because new input arrived.
- Keep `[no-tty]` as a per-view first-token marker; do not turn it into a global process mode without a deliberate
  product-spec change.
- Do not add an implicit shell, input-size cap, multiline editor, child mouse forwarding, or stdout export without a
  deliberate product-spec change.
