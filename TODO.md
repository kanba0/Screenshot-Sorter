# ssort — todo

## tui: series grouping
right now 200 screenshots from the same series show as 200 flat rows.
goal: collapsible group entries like `Sousou no Frieren (200 files)` that expand inline.

requires reworking the list model in app.rs — probably a new enum:
```rust
enum ListEntry {
    Group { series_key, label, entries: Vec<AppEntry>, expanded: bool },
    Single(AppEntry),
}
```
navigation, approve-all, editing, and commit all need to handle both levels.

## more filename patterns to test
- no release group: `Frieren_-_01.mkv`
- dot-separated: `Sword.Art.Online.S2E01.720p.mkv`
- scene-style: `Show.Name.S01E01.720p.BluRay.x264-GROUP.mkv`
- multi-ep: `S01E01-E02`, `01-02`
- groups with different conventions: EMBER, Erai-raws, etc.
- MPV configs that don't sanitize special characters

## anilist improvements
- **file-based cache** — don't re-query titles looked up in previous runs
  (saves time + avoids the 90req/min rate limit on big batches)
- **retry with stripped title** — if search fails, try removing part/cour markers
  ("Part 2", "Cour 2", "The Movie") and retry before giving up
- **more season formats** — SEASON_SUFFIX currently only strips `S\d+`,
  could extend to "2nd Season", "Part II", "Final Season" etc.

## known quirks
- slash in titles (Fate/strange Fake) becomes a space after MPV sanitization;
  AniList fuzzy search handles it but it's lossy
- `#[allow(dead_code)]` on CommitAction fields is intentional — placeholder for undo/log