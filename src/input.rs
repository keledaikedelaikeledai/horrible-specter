use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Conversations,
    Conversation,
    Git,
}

impl Focus {
    pub fn next(self) -> Self {
        match self {
            Self::Conversations => Self::Conversation,
            Self::Conversation => Self::Git,
            Self::Git => Self::Conversations,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::Conversations => Self::Git,
            Self::Conversation => Self::Conversations,
            Self::Git => Self::Conversation,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Compose,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Overlay {
    Help,
    Palette { selected: usize },
    ProviderSelector { selected: usize },
}

impl Overlay {
    pub const PALETTE_LENGTH: usize = 3;

    pub fn selected_palette_command(&self) -> Option<usize> {
        match self {
            Self::Palette { selected } => Some(*selected),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    EnterCompose,
    ExitCompose,
    Submit,
    InsertNewline,
    Backspace,
    Insert(char),
    OpenHelp,
    OpenPalette,
    OpenProviderSelector,
    CloseOverlay,
    ActivateOverlay,
    MoveOverlay(i8),
    ToggleLeftSidebar,
    ToggleRightSidebar,
    NewConversation,
    Move(i8),
    FocusNext,
    FocusPrevious,
    None,
}

pub fn normal_action(key: KeyEvent) -> Action {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('p') => Action::OpenPalette,
            KeyCode::Char('a') => Action::OpenProviderSelector,
            KeyCode::Char('b') => Action::ToggleLeftSidebar,
            KeyCode::Char('d') => Action::ToggleRightSidebar,
            _ => Action::None,
        };
    }
    match key.code {
        KeyCode::Char('q') => Action::Quit,
        KeyCode::Char('i') | KeyCode::Enter => Action::EnterCompose,
        KeyCode::Char('?') => Action::OpenHelp,
        KeyCode::Char('n') => Action::NewConversation,
        KeyCode::Char('j') | KeyCode::Down => Action::Move(1),
        KeyCode::Char('k') | KeyCode::Up => Action::Move(-1),
        KeyCode::Char('h') | KeyCode::Left => Action::FocusPrevious,
        KeyCode::Char('l') | KeyCode::Right => Action::FocusNext,
        KeyCode::Tab => Action::FocusNext,
        KeyCode::BackTab => Action::FocusPrevious,
        _ => Action::None,
    }
}

pub fn compose_action(key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Esc => Action::ExitCompose,
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => Action::InsertNewline,
        KeyCode::Enter => Action::Submit,
        KeyCode::Backspace => Action::Backspace,
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            Action::Insert(character)
        }
        _ => Action::None,
    }
}

pub fn overlay_action(overlay: &Overlay, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => Action::CloseOverlay,
        KeyCode::Enter
            if matches!(
                overlay,
                Overlay::Palette { .. } | Overlay::ProviderSelector { .. }
            ) =>
        {
            Action::ActivateOverlay
        }
        KeyCode::Char('j') | KeyCode::Down
            if matches!(
                overlay,
                Overlay::Palette { .. } | Overlay::ProviderSelector { .. }
            ) =>
        {
            Action::MoveOverlay(1)
        }
        KeyCode::Char('k') | KeyCode::Up
            if matches!(
                overlay,
                Overlay::Palette { .. } | Overlay::ProviderSelector { .. }
            ) =>
        {
            Action::MoveOverlay(-1)
        }
        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent, KeyEventKind, KeyModifiers};

    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new_with_kind(code, modifiers, KeyEventKind::Press)
    }

    #[test]
    fn focus_cycles_in_both_directions() {
        assert_eq!(Focus::Conversations.next(), Focus::Conversation);
        assert_eq!(Focus::Git.next(), Focus::Conversations);
        assert_eq!(Focus::Conversations.previous(), Focus::Git);
        assert_eq!(Focus::Conversation.previous(), Focus::Conversations);
    }

    #[test]
    fn compose_enter_submits_but_shift_enter_adds_a_line() {
        assert_eq!(
            compose_action(key(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Submit
        );
        assert_eq!(
            compose_action(key(KeyCode::Enter, KeyModifiers::SHIFT)),
            Action::InsertNewline
        );
    }

    #[test]
    fn command_palette_supports_selection_and_activation() {
        let overlay = Overlay::Palette { selected: 0 };
        assert_eq!(
            overlay_action(&overlay, key(KeyCode::Down, KeyModifiers::NONE)),
            Action::MoveOverlay(1)
        );
        assert_eq!(
            overlay_action(&overlay, key(KeyCode::Enter, KeyModifiers::NONE)),
            Action::ActivateOverlay
        );
    }
}
