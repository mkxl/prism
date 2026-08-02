use crate::{app::App, template::Configuration};
use anyhow::{Context, Result, bail};
use clap::Parser;
use std::{
    collections::HashSet,
    fs::File,
    io::{IsTerminal, Read},
    path::PathBuf,
};

#[derive(Debug, Parser)]
#[command(version, about = "Run multiple commands against one shared input stream")]
pub struct Cli {
    /// Read input once from this file instead of standard input.
    #[arg(long, value_name = "FILE")]
    input: Option<PathBuf>,

    /// Command views, optionally prefixed with LABEL=. Editors use {[*]NAME[:INDEX][=DEFAULT]}.
    #[arg(value_name = "[LABEL=]SPEC", required = true)]
    views: Vec<String>,
}

impl Cli {
    pub async fn run(self) -> Result<crate::app::RunOutcome> {
        let configuration = Configuration::parse(&self.views)?;
        let source = self.open_input()?;
        App::new(configuration, source)?.run().await
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
