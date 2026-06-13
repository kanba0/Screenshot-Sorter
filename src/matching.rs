use std::path::{Path, PathBuf};

use anyhow::Result;
use strsim::jaro_winkler;

use crate::anilist::{AniListClient, AniListEntry};
use crate::parse::ParsedFile;

const FUZZY_THRESHOLD: f64 = 0.88;
const HIGH_CONFIDENCE: f64 = 0.94;
// Score handed to a token-prefix containment match. Above FUZZY_THRESHOLD so it
// wins, below HIGH_CONFIDENCE so it surfaces as `fuzzy~` (please-review) — a
// folder being a prefix of the title is strong, but not "don't even look" sure.
const CONTAINMENT_SCORE: f64 = 0.90;
const ANILIST_DELAY_MS: u64 = 700;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum MatchSource {
    Exact,
    Fuzzy(f64),
    AniListExact(String),
    AniListFuzzy(f64, String),
}

impl MatchSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Fuzzy(s) if *s >= HIGH_CONFIDENCE => "fuzzy",
            Self::Fuzzy(_) => "fuzzy~",
            Self::AniListExact(_) => "anilist",
            Self::AniListFuzzy(s, _) if *s >= HIGH_CONFIDENCE => "anilist",
            Self::AniListFuzzy(_, _) => "anilist~",
        }
    }

    pub fn matched_title(&self) -> Option<&str> {
        match self {
            Self::AniListExact(t) | Self::AniListFuzzy(_, t) => Some(t),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Destination {
    Existing(PathBuf),
    New(String),
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SeriesKey {
    AniListId(u32),
    NormalizedTitle(String),
}

#[derive(Debug, Clone)]
pub struct SortEntry {
    pub file: ParsedFile,
    pub destination: Destination,
    pub source: Option<MatchSource>,
    pub anilist: Option<AniListEntry>,
    pub series_key: Option<SeriesKey>,
}

// ── Folder tree ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct FolderEntry {
    pub path: PathBuf,
    pub normalized: String,
}

pub struct FolderTree {
    pub entries: Vec<FolderEntry>,
}

pub fn build_folder_tree(root: &Path) -> FolderTree {
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_owned(), 0)];
    let mut entries: Vec<FolderEntry> = Vec::new();

    while let Some((dir, depth)) = stack.pop() {
        if depth > 1 {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy();
            let normalized = normalize(&name);
            entries.push(FolderEntry { path: path.clone(), normalized });
            stack.push((path, depth + 1));
        }
    }

    FolderTree { entries }
}

// ── Matching pipeline ─────────────────────────────────────────────────────────

pub fn run_pipeline(
    files: Vec<ParsedFile>,
    tree: &FolderTree,
    anilist: &mut AniListClient,
) -> Result<Vec<SortEntry>> {
    use std::collections::HashMap;

    // Stage 1: group files by normalised title.
    // Every file from the same series gets one folder lookup instead of one per file.
    // HashMap<normalised_key, (original_title, files_in_this_group)>
    let mut groups: HashMap<String, (String, Vec<ParsedFile>)> = HashMap::new();
    for file in files {
        let title = effective_title(&file).to_string();
        let key = normalize(&title);
        groups.entry(key).or_insert_with(|| (title, Vec::new())).1.push(file);
    }

    // Stage 2: resolve each unique group — local match first, AniList fallback.
    // HashMap<normalised_key, (Destination, MatchSource?, AniListEntry?)>
    type GroupResult = (Destination, Option<MatchSource>, Option<AniListEntry>);
    let mut resolved: HashMap<String, GroupResult> = HashMap::new();
    let mut needs_anilist: Vec<(String, String)> = Vec::new();

    for (key, (title, _)) in &groups {
        if let Some((path, source)) = match_in_tree(title, tree) {
            resolved.insert(key.clone(), (Destination::Existing(path), Some(source), None));
        } else {
            needs_anilist.push((key.clone(), title.clone()));
        }
    }

    if !needs_anilist.is_empty() {
        eprintln!("  Querying AniList for {} unique title(s)...", needs_anilist.len());
    }

    for (i, (key, title)) in needs_anilist.iter().enumerate() {
        eprint!("  [{}/{}] {}... ", i + 1, needs_anilist.len(), title);

        let al_entry = match anilist.search(title) {
            Ok(r) => {
                eprintln!("{}", r.as_ref().and_then(|r| r.romaji.as_deref()).unwrap_or("not found"));
                r
            }
            Err(e) => { eprintln!("error: {}", e); None }
        };

        let (destination, source) = if let Some(ref al) = al_entry {
            let mut found = None;
            for variant in al.all_titles() {
                if let Some((path, src)) = match_in_tree_anilist(variant, tree) {
                    found = Some((path, src));
                    break;
                }
            }
            match found {
                Some((path, src)) => (Destination::Existing(path), Some(src)),
                None              => (Destination::New(al.suggested_folder_name()), None),
            }
        } else {
            (Destination::New(title.clone()), None)
        };

        resolved.insert(key.clone(), (destination, source, al_entry));

        if i + 1 < needs_anilist.len() {
            std::thread::sleep(std::time::Duration::from_millis(ANILIST_DELAY_MS));
        }
    }

    // Stage 3: expand each group back into one SortEntry per file.
    let mut entries: Vec<SortEntry> = Vec::new();

    for (key, (_, files)) in groups {
        let (destination, source, al_entry) = resolved
            .remove(&key)
            .unwrap_or((Destination::Unresolved, None, None));

        let series_key = al_entry
            .as_ref()
            .map(|al| SeriesKey::AniListId(al.id))
            .or_else(|| Some(SeriesKey::NormalizedTitle(key)));

        for file in files {
            entries.push(SortEntry {
                destination: destination.clone(),
                source: source.clone(),
                anilist: al_entry.clone(),
                series_key: series_key.clone(),
                file,
            });
        }
    }

    entries.sort_by(|a, b| a.file.path.cmp(&b.file.path));
    Ok(entries)
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn effective_title(file: &ParsedFile) -> &str {
    file.title.as_deref().unwrap_or(&file.anime_filename)
}

fn match_in_tree(title: &str, tree: &FolderTree) -> Option<(PathBuf, MatchSource)> {
    let norm = normalize(title);
    if let Some(e) = tree.entries.iter().find(|e| e.normalized == norm) {
        return Some((e.path.clone(), MatchSource::Exact));
    }
    best_fuzzy(&norm, tree).map(|(path, score)| (path, MatchSource::Fuzzy(score)))
}

fn match_in_tree_anilist(title: &str, tree: &FolderTree) -> Option<(PathBuf, MatchSource)> {
    let norm = normalize(title);
    if let Some(e) = tree.entries.iter().find(|e| e.normalized == norm) {
        return Some((e.path.clone(), MatchSource::AniListExact(title.to_owned())));
    }
    best_fuzzy(&norm, tree)
        .map(|(path, score)| (path, MatchSource::AniListFuzzy(score, title.to_owned())))
}

fn best_fuzzy(norm: &str, tree: &FolderTree) -> Option<(PathBuf, f64)> {
    tree.entries
        .iter()
        .map(|e| (&e.path, match_score(norm, &e.normalized)))
        .filter(|(_, score)| *score >= FUZZY_THRESHOLD)
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(path, score)| (path.clone(), score))
}

/// Similarity of a normalized title to a folder name. Jaro-Winkler, but lifted
/// to CONTAINMENT_SCORE when one is a token-prefix of the other — that case
/// (folder "acca" vs title "acca 13 ku kansatsu ka") is a real match that
/// raw Jaro-Winkler underrates because of the large length difference.
fn match_score(title: &str, folder: &str) -> f64 {
    let jw = jaro_winkler(title, folder);
    if is_token_prefix(title, folder) {
        jw.max(CONTAINMENT_SCORE)
    } else {
        jw
    }
}

/// True when the shorter of the two space-separated token lists is a prefix of
/// the longer (either direction). Guards against single-character noise by
/// requiring the shorter side to carry at least two characters.
fn is_token_prefix(a: &str, b: &str) -> bool {
    let a: Vec<&str> = a.split(' ').filter(|t| !t.is_empty()).collect();
    let b: Vec<&str> = b.split(' ').filter(|t| !t.is_empty()).collect();
    let (short, long) = if a.len() <= b.len() { (&a, &b) } else { (&b, &a) };
    !short.is_empty()
        && short.iter().map(|t| t.len()).sum::<usize>() >= 2
        && short.iter().zip(long.iter()).all(|(x, y)| x == y)
}

pub fn normalize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
