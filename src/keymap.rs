use anyhow::{Context, Result};
use crossterm::event::KeyEvent;
use mkutils::KeyMapSession;
use serde::Deserialize;
use std::{fs, path::Path};

const DEFAULT_CONFIG: &str = include_str!("default-config.yaml");

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Action {
    FocusNext,
    FocusPrevious,
    Quit,
    LeaveView,
    Restart,
    FollowView,
    ToggleDebug,
}

#[derive(Deserialize)]
struct Config {
    key_map: KeyMapSession<Action>,
}

pub fn load_keymap(config_file: Option<&Path>) -> Result<KeyMapSession<Action>> {
    let config: Config = if let Some(path) = config_file {
        let contents = fs::read(path).with_context(|| format!("failed to read config file {}", path.display()))?;
        serde_yaml_ng::from_slice(&contents)
            .with_context(|| format!("failed to parse config file {}", path.display()))?
    } else {
        serde_yaml_ng::from_str(DEFAULT_CONFIG).context("failed to parse embedded default config")?
    };
    Ok(config.key_map)
}

#[must_use]
pub const fn normalized_key_event(event: KeyEvent) -> KeyEvent {
    KeyEvent::new(event.code, event.modifiers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    #[test]
    fn dispatches_all_default_bindings_through_mkutils() {
        let mut keymap = load_keymap(None).unwrap();
        let cases = [
            (KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), Action::FocusNext),
            (
                KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
                Action::FocusPrevious,
            ),
            (KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL), Action::Quit),
            (
                KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL),
                Action::LeaveView,
            ),
            (
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
                Action::Restart,
            ),
            (KeyEvent::new(KeyCode::End, KeyModifiers::NONE), Action::FollowView),
            (
                KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
                Action::ToggleDebug,
            ),
        ];
        for (key, action) in cases {
            assert_eq!(keymap.on_key_event(key), Some(&action));
        }
        assert_eq!(keymap.on_tick(), None);
    }

    #[test]
    fn custom_config_replaces_default_bindings() {
        let mut file = NamedTempFile::new().unwrap();
        write!(file, "key_map:\n  - keys: [alt+x]\n    binding:\n      command: quit\n").unwrap();

        let mut keymap = load_keymap(Some(file.path())).unwrap();
        assert_eq!(
            keymap.on_key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT)),
            Some(&Action::Quit)
        );
        assert_eq!(
            keymap.on_key_event(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn reports_invalid_custom_bindings() {
        let mut file = NamedTempFile::new().unwrap();
        write!(
            file,
            "key_map:\n  - keys: [not_a_key]\n    binding:\n      command: quit\n"
        )
        .unwrap();

        let Err(error) = load_keymap(Some(file.path())) else {
            panic!("invalid binding was accepted");
        };
        let message = format!("{error:#}");
        assert!(message.contains("failed to parse config file"));
        assert!(message.contains("unable to parse keystroke"));
    }
}
