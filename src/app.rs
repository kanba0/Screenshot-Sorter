use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::matching::{Destination, SeriesKey, SortEntry};

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

// ── Grouped view model ────────────────────────────────────────────────────────

/// A set of screenshots that belong to the same series and share a destination.
/// Holds *indices* into `App.entries` rather than the entries themselves, so the
/// flat list stays the single source of truth and every existing operation on it
/// keeps working unchanged.
#[derive(Debug, Clone)]
pub struct Group {
    pub label: String,
    pub entry_indices: Vec<usize>,
    pub expanded: bool,
}

/// One visible line in the list. `selected` indexes a `Vec<Row>` built from the
/// groups and their expansion state — see `App::visible_rows`.
#[derive(Debug, Clone, Copy)]
pub enum Row {
    /// Collapsible header for a multi-file series. Payload: group index.
    Header(usize),
    /// A one-file series, shown as a single row. Payload: entry index.
    Single(usize),
    /// A file shown beneath an expanded header. Payload: (group, entry).
    Child(usize, usize),
}

/// Aggregate status of a group's files, for the header line and detail panel.
#[derive(Debug, Default, Clone, Copy)]
pub struct GroupStatus {
    pub total: usize,
    pub approved: usize,
    pub skipped: usize,
    pub pending: usize,
    pub unresolved: usize,
}

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    pub entries: Vec<AppEntry>,
    pub groups: Vec<Group>,
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
        let app_entries: Vec<AppEntry> = entries
            .into_iter()
            .map(|s| AppEntry { status: EntryStatus::Pending, sort: s, custom_dest: None })
            .collect();

        let groups = build_groups(&app_entries);

        Self {
            entries: app_entries,
            groups,
            selected: 0,
            mode: Mode::Normal,
            dest_root,
            message: None,
        }
    }

    // ── Navigation ────────────────────────────────────────────────────────────

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.visible_len() {
            self.selected += 1;
        }
    }

    pub fn page_up(&mut self, page: usize) {
        self.selected = self.selected.saturating_sub(page);
    }

    pub fn page_down(&mut self, page: usize) {
        self.selected = (self.selected + page).min(self.visible_len().saturating_sub(1));
    }

    pub fn jump_top(&mut self) {
        self.selected = 0;
    }

    pub fn jump_bottom(&mut self) {
        self.selected = self.visible_len().saturating_sub(1);
    }

    // ── Grouped view ──────────────────────────────────────────────────────────

    /// The rows currently on screen, derived from the groups and their expansion
    /// state. Recomputed on demand — cheap at this scale, and it spares us a cache
    /// we'd have to keep in sync. `selected` is an index into this list.
    pub fn visible_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (gi, g) in self.groups.iter().enumerate() {
            // A one-file series is its own row; no point collapsing a single file.
            if g.entry_indices.len() == 1 {
                rows.push(Row::Single(g.entry_indices[0]));
            } else {
                rows.push(Row::Header(gi));
                if g.expanded {
                    for &ei in &g.entry_indices {
                        rows.push(Row::Child(gi, ei));
                    }
                }
            }
        }
        rows
    }

    pub fn visible_len(&self) -> usize {
        self.visible_rows().len()
    }

    pub fn selected_row(&self) -> Option<Row> {
        self.visible_rows().get(self.selected).copied()
    }

    /// The entry a status/edit action targets: the entry itself for a file row, or
    /// the group's first entry for a header (edits apply to the whole series).
    fn selected_entry_idx(&self) -> Option<usize> {
        match self.selected_row()? {
            Row::Header(gi) => self.groups[gi].entry_indices.first().copied(),
            Row::Single(ei) | Row::Child(_, ei) => Some(ei),
        }
    }

    fn clamp_selected(&mut self) {
        self.selected = self.selected.min(self.visible_len().saturating_sub(1));
    }

    /// Move the cursor onto a group's header row (used when collapsing from a child).
    fn select_header(&mut self, gi: usize) {
        if let Some(pos) = self
            .visible_rows()
            .iter()
            .position(|r| matches!(r, Row::Header(g) if *g == gi))
        {
            self.selected = pos;
        }
    }

    pub fn expand_selected(&mut self) {
        if let Some(Row::Header(gi)) = self.selected_row() {
            self.groups[gi].expanded = true;
        }
    }

    pub fn collapse_selected(&mut self) {
        match self.selected_row() {
            Some(Row::Header(gi)) => self.groups[gi].expanded = false,
            // Collapsing while sitting on a child: fold the parent and ride the
            // cursor back up to its header so we don't land on a vanished row.
            Some(Row::Child(gi, _)) => {
                self.groups[gi].expanded = false;
                self.select_header(gi);
            }
            _ => {}
        }
        self.clamp_selected();
    }

    pub fn toggle_selected(&mut self) {
        if let Some(Row::Header(gi)) = self.selected_row() {
            self.groups[gi].expanded = !self.groups[gi].expanded;
            self.clamp_selected();
        }
    }

    pub fn group_status(&self, gi: usize) -> GroupStatus {
        let mut st = GroupStatus::default();
        for &ei in &self.groups[gi].entry_indices {
            let e = &self.entries[ei];
            st.total += 1;
            match e.status {
                EntryStatus::Approved => st.approved += 1,
                EntryStatus::Skipped => st.skipped += 1,
                EntryStatus::Pending => {
                    st.pending += 1;
                    if !e.is_ready() {
                        st.unresolved += 1;
                    }
                }
            }
        }
        st
    }

    // ── Status changes ────────────────────────────────────────────────────────

    pub fn approve_selected(&mut self) {
        match self.selected_row() {
            // .clone() the index list so we're not borrowing self.groups while
            // we mutate self.entries — a Vec<usize> clone is trivial.
            Some(Row::Header(gi)) => {
                for ei in self.groups[gi].entry_indices.clone() {
                    self.approve_entry(ei);
                }
            }
            Some(Row::Single(ei)) | Some(Row::Child(_, ei)) => self.approve_entry(ei),
            None => {}
        }
    }

    pub fn skip_selected(&mut self) {
        match self.selected_row() {
            Some(Row::Header(gi)) => {
                for ei in self.groups[gi].entry_indices.clone() {
                    self.skip_entry(ei);
                }
            }
            Some(Row::Single(ei)) | Some(Row::Child(_, ei)) => self.skip_entry(ei),
            None => {}
        }
    }

    fn approve_entry(&mut self, ei: usize) {
        if let Some(e) = self.entries.get_mut(ei) {
            if e.is_ready() {
                e.status = EntryStatus::Approved;
            }
        }
    }

    fn skip_entry(&mut self, ei: usize) {
        if let Some(e) = self.entries.get_mut(ei) {
            e.status = EntryStatus::Skipped;
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
        let Some(idx) = self.selected_entry_idx() else { return };
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

    pub fn skipped_count(&self) -> usize {
        self.entries.iter().filter(|e| e.status == EntryStatus::Skipped).count()
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

/// Build display groups from the flat entry list. Entries are visited in order,
/// so groups appear in the order their first file does. Files sharing a
/// `series_key` join the same group; entries with no key each stand alone.
fn build_groups(entries: &[AppEntry]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    let mut lookup: HashMap<SeriesKey, usize> = HashMap::new();

    for (i, entry) in entries.iter().enumerate() {
        match &entry.sort.series_key {
            Some(key) => match lookup.get(key) {
                Some(&gi) => groups[gi].entry_indices.push(i),
                None => {
                    lookup.insert(key.clone(), groups.len());
                    groups.push(Group {
                        label: group_label(entry),
                        entry_indices: vec![i],
                        expanded: false,
                    });
                }
            },
            None => groups.push(Group {
                label: group_label(entry),
                entry_indices: vec![i],
                expanded: false,
            }),
        }
    }
    groups
}

/// A human label for a group: the parsed series title, falling back to the
/// destination folder name, then the raw filename.
fn group_label(entry: &AppEntry) -> String {
    if let Some(title) = &entry.sort.file.title {
        return title.clone();
    }
    match entry.effective_destination() {
        Destination::Existing(p) => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        Destination::New(name) => name.clone(),
        Destination::Unresolved => entry.sort.file.anime_filename.clone(),
    }
}

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
