use super::*;

pub(super) enum InputAction {
    Quit,
    ForceQuit,
    Dismiss,
    ToggleHelp,
    PrevTab,
    NextTab,
    Tab(usize),
    FocusNext,
    Confirm,
    ScrollUp,
    ScrollDown,
    ScrollPageUp,
    ScrollPageDown,
    ScrollTop,
    ScrollBottom,
    SetEventFilter(EventFilter),
    StartFilter,
    FilterChar(char),
    FilterBackspace,
    FilterSubmit,
    FilterCancel,
    CopyVisible,
    OpenOutput,
    ToggleWorkerSort,
    PrevPiece,
    NextPiece,
}

pub(super) fn poll_input(editing_filter: bool, active_tab: usize) -> Option<InputAction> {
    if !event::poll(Duration::ZERO).unwrap_or(false) {
        return None;
    }
    let Ok(Event::Key(key)) = event::read() else {
        return None;
    };
    if key.kind != KeyEventKind::Press {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(InputAction::ForceQuit);
    }
    if editing_filter {
        return match key.code {
            KeyCode::Esc => Some(InputAction::FilterCancel),
            KeyCode::Enter => Some(InputAction::FilterSubmit),
            KeyCode::Backspace => Some(InputAction::FilterBackspace),
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(InputAction::FilterChar(ch))
            }
            _ => None,
        };
    }
    if let Some(action) = horizontal_key_action(key.code, active_tab) {
        return Some(match action {
            HorizontalKeyAction::PrevPage => InputAction::PrevTab,
            HorizontalKeyAction::NextPage => InputAction::NextTab,
            HorizontalKeyAction::PrevPiece => InputAction::PrevPiece,
            HorizontalKeyAction::NextPiece => InputAction::NextPiece,
        });
    }
    match key.code {
        KeyCode::Char('q') => Some(InputAction::Quit),
        KeyCode::Char('?') => Some(InputAction::ToggleHelp),
        KeyCode::Esc => Some(InputAction::Dismiss),
        KeyCode::Tab => Some(InputAction::FocusNext),
        KeyCode::Enter => Some(InputAction::Confirm),
        KeyCode::Char('1') => Some(InputAction::Tab(0)),
        KeyCode::Char('2') => Some(InputAction::Tab(1)),
        KeyCode::Char('3') => Some(InputAction::Tab(2)),
        KeyCode::Char('4') => Some(InputAction::Tab(3)),
        KeyCode::Char('5') => Some(InputAction::Tab(4)),
        KeyCode::Char('6') => Some(InputAction::Tab(5)),
        KeyCode::Char('a') => Some(InputAction::SetEventFilter(EventFilter::All)),
        KeyCode::Char('f') => Some(InputAction::SetEventFilter(EventFilter::Failures)),
        KeyCode::Char('r') => Some(InputAction::SetEventFilter(EventFilter::Retries)),
        KeyCode::Char('w') => Some(InputAction::SetEventFilter(EventFilter::Worker)),
        KeyCode::Char('s') if active_tab == 3 => Some(InputAction::ToggleWorkerSort),
        KeyCode::Char('s') => Some(InputAction::SetEventFilter(EventFilter::SelectedFile)),
        KeyCode::Char('/') => Some(InputAction::StartFilter),
        KeyCode::Char('y') => Some(InputAction::CopyVisible),
        KeyCode::Char('o') => Some(InputAction::OpenOutput),
        KeyCode::Char('[') => Some(InputAction::PrevPiece),
        KeyCode::Char(']') => Some(InputAction::NextPiece),
        KeyCode::Up | KeyCode::Char('k') => Some(InputAction::ScrollUp),
        KeyCode::Down | KeyCode::Char('j') => Some(InputAction::ScrollDown),
        KeyCode::PageUp => Some(InputAction::ScrollPageUp),
        KeyCode::PageDown => Some(InputAction::ScrollPageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(InputAction::ScrollTop),
        KeyCode::End | KeyCode::Char('G') => Some(InputAction::ScrollBottom),
        _ => None,
    }
}
