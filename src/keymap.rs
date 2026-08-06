use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use mkutils::{KeyBinding, KeyMap, KeyMapSession};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    FocusNext,
    FocusPrevious,
    Quit,
    LeaveView,
    Restart,
    FollowView,
    ToggleDebug,
}

#[must_use]
pub fn default_keymap() -> KeyMapSession<Action> {
    let binding = |code, modifiers, action| KeyBinding {
        keys: vec![KeyEvent::new(code, modifiers)],
        binding: action,
    };
    KeyMap::new(vec![
        binding(KeyCode::Tab, KeyModifiers::NONE, Action::FocusNext),
        binding(KeyCode::BackTab, KeyModifiers::SHIFT, Action::FocusPrevious),
        binding(KeyCode::Char('q'), KeyModifiers::CONTROL, Action::Quit),
        binding(KeyCode::Char(']'), KeyModifiers::CONTROL, Action::LeaveView),
        binding(KeyCode::Char('r'), KeyModifiers::CONTROL, Action::Restart),
        binding(KeyCode::End, KeyModifiers::NONE, Action::FollowView),
        binding(KeyCode::Char('g'), KeyModifiers::CONTROL, Action::ToggleDebug),
    ])
    .into()
}

#[must_use]
pub const fn normalized_key_event(event: KeyEvent) -> KeyEvent {
    KeyEvent::new(event.code, event.modifiers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatches_all_default_bindings_through_mkutils() {
        let mut keymap = default_keymap();
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
}
