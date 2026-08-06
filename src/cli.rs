use crate::{app::App, keymap::load_keymap, template::Configuration};
use anyhow::{Context, Result, bail};
use clap::Parser;
use std::{
    collections::HashSet,
    fs::File,
    io::{IsTerminal, Read},
    path::PathBuf,
    str::FromStr,
};

#[derive(Debug, Parser)]
#[command(version, about = "Run multiple commands against one shared input stream")]
pub struct Cli {
    /// Read input once from this file instead of standard input.
    #[arg(long, value_name = "FILE")]
    input: Option<PathBuf>,

    /// Load application keybindings from this YAML file.
    #[arg(long, value_name = "FILE")]
    config_file: Option<PathBuf>,

    /// Lock child TTYs at their initial view size or at a fixed size.
    #[arg(long, value_name = "COLUMNSxROWS", num_args = 0..=1, require_equals = true, default_missing_value = "initial")]
    lock_tty_size: Option<TtySizeLock>,

    /// Command views, optionally prefixed with LABEL=. Start with [no-tty] to use output pipes.
    #[arg(value_name = "[LABEL=]SPEC", required = true)]
    views: Vec<String>,
}

impl Cli {
    pub async fn run(self) -> Result<crate::app::RunOutcome> {
        let configuration = Configuration::parse(&self.views)?;
        let keymap = load_keymap(self.config_file.as_deref())?;
        let source = self.open_input()?;
        App::new(configuration, source, self.lock_tty_size, keymap)?.run().await
    }

    fn open_input(&self) -> Result<Box<dyn Read + Send>> {
        if let Some(path) = &self.input {
            let file = File::open(path).with_context(|| format!("failed to open input file {}", path.display()))?;
            return Ok(Box::new(file));
        }

        if std::io::stdin().is_terminal() {
            bail!("standard input is a terminal; pipe data to prism or use --input <filepath>");
        }

        Ok(Box::new(std::io::stdin()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtySizeLock {
    Initial,
    Fixed { columns: u16, rows: u16 },
}

impl FromStr for TtySizeLock {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        if value == "initial" {
            return Ok(Self::Initial);
        }
        let (columns, rows) = value
            .split_once('x')
            .ok_or_else(|| "TTY size must have the form COLUMNSxROWS".to_owned())?;
        let columns = parse_dimension(columns, "columns")?;
        let rows = parse_dimension(rows, "rows")?;
        Ok(Self::Fixed { columns, rows })
    }
}

fn parse_dimension(value: &str, name: &str) -> std::result::Result<u16, String> {
    let dimension = value
        .parse::<u16>()
        .map_err(|_| format!("TTY {name} must be an integer from 1 to {}", u16::MAX))?;
    if dimension == 0 {
        return Err(format!("TTY {name} must be at least 1"));
    }
    Ok(dimension)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedViewArgument {
    pub label: String,
    pub spec: String,
    pub explicit_label: bool,
}

pub fn parse_view_arguments(arguments: &[String]) -> Result<Vec<ParsedViewArgument>> {
    let mut explicit_labels = HashSet::new();
    let mut parsed = Vec::with_capacity(arguments.len());

    for (index, argument) in arguments.iter().enumerate() {
        let explicit = argument
            .split_once('=')
            .filter(|(candidate, _)| is_valid_label(candidate));
        let (label, spec, explicit_label) = if let Some((label, spec)) = explicit {
            if !explicit_labels.insert(label.to_owned()) {
                bail!("duplicate view label {label:?}");
            }
            (label.to_owned(), spec.to_owned(), true)
        } else {
            (format!("view {}", index + 1), argument.clone(), false)
        };

        parsed.push(ParsedViewArgument {
            label,
            spec,
            explicit_label,
        });
    }

    Ok(parsed)
}

fn is_valid_label(label: &str) -> bool {
    let mut chars = label.chars();
    matches!(chars.next(), Some('A'..='Z' | 'a'..='z' | '_'))
        && chars.all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn accepts_views_as_positional_arguments() {
        let cli = Cli::try_parse_from(["prism", "input=fx", "output=jq .foo"]).unwrap();
        assert_eq!(cli.views, arguments(&["input=fx", "output=jq .foo"]));
    }

    #[test]
    fn rejects_view_option() {
        assert!(Cli::try_parse_from(["prism", "--view=cat"]).is_err());
    }

    #[test]
    fn parses_config_file_without_consuming_views() {
        let cli = Cli::try_parse_from(["prism", "--config-file", "keys.yaml", "cat"]).unwrap();
        assert_eq!(cli.config_file, Some(PathBuf::from("keys.yaml")));
        assert_eq!(cli.views, arguments(&["cat"]));
    }

    #[test]
    fn parses_tty_size_locks_without_consuming_views() {
        let initial = Cli::try_parse_from(["prism", "--lock-tty-size", "cat"]).unwrap();
        assert_eq!(initial.lock_tty_size, Some(TtySizeLock::Initial));
        assert_eq!(initial.views, arguments(&["cat"]));

        let fixed = Cli::try_parse_from(["prism", "--lock-tty-size=80x24", "cat"]).unwrap();
        assert_eq!(fixed.lock_tty_size, Some(TtySizeLock::Fixed { columns: 80, rows: 24 }));
    }

    #[test]
    fn rejects_invalid_tty_sizes() {
        for size in ["80", "80X24", "0x24", "80x0", "65536x24"] {
            assert!(Cli::try_parse_from(["prism", &format!("--lock-tty-size={size}"), "cat"]).is_err());
        }
    }

    #[test]
    fn recognizes_only_valid_labels() {
        let parsed = parse_view_arguments(&arguments(&["raw=cat", "jq .foo=1", "9bad=cat", "snake-case=x"])).unwrap();
        assert_eq!(parsed[0].label, "raw");
        assert_eq!(parsed[0].spec, "cat");
        assert_eq!(parsed[1].label, "view 2");
        assert_eq!(parsed[2].label, "view 3");
        assert_eq!(parsed[3].label, "snake-case");
    }

    #[test]
    fn rejects_duplicate_explicit_labels() {
        let error = parse_view_arguments(&arguments(&["same=cat", "same=wc"])).unwrap_err();
        assert!(error.to_string().contains("duplicate view label"));
    }
}
