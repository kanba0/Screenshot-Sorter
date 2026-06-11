use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;

// MPV screenshot suffix: _HH.MM.SS_YYYY-MM-DD_HH.MM.SS
static MPV_SUFFIX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"_(\d{2}\.\d{2}\.\d{2})_\d{4}-\d{2}-\d{2}_\d{2}\.\d{2}\.\d{2}$").unwrap()
});
static BRACKET_TAGS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\s*[\[(][^\]\)]*[\])]").unwrap()
});
static EP_SUFFIX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\s*-\s*\d+\s*$").unwrap()
});
static VIDEO_EXT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\.(mkv|mp4|avi|m4v|webm|mov)$").unwrap()
});
static HEX_HASH: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"_[0-9A-Fa-f]{8}$").unwrap()
});
// Matches the episode separator " - 01", " - S01E02v2", etc.
static EP_SEPARATOR: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\s+-\s+(?:[Ss]\d+)?[Ee]?\d+[vV]?\d*\b").unwrap()
});
static SEASON_SUFFIX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\s+S\d+$").unwrap()
});

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp"];

#[derive(Debug, Clone)]
pub struct ParsedFile {
    pub path: PathBuf,
    /// Original anime filename with timestamps stripped
    pub anime_filename: String,
    /// Video playback time at the moment of capture
    pub video_time: String,
    /// Series title extracted by anitomy
    pub title: Option<String>,
    pub episode: Option<String>,
    pub release_group: Option<String>,
}

pub fn find_screenshots(dir: &Path) -> Vec<ParsedFile> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut results: Vec<ParsedFile> = read_dir
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_lowercase)?;
            if !IMAGE_EXTS.contains(&ext.as_str()) {
                return None;
            }
            parse_screenshot(&path)
        })
        .collect();

    results.sort_by(|a, b| a.path.cmp(&b.path));
    results
}

fn parse_screenshot(path: &Path) -> Option<ParsedFile> {
    let stem = path.file_stem()?.to_str()?;
    let caps = MPV_SUFFIX.captures(stem)?;

    let video_time = caps[1].to_string();
    // Everything before the suffix
    let anime_filename = stem[..caps.get(0)?.start()].to_string();

    if anime_filename.is_empty() {
        return None;
    }

    let (title, episode, release_group) = parse_with_anitomy(&anime_filename);

    Some(ParsedFile {
        path: path.to_owned(),
        anime_filename,
        video_time,
        title,
        episode,
        release_group,
    })
}

fn strip_tags(s: &str) -> String {
    let s = BRACKET_TAGS.replace_all(s, " ");
    let s = EP_SUFFIX.replace(s.trim(), "");
    s.trim().to_string()
}

/// Returns `(anitomy_input, title_hint)`.
/// `title_hint` is the series title we isolated ourselves — preferred over
/// anitomy's output because the Rust port can bleed episode tokens into titles.
fn preprocess_for_anitomy(s: &str) -> (String, Option<String>) {
    let s = VIDEO_EXT.replace(s, "");
    let s = HEX_HASH.replace(s.trim_end(), "");
    let s = s.replace('_', " ");
    let s = s.trim().to_string();

    if !s.contains('[') && !s.contains('(') {
        if let Some(ep_match) = EP_SEPARATOR.find(&s) {
            let prefix = s[..ep_match.start()].trim();
            let ep_token = &s[ep_match.start()..ep_match.end()];
            if let Some(space) = prefix.find(' ') {
                let group = &prefix[..space];
                let title = prefix[space + 1..].trim().to_string();
                let anitomy_input = format!("[{}] {}{}", group, &title, ep_token);
                return (anitomy_input, Some(title));
            }
        }
    }
    (s, None)
}

fn parse_with_anitomy(filename: &str) -> (Option<String>, Option<String>, Option<String>) {
    use anitomy::{Anitomy, ElementCategory};

    let (preprocessed, title_hint) = preprocess_for_anitomy(filename);
    let mut anitomy = Anitomy::new();
    let anitomy_input = if preprocessed.contains('.') {
        preprocessed.clone()
    } else {
        format!("{}.mkv", &preprocessed)
    };

    let elements = match anitomy.parse(&anitomy_input) {
        Ok(e) | Err(e) => e,
    };

    // title_hint (our own extraction) is more reliable than anitomy for MPV-style
    // names. For normal bracket-format names, title_hint is None and we use anitomy.
    // Either way, strip trailing season indicators (S2, S3…) so AniList can find them.
    let title = title_hint
        .or_else(|| elements.get(ElementCategory::AnimeTitle).map(str::to_owned))
        .or_else(|| {
            let cleaned = strip_tags(&preprocessed);
            if cleaned.is_empty() { None } else { Some(cleaned) }
        })
        .map(|t| SEASON_SUFFIX.replace(&t, "").trim().to_string());
    let episode = elements.get(ElementCategory::EpisodeNumber).map(str::to_owned);
    let group = elements.get(ElementCategory::ReleaseGroup).map(str::to_owned);

    (title, episode, group)
}
