//! The interactive `/storage` dialog: every place ARSY keeps files, how much
//! each takes, and the cleanups it offers.
//!
//! Drawing and key handling only. The rows arrive already measured and every
//! action is handed back to the caller, so the dialog neither reads the disk
//! nor deletes anything itself.
use super::*;

/// One location as the dialog shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageRow {
    pub label: String,
    /// `global` or `project`.
    pub scope: String,
    pub path: String,
    /// The size, already written for a person, or `—` when absent.
    pub size: String,
    /// What Enter offers here, such as `clean`; `None` when nothing.
    pub action: Option<String>,
    /// Whether the action deletes session history, which asks for the
    /// workspace name as well as a yes.
    pub resets_history: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageAction {
    /// Run the cleanup on the row at this index.
    Clean(usize),
    /// Reset the history on this row, with the name the operator typed.
    ResetHistory(usize, String),
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageDialogMode {
    List,
    /// Asking yes or no before the action on this row.
    Confirm(usize),
    /// A yes was given to a history reset; now the workspace name.
    TypeName(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageDialogState {
    pub rows: Vec<StorageRow>,
    pub selected: usize,
    pub mode: StorageDialogMode,
    /// What the operator has typed of the workspace name.
    pub typed: String,
    /// The name a history reset is confirmed with.
    pub workspace_name: String,
    /// What the last action did.
    pub notice: Option<String>,
}

impl StorageDialogState {
    pub fn new(rows: Vec<StorageRow>, workspace_name: String) -> Self {
        Self {
            rows,
            selected: 0,
            mode: StorageDialogMode::List,
            typed: String::new(),
            workspace_name,
            notice: None,
        }
    }

    /// Replace the rows after an action, keeping the marker where it was.
    pub fn reload(&mut self, rows: Vec<StorageRow>) {
        self.selected = self.selected.min(rows.len().saturating_sub(1));
        self.rows = rows;
        self.mode = StorageDialogMode::List;
        self.typed.clear();
    }

    pub fn handle_key(&mut self, key: Key) -> Option<StorageAction> {
        match self.mode {
            StorageDialogMode::List => self.list_key(key),
            StorageDialogMode::Confirm(index) => self.confirm_key(index, key),
            StorageDialogMode::TypeName(index) => self.name_key(index, key),
        }
    }

    fn list_key(&mut self, key: Key) -> Option<StorageAction> {
        let count = self.rows.len();
        match key {
            Key::Up if count > 0 => {
                self.selected = (self.selected + count - 1) % count;
                None
            }
            Key::Down if count > 0 => {
                self.selected = (self.selected + 1) % count;
                None
            }
            Key::Enter | Key::Newline | Key::Char('c' | 'C') => {
                match self.rows.get(self.selected) {
                    Some(row) if row.action.is_some() => {
                        self.notice = None;
                        self.mode = StorageDialogMode::Confirm(self.selected);
                    }
                    Some(_) => {
                        self.notice = Some("nothing to clean here".to_owned());
                    }
                    None => {}
                }
                None
            }
            Key::Interrupt | Key::Eof => Some(StorageAction::Close),
            _ => None,
        }
    }

    fn confirm_key(&mut self, index: usize, key: Key) -> Option<StorageAction> {
        match key {
            Key::Enter | Key::Newline | Key::Char('y' | 'Y') => {
                if self.rows.get(index).is_some_and(|row| row.resets_history) {
                    self.typed.clear();
                    self.mode = StorageDialogMode::TypeName(index);
                    None
                } else {
                    Some(StorageAction::Clean(index))
                }
            }
            Key::Char('n' | 'N') | Key::Interrupt => {
                self.mode = StorageDialogMode::List;
                None
            }
            Key::Eof => Some(StorageAction::Close),
            _ => None,
        }
    }

    fn name_key(&mut self, index: usize, key: Key) -> Option<StorageAction> {
        match key {
            Key::Enter | Key::Newline => {
                Some(StorageAction::ResetHistory(index, self.typed.clone()))
            }
            Key::Char(character) => {
                self.typed.push(character);
                None
            }
            Key::Backspace => {
                self.typed.pop();
                None
            }
            Key::Interrupt => {
                self.mode = StorageDialogMode::List;
                self.typed.clear();
                None
            }
            Key::Eof => Some(StorageAction::Close),
            _ => None,
        }
    }

    pub fn render(&self, width: usize, colour: bool) -> String {
        let width = width.max(MIN_WIDTH);
        let inner = width.saturating_sub(4);
        let rule = "─".repeat(width.saturating_sub(2));
        let mut lines = vec![dialog_top(" STORAGE ", width, colour)];
        match self.mode {
            StorageDialogMode::List => {
                for (index, row) in self.rows.iter().enumerate() {
                    let marked = index == self.selected;
                    let action = row
                        .action
                        .as_deref()
                        .map(|action| format!("  [{action}]"))
                        .unwrap_or_default();
                    let text = format!(
                        "{} {:<8} {:<20} {:>9}{action}",
                        if marked { "›" } else { " " },
                        row.scope,
                        row.label,
                        row.size,
                    );
                    let sgr = if marked { sgr_accent() } else { sgr_dim() };
                    lines.push(dialog_line(&text, inner, colour, sgr));
                    if marked {
                        lines.push(dialog_line(
                            &format!("    {}", row.path),
                            inner,
                            colour,
                            sgr_dim(),
                        ));
                    }
                }
                lines.push(dialog_line("", inner, colour, ""));
                if let Some(notice) = &self.notice {
                    lines.push(dialog_line(notice, inner, colour, sgr_dim()));
                }
                lines.push(dialog_line(
                    "[↑/↓] Navigate  [Enter/c] Clean  [Esc] Close",
                    inner,
                    colour,
                    sgr_dim(),
                ));
            }
            StorageDialogMode::Confirm(index) => {
                if let Some(row) = self.rows.get(index) {
                    let action = row.action.as_deref().unwrap_or("clean");
                    let question = if row.resets_history {
                        format!(
                            "Delete every recorded session of `{}`?",
                            self.workspace_name
                        )
                    } else {
                        format!("{action} {} ({})?", row.label, row.size)
                    };
                    lines.push(dialog_line(&question, inner, colour, sgr_err()));
                    lines.push(dialog_line(&row.path, inner, colour, sgr_dim()));
                    if row.resets_history {
                        lines.push(dialog_line(
                            "This cannot be undone. `arsy session export` keeps a copy first.",
                            inner,
                            colour,
                            sgr_dim(),
                        ));
                    }
                }
                lines.push(dialog_line("", inner, colour, ""));
                lines.push(dialog_line(
                    "[y/Enter] Yes  [n/Esc] Back",
                    inner,
                    colour,
                    sgr_dim(),
                ));
            }
            StorageDialogMode::TypeName(_) => {
                lines.push(dialog_line(
                    &format!(
                        "Type `{}` to delete its session history:",
                        self.workspace_name
                    ),
                    inner,
                    colour,
                    sgr_err(),
                ));
                lines.push(dialog_line(
                    &format!("› {}█", self.typed),
                    inner,
                    colour,
                    sgr_accent(),
                ));
                lines.push(dialog_line("", inner, colour, ""));
                lines.push(dialog_line(
                    "[Enter] Delete  [Esc] Back",
                    inner,
                    colour,
                    sgr_dim(),
                ));
            }
        }
        lines.push(paint(colour, sgr_border(), &format!("╰{rule}╯")));
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(label: &str, action: Option<&str>, resets_history: bool) -> StorageRow {
        StorageRow {
            label: label.to_owned(),
            scope: "project".to_owned(),
            path: format!("/w/.arsy/state/{label}"),
            size: "1.0 KB".to_owned(),
            action: action.map(str::to_owned),
            resets_history,
        }
    }

    fn dialog() -> StorageDialogState {
        StorageDialogState::new(
            vec![
                row("settings", None, false),
                row("cache", Some("clean"), false),
                row("session history", Some("reset"), true),
            ],
            "w".to_owned(),
        )
    }

    #[test]
    fn a_cleanup_asks_once_and_a_row_without_one_says_so() {
        let mut dialog = dialog();
        assert_eq!(dialog.handle_key(Key::Enter), None);
        assert_eq!(dialog.mode, StorageDialogMode::List);
        assert!(dialog.notice.is_some());

        dialog.handle_key(Key::Down);
        assert_eq!(dialog.handle_key(Key::Enter), None);
        assert_eq!(dialog.mode, StorageDialogMode::Confirm(1));
        assert_eq!(dialog.handle_key(Key::Char('n')), None);
        assert_eq!(dialog.mode, StorageDialogMode::List);

        dialog.handle_key(Key::Char('c'));
        assert_eq!(
            dialog.handle_key(Key::Char('y')),
            Some(StorageAction::Clean(1))
        );
    }

    #[test]
    fn a_history_reset_asks_twice_and_hands_back_what_was_typed() {
        let mut dialog = dialog();
        dialog.handle_key(Key::Up);
        assert_eq!(dialog.selected, 2);
        dialog.handle_key(Key::Enter);
        assert_eq!(
            dialog.handle_key(Key::Char('y')),
            None,
            "a yes is not enough"
        );
        assert_eq!(dialog.mode, StorageDialogMode::TypeName(2));
        assert!(dialog.render(80, false).contains("Type `w`"));

        for character in "wx".chars() {
            dialog.handle_key(Key::Char(character));
        }
        dialog.handle_key(Key::Backspace);
        assert_eq!(
            dialog.handle_key(Key::Enter),
            Some(StorageAction::ResetHistory(2, "w".to_owned()))
        );
    }

    #[test]
    fn escape_backs_out_of_the_name_without_acting() {
        let mut dialog = dialog();
        dialog.selected = 2;
        dialog.handle_key(Key::Enter);
        dialog.handle_key(Key::Enter);
        dialog.handle_key(Key::Char('w'));
        assert_eq!(dialog.handle_key(Key::Interrupt), None);
        assert_eq!(dialog.mode, StorageDialogMode::List);
        assert!(dialog.typed.is_empty());
        assert_eq!(
            dialog.handle_key(Key::Interrupt),
            Some(StorageAction::Close)
        );
    }
}
