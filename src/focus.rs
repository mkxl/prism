use crate::template::EditorId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    View(usize),
    Editor(EditorId),
}

impl Focus {
    #[must_use]
    pub fn initial(_view_count: usize, editor_order: &[EditorId]) -> Self {
        editor_order.first().copied().map_or(Self::View(0), Self::Editor)
    }

    pub fn cycle(&mut self, view_count: usize, editor_order: &[EditorId], forward: bool) {
        let total = view_count + editor_order.len();
        if total == 0 {
            return;
        }
        let current = match *self {
            Self::View(view) => view,
            Self::Editor(editor) => {
                view_count
                    + editor_order
                        .iter()
                        .position(|&candidate| candidate == editor)
                        .unwrap_or(0)
            }
        };
        let next = if forward {
            (current + 1) % total
        } else {
            (current + total - 1) % total
        };
        *self = if next < view_count {
            Self::View(next)
        } else {
            Self::Editor(editor_order[next - view_count])
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traverses_views_then_displayed_editors() {
        let mut focus = Focus::View(0);
        focus.cycle(2, &[1, 0], true);
        assert_eq!(focus, Focus::View(1));
        focus.cycle(2, &[1, 0], true);
        assert_eq!(focus, Focus::Editor(1));
        focus.cycle(2, &[1, 0], false);
        assert_eq!(focus, Focus::View(1));
    }
}
