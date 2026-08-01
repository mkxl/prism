use anyhow::{Context, Result};
use crossterm::{
    cursor::{Hide, Show},
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Frame, Terminal, backend::CrosstermBackend};
use std::{
    fs::{File, OpenOptions},
    io::Write as _,
};

pub struct HostTerminal {
    terminal: Terminal<CrosstermBackend<File>>,
    restored: bool,
}

impl HostTerminal {
    pub fn new() -> Result<Self> {
        let mut tty = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .context("no controlling terminal is available for the UI")?;
        enable_raw_mode().context("failed to enable terminal raw mode")?;
        if let Err(error) = execute!(
            tty,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste,
            Hide,
            Clear(ClearType::All)
        ) {
            let _ = disable_raw_mode();
            return Err(error).context("failed to initialize terminal screen");
        }
        let backend = CrosstermBackend::new(tty);
        let terminal = match Terminal::new(backend) {
            Ok(terminal) => terminal,
            Err(error) => {
                let mut tty = OpenOptions::new().write(true).open("/dev/tty")?;
                let _ = execute!(
                    tty,
                    DisableBracketedPaste,
                    DisableMouseCapture,
                    LeaveAlternateScreen,
                    Show
                );
                let _ = disable_raw_mode();
                return Err(error).context("failed to initialize terminal renderer");
            }
        };
        Ok(Self {
            terminal,
            restored: false,
        })
    }

    pub fn draw(&mut self, draw: impl FnOnce(&mut Frame)) -> Result<()> {
        self.terminal.draw(draw).context("failed to render terminal UI")?;
        Ok(())
    }

    pub fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        self.restored = true;
        let writer = self.terminal.backend_mut().writer_mut();
        execute!(
            writer,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen,
            Show
        )
        .context("failed to restore terminal screen")?;
        writer.flush().context("failed to flush restored terminal")?;
        disable_raw_mode().context("failed to disable terminal raw mode")?;
        Ok(())
    }
}

impl Drop for HostTerminal {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
