# prism

`prism` is an interactive terminal application that runs multiple commands against one captured byte stream. Every run
receives the complete stream from byte zero through its own stdin pipe. Views use a PTY by default, may individually use
pipe-backed output instead, and can be parameterized by shared single-line prompt editors.

```sh
producer | prism \
  input=fx \
  output='jq {*flags:1=--compact-output} {filter:2=.items[]}' \
  count='[no-tty] wc -c'
```

Use `--input FILE` instead of a pipeline to read a file once. Input is captured as arbitrary bytes in an append-only
temporary store. Each run replays the captured prefix and then receives newly published bytes independently; input EOF
does not exit the application.

## View Specifications

Each positional view has the form `[LABEL=]SPEC`. View specifications receive one shell-like lexical split, but commands
are executed directly without an implicit shell. Explicit labels must begin with a letter or underscore and may contain
letters, digits, underscores, and hyphens; explicit labels must also be unique. A view without a label uses its command's
basename as its title.

`{name}` substitutes one argument value, while a standalone `{*name}` splits the editor value into zero or more
arguments. Add initial editor text with `{name=default}` or `{name:2=default}`; the optional `:2` sets the editor's display
position. Keep a placeholder quoted within the view specification when its default contains spaces, for example
`output='jq "{query=.items | length}"'`. Shared occurrences of an editor may omit the default or repeat the same one, but
conflicting defaults are rejected. Use `{{` and `}}` for literal braces.

## Terminal Modes

PTY-backed views are the default. Their stdout and stderr share a PTY, `/dev/tty` is available, focused-view keystrokes
are delivered through the PTY master, and application resizes update both the terminal model and the child PTY.

Start an individual view specification with the complete token `[no-tty]` to capture that child's merged stdout and
stderr through a pipe, for example `output='[no-tty] jq .'`. The marker is removed before execution. In this mode the
child has no controlling terminal, `/dev/tty` is unavailable, and focused-view keystrokes are not delivered. Its local
terminal model still follows the view's dimensions for rendering.

By default, each PTY-backed child terminal follows its view's dimensions. Use `--lock-tty-size` to keep each child at its
initial view size, or `--lock-tty-size=80x24` to use an explicit `COLUMNSxROWS` size for every PTY-backed view. This
option does not lock pipe-backed views.

## Keys

| Key | Action |
| --- | --- |
| `Tab` / `Shift+Tab` | Move focus |
| `Ctrl+]` | Leave a focused view |
| `Ctrl+R` | Restart the focused view or editor's views |
| `End` | Follow live output in the focused view |
| `Ctrl+Q` | Quit |

While a PTY-backed view is focused, other keys, including `Ctrl+C`, are sent to its controlling terminal. Mouse events
remain local to prism.
