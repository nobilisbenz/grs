//! Tiny vim-style key parser. Holds a small buffer for multi-key sequences
//! (`gg`, `:N<Enter>`) with a 500ms timeout.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What the parser is currently collecting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyState {
    Idle,
    /// User pressed `g`; waiting for the second `g` of `gg`.
    PendingG,
    /// User pressed `/`; collecting filter text until `Enter`/`Esc`.
    PendingFilter(String),
}

/// Outcome of feeding a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyOutcome {
    /// A complete, immediately-actionable key.
    Action(KeyAction),
    /// Buffer updated; no action yet (caller should keep listening).
    Pending(KeyState),
    /// Esc / timeout / etc. — buffer cleared, no action.
    Cleared,
}

/// All actions the TUI handles.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Quit,
    Back,
    Down,
    Up,
    Left,
    Right,
    /// 10-line jump down (Shift+j).
    JumpDown10,
    /// 10-line jump up (Shift+k).
    JumpUp10,
    /// Viewport top of the current snap's content (`gg`).
    GotoFirst,
    /// Viewport bottom of the current snap's content (`G`).
    GotoLast,
    /// Jump to the previous snap in the session (`[`).
    PrevSnap,
    /// Jump to the next snap in the session (`]`).
    NextSnap,
    TabFile,
    Refresh,
    Enter,
    Filter,    // `/`
    ConfirmFilter, // Enter in filter mode
    CancelFilter,
    /// Pass-through char into the filter input.
    FilterChar(char),
    /// Backspace in filter input.
    FilterBackspace,
    /// `n`: in the session list, create a new session. In the code review
    /// view, jump to the next change row. The view decides.
    NewSession,
    /// `N`: in the session list, create a new session and open it. In the
    /// code review view, jump to the previous change row.
    NewSessionAndOpen,
    /// Delete the selected session (session list only; requires a confirm step).
    Delete,
    /// Toggle the help overlay.
    Help,
    /// No-op (unhandled).
    None,
}

pub struct VimParser {
    state: KeyState,
}

impl Default for VimParser {
    fn default() -> Self {
        Self::new()
    }
}

impl VimParser {
    pub fn new() -> Self {
        Self { state: KeyState::Idle }
    }

    pub fn state(&self) -> &KeyState {
        &self.state
    }

    /// Reset the parser (e.g. on timeout or screen change).
    pub fn reset(&mut self) {
        self.state = KeyState::Idle;
    }

    pub fn feed(&mut self, key: KeyEvent) -> KeyOutcome {
        // Ctrl-C is always quit.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return KeyOutcome::Action(KeyAction::Quit);
        }

        // Handle filter-input state first (we're typing into a buffer).

        // Filter-input state: any char appends to the buffer (including
        // digits, so we don't conflict with colon mode). Enter applies,
        // Esc cancels and clears, Backspace pops.
        if let KeyState::PendingFilter(ref mut buf) = self.state {
            match key.code {
                KeyCode::Esc => {
                    buf.clear();
                    self.reset();
                    return KeyOutcome::Action(KeyAction::CancelFilter);
                }
                KeyCode::Enter => {
                    self.reset();
                    return KeyOutcome::Action(KeyAction::ConfirmFilter);
                }
                KeyCode::Backspace => {
                    buf.pop();
                    return KeyOutcome::Action(KeyAction::FilterBackspace);
                }
                KeyCode::Char(c) => {
                    buf.push(c);
                    return KeyOutcome::Action(KeyAction::FilterChar(c));
                }
                _ => return KeyOutcome::Pending(self.state.clone()),
            }
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.reset();
                KeyOutcome::Action(KeyAction::Quit)
            }
            KeyCode::Char('Q') | KeyCode::Char('i') | KeyCode::Char('s') => {
                // Reserved: ignore (s was side-by-side in the old replay)
                self.reset();
                KeyOutcome::Action(KeyAction::None)
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.reset();
                KeyOutcome::Action(KeyAction::Down)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.reset();
                KeyOutcome::Action(KeyAction::Up)
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.reset();
                KeyOutcome::Action(KeyAction::Back)
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.reset();
                KeyOutcome::Action(KeyAction::Enter)
            }
            KeyCode::Char('J') => {
                self.reset();
                KeyOutcome::Action(KeyAction::JumpDown10)
            }
            KeyCode::Char('K') => {
                self.reset();
                KeyOutcome::Action(KeyAction::JumpUp10)
            }
            KeyCode::Char('[') => {
                self.reset();
                KeyOutcome::Action(KeyAction::PrevSnap)
            }
            KeyCode::Char(']') => {
                self.reset();
                KeyOutcome::Action(KeyAction::NextSnap)
            }
            KeyCode::Char('g') => match self.state {
                KeyState::Idle => {
                    self.state = KeyState::PendingG;
                    KeyOutcome::Pending(self.state.clone())
                }
                KeyState::PendingG => {
                    self.reset();
                    KeyOutcome::Action(KeyAction::GotoFirst)
                }
                _ => {
                    self.reset();
                    KeyOutcome::Action(KeyAction::None)
                }
            },
            KeyCode::Char('G') => {
                self.reset();
                KeyOutcome::Action(KeyAction::GotoLast)
            }
            KeyCode::Tab => {
                self.reset();
                KeyOutcome::Action(KeyAction::TabFile)
            }
            KeyCode::Char('r') => {
                self.reset();
                KeyOutcome::Action(KeyAction::Refresh)
            }
            KeyCode::Char('d') => {
                self.reset();
                KeyOutcome::Action(KeyAction::Delete)
            }
            KeyCode::Char('n') => {
                self.reset();
                KeyOutcome::Action(KeyAction::NewSession)
            }
            KeyCode::Char('N') => {
                self.reset();
                KeyOutcome::Action(KeyAction::NewSessionAndOpen)
            }
            KeyCode::Char('?') => {
                self.reset();
                KeyOutcome::Action(KeyAction::Help)
            }
            KeyCode::Enter => {
                self.reset();
                KeyOutcome::Action(KeyAction::Enter)
            }
            KeyCode::Char('/') => {
                // Enter filter mode: any subsequent chars update the filter
                // buffer (see PendingFilter handling above).
                self.state = KeyState::PendingFilter(String::new());
                KeyOutcome::Action(KeyAction::Filter)
            }
            _ => {
                self.reset();
                KeyOutcome::Action(KeyAction::None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    #[test]
    fn gg_is_goto_first() {
        let mut p = VimParser::new();
        assert!(matches!(p.feed(k('g')), KeyOutcome::Pending(_)));
        assert!(matches!(p.feed(k('g')), KeyOutcome::Action(KeyAction::GotoFirst)));
    }

    #[test]
    fn j_k_h_l() {
        let mut p = VimParser::new();
        assert!(matches!(p.feed(k('j')), KeyOutcome::Action(KeyAction::Down)));
        assert!(matches!(p.feed(k('k')), KeyOutcome::Action(KeyAction::Up)));
        // `h`/`l` are no longer bound to step actions; they go through the
        // generic arm and return `None` (effectively a no-op for the view).
        assert!(matches!(p.feed(k('h')), KeyOutcome::Action(KeyAction::Back)));
        assert!(matches!(p.feed(k('l')), KeyOutcome::Action(KeyAction::Enter)));
    }

    #[test]
    fn jk_caps_jump_10() {
        let mut p = VimParser::new();
        assert!(matches!(p.feed(k('J')), KeyOutcome::Action(KeyAction::JumpDown10)));
        assert!(matches!(p.feed(k('K')), KeyOutcome::Action(KeyAction::JumpUp10)));
    }

    #[test]
    fn brackets_step_snap() {
        let mut p = VimParser::new();
        assert!(matches!(p.feed(k('[')), KeyOutcome::Action(KeyAction::PrevSnap)));
        assert!(matches!(p.feed(k(']')), KeyOutcome::Action(KeyAction::NextSnap)));
    }

    #[test]
    fn reset_after_non_g_char() {
        let mut p = VimParser::new();
        let _ = p.feed(k('g'));
        let _ = p.feed(k('x')); // not g
        assert!(matches!(p.state, KeyState::Idle));
    }
}
