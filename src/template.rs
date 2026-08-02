use crate::cli::parse_view_arguments;
use anyhow::{Context, Result, bail};
use std::{collections::HashMap, path::Path};

pub type EditorId = usize;
pub type ViewId = usize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorDefinition {
    pub id: EditorId,
    pub name: String,
    pub initial_text: String,
    pub explicitly_ordered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Placeholder(EditorId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenTemplate {
    Joined(Vec<Segment>),
    Starred(EditorId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandTemplate {
    tokens: Vec<TokenTemplate>,
}

impl CommandTemplate {
    pub fn expand(&self, editor_values: &[String]) -> Result<Vec<String>, ExpansionError> {
        let mut arguments = Vec::new();
        for token in &self.tokens {
            match token {
                TokenTemplate::Joined(segments) => {
                    let mut argument = String::new();
                    for segment in segments {
                        match segment {
                            Segment::Literal(literal) => argument.push_str(literal),
                            Segment::Placeholder(editor) => argument.push_str(&editor_values[*editor]),
                        }
                    }
                    arguments.push(argument);
                }
                TokenTemplate::Starred(editor) => {
                    let value = &editor_values[*editor];
                    let expanded = shell_words::split(value).map_err(|error| ExpansionError::InvalidEditor {
                        editor: *editor,
                        message: error.to_string(),
                    })?;
                    arguments.extend(expanded);
                }
            }
        }

        if arguments.first().is_none_or(String::is_empty) {
            return Err(ExpansionError::EmptyCommand);
        }
        Ok(arguments)
    }

    pub fn starred_editors(&self) -> impl Iterator<Item = EditorId> + '_ {
        self.tokens.iter().filter_map(|token| match token {
            TokenTemplate::Starred(editor) => Some(*editor),
            TokenTemplate::Joined(_) => None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpansionError {
    InvalidEditor { editor: EditorId, message: String },
    EmptyCommand,
}

impl std::fmt::Display for ExpansionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidEditor { message, .. } => write!(formatter, "{message}"),
            Self::EmptyCommand => formatter.write_str("command expands to an empty executable"),
        }
    }
}

impl std::error::Error for ExpansionError {}

#[derive(Debug, Clone)]
pub struct ViewDefinition {
    pub id: ViewId,
    pub label: String,
    pub command: CommandTemplate,
    pub referenced_editors: Vec<EditorId>,
    pub use_pty: bool,
}

#[derive(Debug, Clone)]
pub struct Configuration {
    pub views: Vec<ViewDefinition>,
    pub editors: Vec<EditorDefinition>,
    pub editor_order: Vec<EditorId>,
    pub affected_views: Vec<Vec<ViewId>>,
}

#[derive(Debug)]
struct DiscoveredEditor {
    name: String,
    ordering: Option<usize>,
    initial_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedPlaceholder {
    name: String,
    starred: bool,
    ordering: Option<usize>,
    initial_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RawSegment {
    Literal(String),
    Placeholder(ParsedPlaceholder),
}

impl Configuration {
    pub fn parse(arguments: &[String]) -> Result<Self> {
        let view_arguments = parse_view_arguments(arguments)?;
        let mut names = HashMap::<String, EditorId>::new();
        let mut discovered = Vec::<DiscoveredEditor>::new();
        let mut views = Vec::with_capacity(view_arguments.len());

        for (view_id, view_argument) in view_arguments.into_iter().enumerate() {
            let mut words = shell_words::split(&view_argument.spec)
                .with_context(|| format!("invalid view specification for {:?}", view_argument.label))?;
            let use_pty = if words.first().is_some_and(|word| word == "[no-tty]") {
                words.remove(0);
                false
            } else {
                true
            };
            if words.is_empty() {
                bail!("view {:?} has an empty specification", view_argument.label);
            }
            let label = if view_argument.explicit_label {
                view_argument.label
            } else {
                Path::new(&words[0])
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(&words[0])
                    .to_owned()
            };

            let mut referenced_editors = Vec::new();
            let mut command_tokens = Vec::with_capacity(words.len());
            for word in words {
                let raw_segments = parse_token(&word);
                let mut segments = Vec::with_capacity(raw_segments.len());
                let mut starred_editor = None;
                for raw_segment in raw_segments {
                    match raw_segment {
                        RawSegment::Literal(literal) => segments.push(Segment::Literal(literal)),
                        RawSegment::Placeholder(placeholder) => {
                            let editor = register_editor(&placeholder, &mut names, &mut discovered)?;
                            if !referenced_editors.contains(&editor) {
                                referenced_editors.push(editor);
                            }
                            if placeholder.starred {
                                starred_editor = Some(editor);
                            }
                            segments.push(Segment::Placeholder(editor));
                        }
                    }
                }

                if let Some(editor) = starred_editor {
                    if segments.len() != 1 || !matches!(segments[0], Segment::Placeholder(_)) {
                        bail!("starred placeholder in view {label:?} must occupy an entire token");
                    }
                    command_tokens.push(TokenTemplate::Starred(editor));
                } else {
                    command_tokens.push(TokenTemplate::Joined(segments));
                }
            }

            views.push(ViewDefinition {
                id: view_id,
                label,
                command: CommandTemplate { tokens: command_tokens },
                referenced_editors,
                use_pty,
            });
        }

        let editor_order = order_editors(&discovered)?;
        let editors = discovered
            .into_iter()
            .enumerate()
            .map(|(id, editor)| EditorDefinition {
                id,
                name: editor.name,
                initial_text: editor.initial_text.unwrap_or_default(),
                explicitly_ordered: editor.ordering.is_some(),
            })
            .collect::<Vec<_>>();
        let mut affected_views = vec![Vec::new(); editors.len()];
        for view in &views {
            for &editor in &view.referenced_editors {
                affected_views[editor].push(view.id);
            }
        }

        Ok(Self {
            views,
            editors,
            editor_order,
            affected_views,
        })
    }
}

fn register_editor(
    placeholder: &ParsedPlaceholder,
    names: &mut HashMap<String, EditorId>,
    discovered: &mut Vec<DiscoveredEditor>,
) -> Result<EditorId> {
    if let Some(&id) = names.get(&placeholder.name) {
        let editor = &mut discovered[id];
        if let (Some(previous), Some(current)) = (editor.ordering, placeholder.ordering)
            && previous != current
        {
            bail!(
                "editor {:?} has conflicting positions {previous} and {current}",
                placeholder.name
            );
        }
        if editor.ordering.is_none() {
            editor.ordering = placeholder.ordering;
        }
        if let (Some(previous), Some(current)) = (&editor.initial_text, &placeholder.initial_text)
            && previous != current
        {
            bail!("editor {:?} has conflicting defaults", placeholder.name);
        }
        if editor.initial_text.is_none() {
            editor.initial_text.clone_from(&placeholder.initial_text);
        }
        return Ok(id);
    }

    let id = discovered.len();
    names.insert(placeholder.name.clone(), id);
    discovered.push(DiscoveredEditor {
        name: placeholder.name.clone(),
        ordering: placeholder.ordering,
        initial_text: placeholder.initial_text.clone(),
    });
    Ok(id)
}

fn order_editors(discovered: &[DiscoveredEditor]) -> Result<Vec<EditorId>> {
    let count = discovered.len();
    let mut slots = vec![None::<usize>; count];
    for (id, editor) in discovered.iter().enumerate() {
        if let Some(ordering) = editor.ordering {
            if ordering == 0 || ordering > count {
                bail!("editor {:?} position {ordering} is outside 1..={count}", editor.name);
            }
            let slot = &mut slots[ordering - 1];
            if let Some(other_id) = slot {
                bail!(
                    "editors {:?} and {:?} both claim position {ordering}",
                    discovered[*other_id].name,
                    editor.name
                );
            }
            *slot = Some(id);
        }
    }

    let unordered = discovered
        .iter()
        .enumerate()
        .filter_map(|(id, editor)| editor.ordering.is_none().then_some(id));
    let mut unordered = unordered.into_iter();
    for slot in &mut slots {
        if slot.is_none() {
            *slot = unordered.next();
        }
    }
    Ok(slots.into_iter().flatten().collect())
}

fn parse_token(token: &str) -> Vec<RawSegment> {
    let mut segments = Vec::new();
    let mut literal = String::new();
    let mut index = 0;

    while index < token.len() {
        let remaining = &token[index..];
        if remaining.starts_with("{{") {
            literal.push('{');
            index += 2;
        } else if remaining.starts_with("}}") {
            literal.push('}');
            index += 2;
        } else if remaining.starts_with('{') {
            if let Some(close) = placeholder_close(remaining)
                && let Some(placeholder) = parse_placeholder(&remaining[1..close])
            {
                if !literal.is_empty() {
                    segments.push(RawSegment::Literal(std::mem::take(&mut literal)));
                }
                segments.push(RawSegment::Placeholder(placeholder));
                index += close + 1;
                continue;
            }
            literal.push('{');
            index += 1;
        } else {
            let character = remaining.chars().next().expect("nonempty remaining token");
            literal.push(character);
            index += character.len_utf8();
        }
    }

    if !literal.is_empty() || segments.is_empty() {
        segments.push(RawSegment::Literal(literal));
    }
    segments
}

fn placeholder_close(remaining: &str) -> Option<usize> {
    let mut index = 1;
    let mut in_default = false;
    let mut fallback_close = None;
    while index < remaining.len() {
        let rest = &remaining[index..];
        if !in_default && rest.starts_with('=') {
            in_default = true;
            index += 1;
        } else if in_default && rest.starts_with("{{") {
            index += 2;
        } else if rest.starts_with('}') {
            if in_default {
                let braces = rest.bytes().take_while(|&byte| byte == b'}').count();
                if braces % 2 == 1 {
                    return Some(index + braces - 1);
                }
                fallback_close.get_or_insert(index);
                index += braces;
                continue;
            }
            return Some(index);
        } else {
            index += rest.chars().next().expect("nonempty placeholder").len_utf8();
        }
    }
    fallback_close
}

fn parse_placeholder(content: &str) -> Option<ParsedPlaceholder> {
    let (starred, content) = content.strip_prefix('*').map_or((false, content), |rest| (true, rest));
    let (content, initial_text) = content.split_once('=').map_or((content, None), |(prefix, default)| {
        (prefix, Some(unescape_braces(default)))
    });
    let (name, ordering) = content
        .split_once(':')
        .map_or((content, None), |(name, ordering)| (name, Some(ordering)));
    if !is_valid_name(name) {
        return None;
    }
    let ordering = match ordering {
        Some(value) if !value.starts_with('0') => Some(value.parse::<usize>().ok()?),
        Some(_) => return None,
        None => None,
    };
    Some(ParsedPlaceholder {
        name: name.to_owned(),
        starred,
        ordering,
        initial_text,
    })
}

fn unescape_braces(value: &str) -> String {
    let mut unescaped = String::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        let remaining = &value[index..];
        if remaining.starts_with("{{") {
            unescaped.push('{');
            index += 2;
        } else if remaining.starts_with("}}") {
            unescaped.push('}');
            index += 2;
        } else {
            let character = remaining.chars().next().expect("nonempty default text");
            unescaped.push(character);
            index += character.len_utf8();
        }
    }
    unescaped
}

fn is_valid_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::literal_string_with_formatting_args)]

    use super::*;

    fn configuration(specs: &[&str]) -> Configuration {
        Configuration::parse(&specs.iter().map(ToString::to_string).collect::<Vec<_>>()).unwrap()
    }

    fn values(configuration: &Configuration, entries: &[(&str, &str)]) -> Vec<String> {
        let mut values = configuration
            .editors
            .iter()
            .map(|editor| editor.initial_text.clone())
            .collect::<Vec<_>>();
        for (name, value) in entries {
            let id = configuration
                .editors
                .iter()
                .find(|editor| editor.name == *name)
                .unwrap()
                .id;
            values[id] = (*value).to_owned();
        }
        values
    }

    #[test]
    fn lexes_view_specs_without_shell_evaluation() {
        let configuration = configuration(&[r#"program "one two" 'three' |"#]);
        let arguments = configuration.views[0].command.expand(&[]).unwrap();
        assert_eq!(arguments, ["program", "one two", "three", "|"]);
    }

    #[test]
    fn uses_command_name_for_implicit_view_labels() {
        let configuration = configuration(&["[no-tty] /usr/bin/program argument", "custom=other argument"]);
        assert_eq!(configuration.views[0].label, "program");
        assert_eq!(configuration.views[1].label, "custom");
    }

    #[test]
    fn configures_no_tty_per_view() {
        let configuration = configuration(&["terminal=one argument", "piped=[no-tty] two argument"]);
        assert!(configuration.views[0].use_pty);
        assert!(!configuration.views[1].use_pty);
        assert_eq!(configuration.views[1].command.expand(&[]).unwrap(), ["two", "argument"]);
    }

    #[test]
    fn rejects_no_tty_marker_without_a_command() {
        let error = Configuration::parse(&["[no-tty]".to_owned()]).unwrap_err();
        assert!(error.to_string().contains("empty specification"));
    }

    #[test]
    fn rejects_bad_view_quotes() {
        let error = Configuration::parse(&["program 'unterminated".to_owned()]).unwrap_err();
        assert!(error.to_string().contains("invalid view specification"));
    }

    #[test]
    fn expands_unstarred_and_escaped_braces() {
        let configuration =
            configuration(&[r#"program --query={query} {empty} "{{foo: .foo}}" --range={start}:{end}"#]);
        let arguments = configuration.views[0]
            .command
            .expand(&values(
                &configuration,
                &[("query", "hello world"), ("start", "1"), ("end", "3")],
            ))
            .unwrap();
        assert_eq!(
            arguments,
            ["program", "--query=hello world", "", "{foo: .foo}", "--range=1:3"]
        );
    }

    #[test]
    fn initializes_editors_and_expands_their_defaults() {
        let configuration = configuration(&[
            r#"program "{query=hello world}" {punctuation:1=a:b=c} {braces={{value}}} "{*args=--name 'Jane Doe'}""#,
        ]);
        let initial_text = configuration
            .editors
            .iter()
            .map(|editor| (editor.name.as_str(), editor.initial_text.as_str()))
            .collect::<HashMap<_, _>>();
        assert_eq!(initial_text["query"], "hello world");
        assert_eq!(initial_text["punctuation"], "a:b=c");
        assert_eq!(initial_text["braces"], "{value}");
        assert_eq!(initial_text["args"], "--name 'Jane Doe'");
        assert!(configuration.editors[1].explicitly_ordered);

        let arguments = configuration.views[0]
            .command
            .expand(&values(&configuration, &[]))
            .unwrap();
        assert_eq!(
            arguments,
            ["program", "hello world", "a:b=c", "{value}", "--name", "Jane Doe"]
        );
    }

    #[test]
    fn merges_matching_defaults_for_shared_editors() {
        let config = configuration(&["one {shared}", "two {*shared=seed}", "three {shared=seed}"]);
        assert_eq!(config.editors.len(), 1);
        assert_eq!(config.editors[0].initial_text, "seed");

        let explicit_empty = configuration(&["one {shared}", "two {shared=}"]);
        assert!(explicit_empty.editors[0].initial_text.is_empty());
    }

    #[test]
    fn rejects_conflicting_defaults_for_shared_editors() {
        let error =
            Configuration::parse(&["one {shared=first}".to_owned(), "two {shared=second}".to_owned()]).unwrap_err();
        assert!(error.to_string().contains("conflicting defaults"));

        let error = Configuration::parse(&["one {shared=}".to_owned(), "two {shared=second}".to_owned()]).unwrap_err();
        assert!(error.to_string().contains("conflicting defaults"));
    }

    #[test]
    fn treats_separators_after_the_equals_sign_as_default_text() {
        let configuration = configuration(&["program {value=one:2=three}"]);
        assert_eq!(configuration.editors[0].initial_text, "one:2=three");
        assert!(!configuration.editors[0].explicitly_ordered);
    }

    #[test]
    fn unescapes_closing_braces_within_default_text() {
        let configuration = configuration(&["program {value=before}}after}"]);
        assert_eq!(configuration.editors[0].initial_text, "before}after");
    }

    #[test]
    fn starred_placeholder_splits_zero_or_many_arguments() {
        let configuration = configuration(&["program {*args} tail"]);
        assert_eq!(
            configuration.views[0]
                .command
                .expand(&values(&configuration, &[("args", "")]))
                .unwrap(),
            ["program", "tail"]
        );
        assert_eq!(
            configuration.views[0]
                .command
                .expand(&values(&configuration, &[("args", r#"--name "Jane Doe" --empty ''"#)]))
                .unwrap(),
            ["program", "--name", "Jane Doe", "--empty", "", "tail"]
        );
    }

    #[test]
    fn reports_starred_quote_errors_against_editor() {
        let configuration = configuration(&["program {*args}"]);
        let error = configuration.views[0]
            .command
            .expand(&values(&configuration, &[("args", "'bad")]))
            .unwrap_err();
        assert!(matches!(error, ExpansionError::InvalidEditor { editor: 0, .. }));
    }

    #[test]
    fn rejects_embedded_starred_placeholders() {
        let error = Configuration::parse(&["program --flags={*flags}".to_owned()]).unwrap_err();
        assert!(error.to_string().contains("must occupy an entire token"));
    }

    #[test]
    fn shares_repeated_editors_and_maps_views() {
        let configuration = configuration(&["one {shared} {shared}", "two {*shared} {other}"]);
        assert_eq!(configuration.editors.len(), 2);
        assert_eq!(configuration.views[0].referenced_editors, [0]);
        assert_eq!(configuration.affected_views[0], [0, 1]);
        assert_eq!(configuration.affected_views[1], [1]);
    }

    #[test]
    fn orders_editors_and_fills_gaps_by_appearance() {
        let configuration = configuration(&["cmd {first} {second:1} {third}"]);
        let names = configuration
            .editor_order
            .iter()
            .map(|&id| configuration.editors[id].name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["second", "first", "third"]);
    }

    #[test]
    fn rejects_invalid_editor_orderings() {
        for spec in ["cmd {one:1} {two:1}", "cmd {one:1} {one:2}", "cmd {one:2}"] {
            assert!(Configuration::parse(&[spec.to_owned()]).is_err(), "accepted {spec}");
        }
    }

    #[test]
    fn detects_empty_final_commands() {
        let configuration = configuration(&["{*command}"]);
        assert_eq!(
            configuration.views[0].command.expand(&values(&configuration, &[])),
            Err(ExpansionError::EmptyCommand)
        );
    }
}
