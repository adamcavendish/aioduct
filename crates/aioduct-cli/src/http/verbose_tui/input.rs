use super::*;

use super::render::copy_visible_text;

pub(super) fn handle_key_event(key: crossterm::event::KeyEvent, state: &mut TuiState) -> bool {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return true;
    }
    if state.editing_filter {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => state.editing_filter = false,
            KeyCode::Backspace => {
                state.filter_query.pop();
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.filter_query.push(ch);
            }
            _ => {}
        }
        return false;
    }
    match key.code {
        KeyCode::Char('q') => return true,
        KeyCode::Esc if state.show_help => state.show_help = false,
        KeyCode::Tab => state.next_focus(),
        KeyCode::BackTab => state.prev_focus(),
        KeyCode::Right => state.next_focus(),
        KeyCode::Left => state.prev_focus(),
        KeyCode::Char('l') => state.next_page(),
        KeyCode::Char('h') => state.prev_page(),
        KeyCode::Char('1') => state.set_page(0),
        KeyCode::Char('2') => state.set_page(1),
        KeyCode::Char('3') => state.set_page(2),
        KeyCode::Char('4') => state.set_page(3),
        KeyCode::Char('5') => state.set_page(4),
        KeyCode::Char('6') => state.set_page(5),
        KeyCode::Char('j') | KeyCode::Down => state.scroll_down(1),
        KeyCode::Char('k') | KeyCode::Up => state.scroll_up(1),
        KeyCode::PageDown => state.scroll_down(state.body_visible_rows),
        KeyCode::PageUp => state.scroll_up(state.body_visible_rows),
        KeyCode::Char('g') | KeyCode::Home => state.scroll_to_top(),
        KeyCode::Char('G') | KeyCode::End => state.scroll_to_bottom(),
        KeyCode::Char(' ') => state.toggle_body_autoscroll(),
        KeyCode::Char('/') => state.editing_filter = true,
        KeyCode::Char('y') => copy_visible_text(state),
        KeyCode::Char('?') => state.show_help = !state.show_help,
        _ => {}
    }
    false
}
