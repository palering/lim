//! Terminal setup and teardown.
//!
//! Handles entering and leaving raw mode, setting up the alternate screen,
//! and creating the ratatui `Terminal` instance.

use std::io::{self, Stdout};

use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use lim_core::error::{LimError, Result};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

/// RAII guard that restores the terminal on drop.
///
/// Created by [`init`]; dropping it restores the original terminal state
/// (leaves alternate screen, disables raw mode).
pub struct TerminalGuard {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    /// Enter alternate screen + raw mode, return a guard holding the terminal.
    pub fn init() -> Result<Self> {
        enable_raw_mode().map_err(|e| LimError::terminal("enable raw mode", e))?;
        execute!(io::stdout(), EnterAlternateScreen)
            .map_err(|e| LimError::terminal("enter alternate screen", e))?;
        let backend = CrosstermBackend::new(io::stdout());
        let terminal =
            Terminal::new(backend).map_err(|e| LimError::terminal("create terminal backend", e))?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}
