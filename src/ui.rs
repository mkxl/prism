use crate::{app::App, focus::Focus};
use mkutils::{Orientation, PointUsize, ScrollViewState, ScrollWhen};
use ratatui::{
    Frame,
    layout::{Alignment, Position, Rect},
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};

const EDITOR_HEIGHT: u16 = 3;

#[derive(Debug, Default)]
pub struct Areas {
    pub views: Vec<Rect>,
    pub editors: Vec<(usize, Rect)>,
    pub too_small: bool,
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let areas = calculate_areas(area, app.views.len(), &app.editor_order);
    app.areas = areas;
    if app.areas.too_small {
        render_too_small(frame, area);
        return;
    }

    for view_id in 0..app.views.len() {
        let area = app.areas.views[view_id];
        render_view(frame, app, view_id, area);
    }
    for order_index in 0..app.areas.editors.len() {
        let (editor_id, area) = app.areas.editors[order_index];
        render_editor(frame, app, editor_id, area);
    }
}

fn calculate_areas(area: Rect, view_count: usize, editor_order: &[usize]) -> Areas {
    let editor_rows = u16::try_from(editor_order.len())
        .unwrap_or(u16::MAX)
        .saturating_mul(EDITOR_HEIGHT);
    let view_height = area.height.saturating_sub(editor_rows);
    let view_count_u16 = u16::try_from(view_count).unwrap_or(u16::MAX).max(1);
    let too_small = view_height < 3 || area.width < view_count_u16.saturating_mul(3);
    if too_small {
        return Areas {
            too_small: true,
            ..Areas::default()
        };
    }

    let mut views = Vec::with_capacity(view_count);
    let base_width = area.width / view_count_u16;
    let remainder = area.width % view_count_u16;
    let mut x = area.x;
    for index in 0..view_count_u16 {
        let width = base_width + u16::from(index < remainder);
        views.push(Rect::new(x, area.y, width, view_height));
        x = x.saturating_add(width);
    }

    let editors = editor_order
        .iter()
        .enumerate()
        .map(|(index, &editor)| {
            let y = area.y + view_height + u16::try_from(index).unwrap_or(u16::MAX).saturating_mul(EDITOR_HEIGHT);
            (editor, Rect::new(area.x, y, area.width, EDITOR_HEIGHT))
        })
        .collect();
    Areas {
        views,
        editors,
        too_small: false,
    }
}

fn render_too_small(frame: &mut Frame, area: Rect) {
    let line = Rect::new(area.x, area.y + area.height.saturating_sub(1) / 2, area.width, 1);
    frame.render_widget(
        Paragraph::new("terminal too small")
            .alignment(Alignment::Center)
            .style(Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        line,
    );
}

fn render_view(frame: &mut Frame, app: &mut App, view_id: usize, area: Rect) {
    let focused = app.focus == Focus::View(view_id);
    let border_style = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let view = &mut app.views[view_id];
    let title = format!(" {} [{}] ", view.definition.label, view.state_label());
    let mut block = Block::new()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(title));
    if let Some(error) = view.error() {
        block = block.title_bottom(Line::from(format!(" {error} ")).style(Style::new().fg(Color::Red)));
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    view.resize(inner.height, inner.width);
    let scroll_position = view.terminal.scroll_position();
    view.terminal.render(frame, inner, focused);
    if let Some((position, max_position)) = scroll_position {
        render_view_scrollbar(frame, inner, position, max_position, focused);
    }
}

fn render_view_scrollbar(frame: &mut Frame, area: Rect, position: usize, max_position: usize, focused: bool) {
    let mut state = ScrollViewState::new(ScrollWhen::ForLargeContent, None);
    state.set_latest_content_size(PointUsize::new(
        usize::from(area.width),
        max_position.saturating_add(usize::from(area.height)),
    ));
    state.set_latest_scroll_view_area_size(area.as_size().into());
    state.scroll_down(position);
    let style = Style::new().fg(if focused { Color::Cyan } else { Color::DarkGray });
    state
        .scroll_bar(Orientation::Vertical, style)
        .render(area, frame.buffer_mut());
}

fn render_editor(frame: &mut Frame, app: &mut App, editor_id: usize, area: Rect) {
    let focused = app.focus == Focus::Editor(editor_id);
    let border_style = if focused {
        Style::new().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let editor = &mut app.editors[editor_id];
    let mut block = Block::new()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(Line::from(format!(" {} ", editor.name)));
    if let Some(error) = &editor.validation_error {
        block = block.title_bottom(Line::from(format!(" {error} ")).style(Style::new().fg(Color::Red)));
    }
    let inner = block.inner(area);
    editor.ensure_cursor_visible(inner.width);
    let text = editor.visible_text(inner.width);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(text), inner);
    if focused && inner.width > 0 {
        let column = editor.cursor_column().min(inner.width.saturating_sub(1));
        frame.set_cursor_position((inner.x + column, inner.y));
    }
}

pub const fn position(column: u16, row: u16) -> Position {
    Position::new(column, row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distributes_view_remainder_left_to_right() {
        let areas = calculate_areas(Rect::new(0, 0, 11, 8), 3, &[]);
        assert_eq!(areas.views.iter().map(|area| area.width).collect::<Vec<_>>(), [4, 4, 3]);
    }

    #[test]
    fn reserves_three_rows_per_editor_and_recovers() {
        assert!(calculate_areas(Rect::new(0, 0, 10, 8), 1, &[0, 1]).too_small);
        assert!(!calculate_areas(Rect::new(0, 0, 10, 9), 1, &[0, 1]).too_small);
    }
}
