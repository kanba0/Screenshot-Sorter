use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::matching::{Destination, SortEntry};

// ── Entry status ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum EntryStatus {
    Pending,
    Approved,
    Skipped,
}

// ── Per-file entry ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct AppEntry {
    pub sort: SortEntry,
    pub status: EntryStatus,
    /// User-overridden destination (takes priority over sort.destination)
    pub custom_dest: Option<Destination>,
}

impl AppEntry {
    pub fn effective_destination(&self) -> &Destination {
        self.custom_dest.as_ref().unwrap_or(&self.sort.destination)
    }

    /// Ready means a destination is set (even if it needs a new folder created).
    pub fn is_ready(&self) -> bool {
        !matches!(self.effective_destination(), Destination::Unresolved)
    }
}

// ── Edit state ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct EditState {
    pub entry_idx: usize,
    pub text: String,
    pub cursor: usize, // char index, not byte index
}

// ── App mode ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Normal,
    Editing(EditState),
    Confirming,
    Done,
}

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    pub entries: Vec<AppEntry>,
    pub selected: usize,
    pub mode: Mode,
    pub dest_root: PathBuf,
    pub message: Option<String>,
}

#[allow(dead_code)]
pub struct CommitAction {
    pub from: PathBuf,
    pub to: PathBuf,
}

impl App {
    pub fn new(entries: Vec<SortEntry>, dest_root: PathBuf) -> Self {
        let app_entries = entries
            .into_iter()
            .map(|s| AppEntry { status: EntryStatus::Pending, sort: s, custom_dest: None })
            .collect();

        Self { entries: app_entries, selected: 0, mode: Mode::Normal, dest_root, message: None }
    }

    // ── Navigation ────────────────────────────────────────────────────────────

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
        }
    }

    pub fn page_up(&mut self, page: usize) {
        self.selected = self.selected.saturating_sub(page);
    }

    pub fn page_down(&mut self, page: usize) {
        self.selected = (self.selected + page).min(self.entries.len().saturating_sub(1));
    }

    pub fn jump_top(&mut self) {
        self.selected = 0;
    }

    pub fn jump_bottom(&mut self) {
        self.selected = self.entries.len().saturating_sub(1);
    }

    // ── Status changes ────────────────────────────────────────────────────────

    pub fn approve_selected(&mut self) {
        if let Some(entry) = self.entries.get_mut(self.selected) {
            if entry.is_ready() {
                entry.status = EntryStatus::Approved;
            }
        }
    }

    pub fn skip_selected(&mut self) {
        if let Some(entry) = self.entries.get_mut(self.selected) {
            entry.status = EntryStatus::Skipped;
        }
    }

    pub fn approve_all_ready(&mut self) {
        for entry in self.entries.iter_mut() {
            if entry.is_ready() && entry.status == EntryStatus::Pending {
                entry.status = EntryStatus::Approved;
            }
        }
    }

    // ── Editing ───────────────────────────────────────────────────────────────

    pub fn start_editing(&mut self) {
        let idx = self.selected;
        if idx >= self.entries.len() {
            return;
        }
        let current = match self.entries[idx].effective_destination() {
            Destination::Existing(p) => p
                .strip_prefix(&self.dest_root)
                .unwrap_or(p)
                .to_string_lossy()
                .to_string(),
            Destination::New(s) => s.clone(),
            Destination::Unresolved => String::new(),
        };
        let cursor = current.chars().count();
        self.mode = Mode::Editing(EditState { entry_idx: idx, text: current, cursor });
    }

    pub fn edit_insert(&mut self, c: char) {
        if let Mode::Editing(ref mut s) = self.mode {
            let byte = char_to_byte(&s.text, s.cursor);
            s.text.insert(byte, c);
            s.cursor += 1;
        }
    }

    pub fn edit_backspace(&mut self) {
        if let Mode::Editing(ref mut s) = self.mode {
            if s.cursor > 0 {
                s.cursor -= 1;
                let byte = char_to_byte(&s.text, s.cursor);
                s.text.remove(byte);
            }
        }
    }

    pub fn edit_delete(&mut self) {
        if let Mode::Editing(ref mut s) = self.mode {
            let len = s.text.chars().count();
            if s.cursor < len {
                let byte = char_to_byte(&s.text, s.cursor);
                s.text.remove(byte);
            }
        }
    }

    pub fn edit_cursor_left(&mut self) {
        if let Mode::Editing(ref mut s) = self.mode {
            if s.cursor > 0 {
                s.cursor -= 1;
            }
        }
    }

    pub fn edit_cursor_right(&mut self) {
        if let Mode::Editing(ref mut s) = self.mode {
            if s.cursor < s.text.chars().count() {
                s.cursor += 1;
            }
        }
    }

    pub fn edit_cursor_home(&mut self) {
        if let Mode::Editing(ref mut s) = self.mode {
            s.cursor = 0;
        }
    }

    pub fn edit_cursor_end(&mut self) {
        if let Mode::Editing(ref mut s) = self.mode {
            s.cursor = s.text.chars().count();
        }
    }

    pub fn confirm_edit(&mut self) {
        let state = match &self.mode {
            Mode::Editing(s) => s.clone(),
            _ => return,
        };

        let new_dest = self.resolve_destination(&state.text);
        let series_key = self.entries[state.entry_idx].sort.series_key.clone();

        for (i, entry) in self.entries.iter_mut().enumerate() {
            let applies = match &series_key {
                Some(key) => entry.sort.series_key.as_ref() == Some(key),
                None => i == state.entry_idx,
            };
            if applies {
                entry.custom_dest = Some(new_dest.clone());
                if entry.status == EntryStatus::Approved {
                    entry.status = EntryStatus::Pending;
                }
            }
        }

        self.mode = Mode::Normal;
    }

    pub fn cancel_edit(&mut self) {
        self.mode = Mode::Normal;
    }

    fn resolve_destination(&self, text: &str) -> Destination {
        if text.is_empty() {
            return Destination::Unresolved;
        }
        let candidate = self.dest_root.join(text);
        if candidate.is_dir() {
            Destination::Existing(candidate)
        } else {
            Destination::New(text.to_string())
        }
    }

    // ── Stats ─────────────────────────────────────────────────────────────────

    pub fn approved_count(&self) -> usize {
        self.entries.iter().filter(|e| e.status == EntryStatus::Approved).count()
    }

    pub fn pending_count(&self) -> usize {
        self.entries.iter().filter(|e| e.status == EntryStatus::Pending).count()
    }

    pub fn needs_attention_count(&self) -> usize {
        self.entries.iter().filter(|e| e.status == EntryStatus::Pending && !e.is_ready()).count()
    }

    // ── Commit ────────────────────────────────────────────────────────────────

    pub fn commit(&self) -> Result<Vec<CommitAction>> {
        let mut actions = Vec::new();

        for entry in self.entries.iter().filter(|e| e.status == EntryStatus::Approved) {
            let dest_dir = match entry.effective_destination() {
                Destination::Existing(p) => p.clone(),
                Destination::New(name) => {
                    let dir = self.dest_root.join(name);
                    std::fs::create_dir_all(&dir)
                        .with_context(|| format!("failed to create '{}'", dir.display()))?;
                    dir
                }
                Destination::Unresolved => continue,
            };

            let src = &entry.sort.file.path;
            let filename = src.file_name().expect("screenshot path has no filename");
            let dest = unique_dest_path(&dest_dir, filename);

            move_file(src, &dest)
                .with_context(|| format!("failed to move '{}'", src.display()))?;

            actions.push(CommitAction { from: src.clone(), to: dest });
        }

        Ok(actions)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Convert a char index to its corresponding byte offset in a string.
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(byte, _)| byte)
        .unwrap_or(s.len())
}

/// Find a destination path that doesn't clash with an existing file.
/// If `dir/name` exists, tries `dir/name_2`, `dir/name_3`, etc.
fn unique_dest_path(dir: &Path, filename: &std::ffi::OsStr) -> PathBuf {
    let base = dir.join(filename);
    if !base.exists() {
        return base;
    }
    let p = Path::new(filename);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let ext = p.extension().and_then(|s| s.to_str());

    for n in 2u32.. {
        let name = match ext {
            Some(e) => format!("{}_{}.{}", stem, n, e),
            None => format!("{}_{}", stem, n),
        };
        let candidate = dir.join(&name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

/// Move a file, falling back to copy+delete for cross-device moves.
fn move_file(src: &Path, dest: &Path) -> std::io::Result<()> {
    if let Err(e) = std::fs::rename(src, dest) {
        // CrossesDevices = EXDEV on Unix, ERROR_NOT_SAME_DEVICE on Windows
        if e.kind() == std::io::ErrorKind::CrossesDevices {
            std::fs::copy(src, dest)?;
            std::fs::remove_file(src)?;
        } else {
            return Err(e);
        }
    }
    Ok(())
}
