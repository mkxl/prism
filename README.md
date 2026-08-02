# prism

`prism` is an interactive terminal application that runs multiple commands against one captured byte stream. Every
view receives the same live input, renders through its own PTY, and can be parameterized by shared single-line prompt
editors.

```sh
producer | prism \
  input=fx \
  output='jq {*flags:1=--compact-output} {filter:2=.items[]}'
```

Use `--input FILE` instead of a pipeline to read a file once. View specifications use shell-like quoting only for
lexical splitting; commands are executed directly without an implicit shell. `{name}` substitutes one argument value,
while a standalone `{*name}` splits the editor value into zero or more arguments. Add initial editor text with
`{name=default}` or `{name:2=default}`; the optional `:2` sets the editor's display position. Keep a placeholder quoted
within the view specification when its default contains spaces, for example `output='jq "{query=.items | length}"'`.
Shared occurrences of an editor may omit the default or repeat the same one, but conflicting defaults are rejected. Use
`{{` and `}}` for literal braces.

| Key | Action |
| --- | --- |
| `Tab` / `Shift+Tab` | Move focus |
| `Ctrl+]` | Leave a focused view |
| `Ctrl+R` | Restart the focused view or editor's views |
| `End` | Follow live output in the focused view |
| `Ctrl+Q` | Quit |

While a view is focused, other keys, including `Ctrl+C`, are sent to its controlling terminal.
