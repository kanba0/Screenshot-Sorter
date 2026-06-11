use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

const ANILIST_URL: &str = "https://graphql.anilist.co";
const TIMEOUT_SECS: u64 = 15;

#[derive(Debug, Clone)]
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
    /// Cache keyed by lowercased query string
    cache: HashMap<String, Option<AniListEntry>>,
}

impl AniListClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(TIMEOUT_SECS))
                .build()
                .expect("failed to build HTTP client"),
            cache: HashMap::new(),
        }
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
}
