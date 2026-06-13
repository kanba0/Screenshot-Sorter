# ssort — todo

## tui: series grouping — DONE
series now collapse into one header row (`Title  (n) → dest  k/n ✓`); expand with
`→`/`l`/`space` to act on individual files. implemented as a *derived view* rather
than the nested-enum sketch below: `entries` stays a flat `Vec<AppEntry>` (the
source of truth), `groups: Vec<Group>` holds indices into it, and `visible_rows()`
projects groups + expansion state into a `Vec<Row>` that `selected` indexes.
approve/skip are group-aware; editing already applied per-series.

original sketch (rejected — nesting entries inside groups complicates every
file-level operation for no gain):
```rust
enum ListEntry {
    Group { series_key, label, entries: Vec<AppEntry>, expanded: bool },
    Single(AppEntry),
}
```

possible follow-ups:
- remember expansion state across an edit (currently fine, but worth a look at scale)
- a key to expand/collapse *all* groups at once

## screenshot pattern + matching — DONE
- screenshot suffix regex now matches the real mpv `screenshot-template`
  (bracketed wall-clock, dot-or-dash date, 2-or-3-part playback time)
- fuzzy matcher does token-prefix containment, so a short folder name matches a
  longer parsed title; surfaced as a review-me match, not a confident one
- simplified anitomy preprocessing now that names keep their brackets
- strip disc/container extensions (iso/ifo/vob/etc.) so they don't leak into titles

still rough: disc rips with no title in the filename, some donghua naming, and
compilation movies with non-latin titles

## more filename patterns still untested
- no release group
- multi-episode ranges
- release groups with unusual tag conventions

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