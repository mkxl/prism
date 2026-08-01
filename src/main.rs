mod app;
mod cli;
mod editor;
mod event;
mod focus;
mod input_store;
mod keymap;
mod pty;
mod template;
mod terminal;
mod terminal_model;
mod ui;
mod view;

use crate::{app::RunOutcome, cli::Cli};
use clap::Parser;
use std::process::ExitCode;

fn main() -> ExitCode {
    match tokio::runtime::Runtime::new()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| runtime.block_on(Cli::parse().run()))
    {
        Ok(RunOutcome::Quit) => ExitCode::SUCCESS,
        Ok(RunOutcome::Signal(signal)) => ExitCode::from((128 + signal).try_into().unwrap_or(u8::MAX)),
        Err(error) => {
            eprintln!("prism: {error:#}");
            ExitCode::FAILURE
        }
    }
}
