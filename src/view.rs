use crate::{pty::RunningProcess, template::ViewDefinition, terminal_model::TerminalModel};
use std::{os::unix::process::ExitStatusExt as _, process::ExitStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunState {
    Starting,
    Running,
    Exited(String),
    Error(String),
}

#[derive(Debug)]
pub struct View {
    pub definition: ViewDefinition,
    pub generation: u64,
    pub state: RunState,
    pub terminal: TerminalModel,
    pub process: Option<RunningProcess>,
}

impl View {
    #[must_use]
    pub fn new(definition: ViewDefinition) -> Self {
        Self {
            definition,
            generation: 0,
            state: RunState::Starting,
            terminal: TerminalModel::new(1, 1),
            process: None,
        }
    }

    #[must_use]
    pub const fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.generation
    }

    pub fn terminate(&mut self) {
        if let Some(mut process) = self.process.take() {
            process.terminate();
        }
    }

    pub fn resize(&mut self, rows: u16, columns: u16) {
        let size = (rows.max(1), columns.max(1));
        if self.terminal.size() == size {
            return;
        }
        self.terminal.resize(size.0, size.1);
        if let Some(process) = &self.process
            && let Err(error) = process.resize(size.0, size.1)
        {
            self.state = RunState::Error(format!("resize failed: {error:#}"));
        }
    }

    pub fn poll_exit(&mut self) {
        let Some(process) = &mut self.process else {
            return;
        };
        match process.poll_exit() {
            Ok(Some(status)) => self.state = RunState::Exited(format_exit_status(status)),
            Ok(None) => {}
            Err(error) => self.state = RunState::Error(error.to_string()),
        }
    }

    #[must_use]
    pub fn state_label(&self) -> String {
        match &self.state {
            RunState::Starting => "starting".to_owned(),
            RunState::Running => "running".to_owned(),
            RunState::Exited(status) => format!("exited {status}"),
            RunState::Error(_) => "error".to_owned(),
        }
    }

    #[must_use]
    pub fn error(&self) -> Option<&str> {
        match &self.state {
            RunState::Error(error) => Some(error),
            _ => None,
        }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn format_exit_status(status: ExitStatus) -> String {
    status.code().map_or_else(
        || {
            status
                .signal()
                .map_or_else(|| "unknown".to_owned(), |signal| format!("signal {signal}"))
        },
        |code| code.to_string(),
    )
}
