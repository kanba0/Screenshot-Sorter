use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;

// MPV screenshot suffix, from our screenshot-template:
//   %f_%wH.%wM.%wS_[%tY-%tm-%td_%tH.%tM.%tS]  ->  _HH.MM.SS_[YYYY-MM-DD_HH.MM.SS]
// The wall-clock date+time is bracketed (group 0 of the template is `%f`, the
// untouched source filename). We tolerate two legacy variants from older shots:
// a 2-part playback time (HH.MM) and a dot-separated date (YYYY.MM.DD).
static MPV_SUFFIX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"_(\d{2}\.\d{2}(?:\.\d{2})?)_\[\d{4}[-.]\d{2}[-.]\d{2}_\d{2}\.\d{2}\.\d{2}\]$").unwrap()
});
static BRACKET_TAGS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\s*[\[(][^\]\)]*[\])]").unwrap()
});
static EP_SUFFIX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\s*-\s*\d+\s*$").unwrap()
});
// Trailing source-file extension. Includes disc-image / DVD container types
// (iso, ifo, vob, m2ts, ts) so they don't bleed into the parsed title.
static VIDEO_EXT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\.(mkv|mp4|m4v|m2ts|webm|mov|avi|iso|ifo|vob|ts|wmv|flv|mpe?g)$").unwrap()
});
static HEX_HASH: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"_[0-9A-Fa-f]{8}$").unwrap()
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

/// Outcome of scanning a directory: the screenshots we parsed, plus a count of
/// image files we saw but couldn't parse (no MPV pattern) — i.e. junk we skip.
pub struct ScanResult {
    pub files: Vec<ParsedFile>,
    pub unmatched: usize,
}

pub fn find_screenshots(dir: &Path) -> ScanResult {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return ScanResult { files: Vec::new(), unmatched: 0 };
    };

    let mut files: Vec<ParsedFile> = Vec::new();
    let mut images_seen = 0usize;

    for entry in read_dir.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()).map(str::to_lowercase) else {
            continue;
        };
        if !IMAGE_EXTS.contains(&ext.as_str()) {
            continue;
        }
        images_seen += 1;
        if let Some(parsed) = parse_screenshot(&path) {
            files.push(parsed);
        }
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    let unmatched = images_seen - files.len();
    ScanResult { files, unmatched }
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

/// Strip the source extension, the release-group hex hash, and turn `_`
/// separators into spaces so anitomy sees a clean `[Group] Title - ep` string.
/// Our screenshot-template keeps the original brackets, so anitomy can take it
/// from here — no manual title reconstruction needed.
fn preprocess_for_anitomy(s: &str) -> String {
    let s = VIDEO_EXT.replace(s, "");
    let s = HEX_HASH.replace(s.trim_end(), "");
    let s = s.replace('_', " ");
    s.trim().to_string()
}

fn parse_with_anitomy(filename: &str) -> (Option<String>, Option<String>, Option<String>) {
    use anitomy::{Anitomy, ElementCategory};

    let preprocessed = preprocess_for_anitomy(filename);
    let mut anitomy = Anitomy::new();
    let anitomy_input = if preprocessed.contains('.') {
        preprocessed.clone()
    } else {
        format!("{}.mkv", &preprocessed)
    };

    let elements = match anitomy.parse(&anitomy_input) {
        Ok(e) | Err(e) => e,
    };

    // anitomy gives us the title; fall back to bracket-stripping if it bails.
    // Either way, strip trailing season indicators (S2, S3…) so AniList can find them.
    let title = elements.get(ElementCategory::AnimeTitle).map(str::to_owned)
        .or_else(|| {
            let cleaned = strip_tags(&preprocessed);
            if cleaned.is_empty() { None } else { Some(cleaned) }
        })
        .map(|t| SEASON_SUFFIX.replace(&t, "").trim().to_string());
    let episode = elements.get(ElementCategory::EpisodeNumber).map(str::to_owned);
    let group = elements.get(ElementCategory::ReleaseGroup).map(str::to_owned);

    (title, episode, group)
}
