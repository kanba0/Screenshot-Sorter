# Screenshot sorter

sorts MPV anime screenshots into per-series folders.

parses screenshot filenames to extract the series title, fuzzy-matches against
existing folders in the destination directory, and falls back to an AniList
lookup for anything it can't match locally. presents everything in a TUI for
review before moving any files.

## build

```
cargo build --release
```

binary lands at `target/release/ssort`.

## usage

```
ssort [--source <dir>] [--dest <dir>]
```

both flags are optional and default to the current directory. `--source` is
where your unsorted screenshots live; `--dest` is where your series folders are.
if both are the same directory you only need `--source`.

```
ssort --source ~/Pictures/Screenshots --dest ~/Pictures/Anime
```

## tui controls

| key | action |
|-----|--------|
| `↑` `↓` / `j` `k` | navigate |
| `g` / `G` | jump to top / bottom |
| `a` | approve selected |
| `A` | approve all matched entries |
| `s` | skip selected |
| `e` | edit destination folder name |
| `c` | commit approved moves |
| `q` | quit |

editing a destination applies to all screenshots from the same series at once.
new folders are created automatically on commit.

## notes

- only picks up files matching MPV's screenshot naming format
  (`filename_HH.MM.SS_YYYY-MM-DD_HH.MM.SS.ext`) which is an mpv setting I use
- AniList lookups are rate-limited to ~1 per second; a large batch of unknown
  series will take a moment
- screenshots are never moved until you explicitly commit