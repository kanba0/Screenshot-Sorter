use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const ANILIST_URL: &str = "https://graphql.anilist.co";
const TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AniListEntry {
    pub id: u32,
    pub romaji: Option<String>,
    pub english: Option<String>,
    pub native: Option<String>,
    pub synonyms: Vec<String>,
}

impl AniListEntry {
    /// Preferred folder name: romaji first, then english, then a fallback.
    pub fn suggested_folder_name(&self) -> String {
        self.romaji
            .clone()
            .or_else(|| self.english.clone())
            .unwrap_or_else(|| "Unknown".to_string())
    }

    /// All title variants in priority order for folder matching.
    pub fn all_titles(&self) -> Vec<&str> {
        let mut titles = Vec::new();
        if let Some(r) = &self.romaji {
            titles.push(r.as_str());
        }
        if let Some(e) = &self.english {
            titles.push(e.as_str());
        }
        if let Some(n) = &self.native {
            titles.push(n.as_str());
        }
        for s in &self.synonyms {
            titles.push(s.as_str());
        }
        titles
    }
}

// ── GraphQL response types ────────────────────────────────────────────────────

#[derive(Deserialize)]
struct GqlResponse {
    data: Option<GqlData>,
}

#[derive(Deserialize)]
struct GqlData {
    #[serde(rename = "Media")]
    media: Option<GqlMedia>,
}

#[derive(Deserialize)]
struct GqlMedia {
    id: u32,
    title: GqlTitle,
    synonyms: Vec<String>,
}

#[derive(Deserialize)]
struct GqlTitle {
    romaji: Option<String>,
    english: Option<String>,
    native: Option<String>,
}

// ── Client ────────────────────────────────────────────────────────────────────

pub struct AniListClient {
    client: reqwest::blocking::Client,
    /// Cache keyed by lowercased query string. `None` = "AniList found nothing"
    /// (kept only for the current run; never persisted — see `save`).
    cache: HashMap<String, Option<AniListEntry>>,
    /// Where the on-disk cache lives. `None` if we couldn't resolve a cache dir,
    /// in which case caching silently degrades to in-memory only.
    path: Option<PathBuf>,
}

impl AniListClient {
    pub fn new() -> Self {
        let path = cache_path();
        // Warm the in-memory cache with anything we found in previous runs.
        let cache = path.as_deref().map(load_cache).unwrap_or_default();
        Self {
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(TIMEOUT_SECS))
                .build()
                .expect("failed to build HTTP client"),
            cache,
            path,
        }
    }

    /// Whether `title` is already in the cache (from disk or this run), so the
    /// caller can skip the rate-limit sleep and the "querying" UI for a hit.
    pub fn is_cached(&self, title: &str) -> bool {
        self.cache.contains_key(&title.to_lowercase())
    }

    pub fn search(&mut self, title: &str) -> Result<Option<AniListEntry>> {
        let key = title.to_lowercase();
        if let Some(cached) = self.cache.get(&key) {
            return Ok(cached.clone());
        }
        let result = self.query(title)?;
        self.cache.insert(key, result.clone());
        Ok(result)
    }

    fn query(&self, title: &str) -> Result<Option<AniListEntry>> {
        const QUERY: &str = "query ($search: String) { \
            Media(search: $search, type: ANIME) { \
                id \
                title { romaji english native } \
                synonyms \
            } \
        }";

        let body = serde_json::json!({
            "query": QUERY,
            "variables": { "search": title }
        });

        let resp = self
            .client
            .post(ANILIST_URL)
            .json(&body)
            .send()
            .with_context(|| format!("AniList request failed for '{}'", title))?;

        if resp.status().as_u16() == 429 {
            eprintln!("  AniList rate limit hit, waiting 60 s...");
            std::thread::sleep(Duration::from_secs(60));
            return self.query(title);
        }

        let gql: GqlResponse = resp
            .json()
            .with_context(|| format!("failed to parse AniList response for '{}'", title))?;

        Ok(gql.data.and_then(|d| d.media).map(|m| AniListEntry {
            id: m.id,
            romaji: m.title.romaji,
            english: m.title.english,
            native: m.title.native,
            synonyms: m.synonyms,
        }))
    }

    /// Write the successful lookups back to disk. Negative results (`None`) are
    /// deliberately dropped: a "not found" is usually our own parse miss, and we
    /// don't want to freeze that mistake into the cache.
    fn save(&self) -> Result<()> {
        let Some(path) = &self.path else { return Ok(()); };

        // Borrow the hits straight out of the map — no cloning needed, serde is
        // happy to serialize through references.
        let hits: HashMap<&String, &AniListEntry> = self
            .cache
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|entry| (k, entry)))
            .collect();
        if hits.is_empty() {
            return Ok(());
        }

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating cache dir {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(&hits)?;
        std::fs::write(path, json).with_context(|| format!("writing cache {}", path.display()))?;
        Ok(())
    }
}

/// Flush the cache when the client goes out of scope. `Drop` is Rust's RAII
/// hook (like a C++ destructor) — but it can't return a `Result` or take args,
/// so any failure has to be handled right here rather than propagated.
impl Drop for AniListClient {
    fn drop(&mut self) {
        if let Err(e) = self.save() {
            eprintln!("  warning: couldn't write AniList cache: {e}");
        }
    }
}

/// `$XDG_CACHE_HOME/ssort/anilist.json`, falling back to `~/.cache/...`.
/// Returns `None` if we can't even find a home dir, disabling disk caching.
fn cache_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("ssort").join("anilist.json"))
}

/// Load the persisted hits into the in-memory shape (`Some(entry)`). Any
/// problem — missing file, corrupt json — just yields an empty cache so a bad
/// file can never crash the run; worst case we start cold.
fn load_cache(path: &Path) -> HashMap<String, Option<AniListEntry>> {
    let Ok(bytes) = std::fs::read(path) else {
        return HashMap::new();
    };
    let Ok(stored) = serde_json::from_slice::<HashMap<String, AniListEntry>>(&bytes) else {
        return HashMap::new();
    };
    stored.into_iter().map(|(k, v)| (k, Some(v))).collect()
}
