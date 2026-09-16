# Timeliner TUI

A terminal workbench for investigating forensic bodyfile timelines. Open bodyfiles
or extracted UAC collections, browse a merged timeline, assess records, and export
the original evidence records with host and annotation metadata.

This is a separate Rust application. It carries over Timeliner's UTC time
semantics, MACB grouping, and escaped-field handling. Records retain their source
identity, including duplicates on the same host or across different hosts.

## Run with Nix

```sh
# One bodyfile
nix run path:. -- timeline.body

# Multiple extracted UAC directories or bodyfiles
nix run path:. -- /cases/server-a /cases/server-b/bodyfile/bodyfile.txt

# Put the index on a disk with sufficient free space
nix run path:. -- --workspace /analysis/case-one \
  --ioc-file ~/ioc.filenames.yaml /cases/server-a /cases/server-b

# Resume an investigation without supplying its sources again
nix run path:. -- --workspace /analysis/case-one

# Build the index without opening a terminal interface
nix run path:. -- --index-only --workspace /analysis/case-one timeline.body
```

`path:.` also works before new files have been added to Git. After committing the
project, `nix run .` works too. Flake outputs support `x86_64-linux` and
`aarch64-linux`; each platform needs a compatible builder.

The initial import indexes the complete input and displays progress. Reopening
reuses that index. Source bodyfiles remain external and read-only: the workspace
does **not** copy them.

If an import is interrupted, resuming the workspace restarts that incomplete
source before enabling browsing. Already completed sources and their annotations
are retained.

Without `--workspace`, the application chooses a stable directory under
`$XDG_DATA_HOME/timeliner-tui`, falling back to
`~/.local/share/timeliner-tui`. Tabs, locations, tags, bookmarks, and applied IOC
rules are saved automatically. Only one application may open a workspace at a
time.

## Appearance

The interface uses 24-bit RGB colors with coordinated light and dark palettes.
Light is the default. Press **F11** to switch themes, including inside dialogs;
the choice is saved with the workspace. Override it at launch with
`--theme light` or `--theme dark`.

Hosts have stable accent colors, MACB flags have distinct colors, and assessment
badges use amber, violet, and rose. Subtle row shading, an accent selection marker,
and explicit foreground/background pairs keep the timeline readable. Dialogs
use rounded borders, focused input fields, and a separate surface color.
No special icon font is required.

Previews below use synthetic records rendered by the actual UI:

![Light theme](docs/images/light.png)

![Dark theme](docs/images/dark.png)

## Navigation and assessment

The timeline shows timestamp, hostname, MACB flags, pathname, and a tag column at
the right edge. Amber paths identify IOC rule matches; the tag column shows the
current assessment. ENTER opens the full pathname, all 11 fields, source line and
byte offset, original record, and matching rules.

Directory paths end in `/`, or `\` for Windows-style paths, in the timeline and
Details. The bodyfile's mode field identifies directories; a separator already
present is not duplicated. Long directory rows retain an `…/` or `…\` marker when
clipped. These markers affect display only: filters, exclusions, hash lookup, and
exports use the original pathname. Existing investigations read mode information
from the original record when it is missing from the index.
See the [directory-row terminal capture](docs/images/directories.txt).

Entries whose bodyfile size field is zero show a teal `[0 B]` badge before the
pathname, including directories. The badge remains visible when a long path is
clipped. Existing investigations read missing size metadata from the original
record. See the
[zero-size terminal capture](docs/images/zero-size.txt).

| Key | Action |
| --- | --- |
| ↑ / ↓ | Scroll one row, keeping the same event selected while it is visible |
| j / k | Select the next / previous event; scroll one row at screen edges |
| Page Up / Page Down | Scroll a screen |
| g | Goto an ISO date/time; Enter centers the first matching event at or after it |
| gg / G | Jump to the first / last matching event |
| nm / nh / nd / nM / ny | Jump to the next minute / hour / day / month / year, centered |
| nt | Jump to the next tagged event, centered |
| bm / bh / bd / bM / by | Jump to the preceding minute / hour / day / month / year boundary, centered |
| bt | Jump to the previous tagged event, centered |
| zt / zz / zb | Put the selected event at the top / middle / bottom |
| z ENTER / z. / z- | Aliases for zt / zz / zb |
| SPACE | Toggle a file in the selection |
| TAB | Cycle untagged → SUSPICIOUS → IOC → MALICIOUS → untagged for the selection, or the focused file |
| ENTER / F3 | Record details |
| F1 | Help |
| F2 | Set/clear a tag or restore automatic assessment for the selection; also offers IOC rule reload |
| F4 | Begin/end timeframe and timezone |
| F5 / `/` | Path, host, timestamp-type, and bogus-time filters |
| F6 | Fork the current view |
| F7 | Global smart tabs; rename or close a tab |
| 1–9 (with Shift if needed) | Select tab by number |
| gt / gT | Next / previous tab |
| F8 | Bookmark list |
| F9 | Export |
| F10 | Toggle mouse capture for terminal text selection and copying |
| q / Ctrl+C | Quit |
| F11 | Switch light/dark theme |
| c | Collapse successive bursts starting at the selected event; expand a selected summary |
| Shift+Enter | Show only the selected burst's tagged events; `''` returns to the folded view |
| C (Shift+c) | Set the burst density threshold |
| x | Exclude all SPACE-selected pathnames, or the focused pathname |
| X (Shift+x) | Edit an exclusion for the selected entry's parent directory |
| u | Remove the most recent exclusion, including a whole batch |
| D | Malformed-record diagnostics |
| Escape | Close a dialog; otherwise clear the selection and cancel pending work |

Arrow keys keep the same event highlighted as the timeline scrolls. If that
event leaves the screen, selection moves to the nearest visible event.

`j` and `k` move the highlight between events. At the bottom or top of the screen,
they scroll one row and select the adjacent event, loading another page when needed.
They stop at the first or last matching event. Collapsed bursts stay folded and
count as one displayed row. See the [terminal capture](docs/images/edge-scrolling.txt).

The `z` shortcuts scroll around the selected event without changing its identity.
They load more rows when needed and stop at the beginning of the timeline if
there are too few preceding events. Near the end, empty space can remain below
the last event. Escape cancels a pending shortcut or page load.

Press `n`, then `m`, `h`, `d`, `M` (Shift+m), or `y` to move to the first
matching event at or after the next time boundary in the displayed timezone.
For example, `nh` from 10:25 jumps to the first match at or after 11:00;
`nM` jumps to the first day of the next month. Current filters stay active.
The destination is centered, with preceding events available above it.
If there are too few preceding events, it appears closer to the top.
If no later event matches, the view stays where it is. Escape cancels either
the shortcut or its query; `''` returns to the location before a successful jump.

Minute and hour jumps follow clock boundaries, including repeated hours during
daylight-saving changes. Day, month, and year jumps use local midnight; if that
midnight is skipped, they use the first valid instant afterward.
From a collapsed burst, a jump starts after the burst's final timestamp.
A destination hidden inside another burst opens that burst; unrelated folds
stay closed. See the [terminal capture](docs/images/time-jump.txt).

`nt` finds the next event assessed as SUSPICIOUS, IOC, or MALICIOUS, including
later events with the same timestamp. It respects the current filters, including
any selected tag categories, and centers the destination among the view's
surrounding events. From a burst summary, it searches after the burst's end. If there is
no later matching tagged event, the view stays where it is.

Use `b` instead of `n` to jump backward. `bm`, `bh`, `bd`, `bM`, and `by` find
the last matching event at or before the preceding clock or calendar boundary.
For example, `bh` from 10:25 targets 10:00; from exactly 10:00 it targets 09:00.
`bd` from midday targets that day's midnight, while `bM` targets the start of
that month. `bt` finds the previous tagged event, including within the same second.
Backward jumps center the destination and preserve filters. From a collapsed
burst, they search before its first timestamp; a burst containing the destination
opens to reveal it. If no earlier event matches, the view stays where it is.
See the [backward-jump terminal capture](docs/images/backward-jump.txt).

Press `q` to save and quit from the timeline, help, or record details. Ctrl+C
also quits from dialogs. In text fields, `q` is ordinary input; in bookmark
commands and the F8 list, it selects bookmark `q`. F10 has no action.
See the [terminal capture](docs/images/quit.txt).

Press **SPACE** to select or deselect the focused file. A `✓` appears beside all
timestamp entries for that source record, and the status line shows the number
of selected files. Move with `j`/`k` or the arrow keys and press SPACE on more
files; the selection survives scrolling and page loads.

With a selection, **TAB** and **F2** apply the chosen assessment to every selected
file. TAB advances the focused selected file's tag; if focus is outside the
selection, it advances the first selected source record's tag. Every selected
file receives that same tag. F2 can also clear their tags or restore each file's
automatic IOC assessment. With no selection, these actions affect the focused
file. Tags belong to the **whole original bodyfile record**, so its other
timestamp events carry the same assessment in every tab.

Press **x** to exclude all selected pathnames together; **u** undoes that batch.
Successful tagging or exclusion clears the selection markers. Failed actions
keep the selection so you can retry. Escape in the timeline, switching tabs, and
applying different filters or a timeframe clear the selection, as does reloading
IOC rules. Selections are not saved between runs. `X` continues to edit the focused
entry's parent directory.
Expand a burst summary before selecting its files.
See the [selection capture](docs/images/multi-selection.txt) and
[markers cleared after tagging](docs/images/selection-action.txt).

TAB switches fields when a dialog has focus.
Forms support Ctrl+U to clear a field. In the filter editor, F2 focuses a new
exclusion and Ctrl+D removes the focused exclusion; Enter applies the changes.

New workspaces start with Timeline and IOC tabs. The IOC tab shows records whose
current assessment is IOC across the whole investigation. Saved workspaces
restore their existing tabs.

Forking copies the current filters and location. The new tab is independent.
Smart tabs—All tagged, SUSPICIOUS, IOC, and MALICIOUS—start across the whole
investigation and update when tags change.

The header shows numbered tabs, marks the active tab with `●`, and displays its
position (for example, `Tab 2/4`). Use `1`–`9` to select a tab directly or `gt`/`gT`
to switch in order. When titles exceed the screen width, `<` and `>` indicate
hidden tabs; the active tab stays visible.

Use `ma` to set bookmark `a`, `'a` to jump to it, and `''` to return after a jump.
Bookmarked events show a `●` in the left margin, including on the selected row.
F8 lists each mark's date (`YYYY-MM-DD` in the selected timezone) and file path.
Marks `a` through `z` persist. If the current filters hide a bookmarked event, a
new unfiltered tab reveals it. Press `d` followed by the bookmark's letter (for
example, `da`) to delete it from the timeline or the F8 list. Escape cancels a
pending shortcut. Text registers and yank commands are not implemented.

## Collapsing filesystem activity bursts

Press **c** anywhere inside a wave of events. The application counts matching
events per second on the selected host, across all directories and MACB types
enabled in the current view. Starting at the selected second, it scans backward
and forward while each consecutive second exceeds the threshold. A sparse or
empty second ends the burst; no gaps are bridged. An isolated dense second also
qualifies. Density describes activity, not whether it was a system upgrade.

After each fold, the application tries to collapse the next visible entry.
It continues until that entry is at or below the threshold, is already a summary, or
there are no more entries. A sparse entry stops the sequence even if a later
burst could collapse. Each burst remains a separate summary.

The default threshold is **more than 100 events per second**. Press **C** to
change it to a positive whole number; the workspace saves the setting. Changing
the threshold affects subsequent collapse requests. Counts refer to timeline
events: a record's timestamp flags combined at the same second count as one event.
The current timeframe, path, host, assessment, and timestamp-type filters apply.

A summary shows the event count, beginning and end timestamps, combined MACB
flags, and counts of tagged events and IOC matches when present. Events from
other hosts stay in chronological order outside the group and can collapse in
turn if their next visible entry qualifies. The most recent summary stays
selected, with earlier entries visible above it when available so `k` can move
back into them. Press **Enter**, **F3**,
or **c** on the summary to expand it and return to the event where you requested
the fold. Bookmark jumps reveal their exact event. Expand a summary before
tagging or excluding an individual record.

Press **Shift+Enter** on a summary to show only its tagged events (SUSPICIOUS,
IOC, or MALICIOUS), starting with the earliest match. This narrows the current
tab to the burst's host and time range while keeping its other filters.
Press **`''`** to return to the folded view and its selection. If the burst has
no tagged events, the view stays where it is. Automatic IOC matches whose
assessment has been cleared are not tagged events.
Shift+Enter requires a terminal that reports the Shift modifier for Enter;
the application enables the enhanced keyboard protocol on supporting terminals.

Folds are saved with their tab and restored when reopening the investigation;
forking copies them independently. Expanding a group saves its expanded state.
On reopening, summary counts are refreshed from the current index, including
newly imported events within the saved interval. Changing the burst threshold
does not discard saved groups.

Applying filters clears that tab's folds. Tag edits keep unrelated folds closed
and update summary tag counts, including when another timestamp of a folded
record is assessed.
In a tag-filtered view, a fold is cleared only if the edit changes its membership;
other hosts becoming visible remain outside the fold. Reloading IOC rules clears
folds across tabs.
Exports include folded records according to the normal export filters.

Detection runs in the background; Escape cancels further work and keeps any
folds already displayed.
Large waves still take time to count on their first collapse. Only group
descriptors and bounded pages are retained in memory. For a folded interval
containing only the selected host's matching events, scrolling seeks past its
hidden records. Interleaved hosts can require scanning hidden candidates to
retrieve the other hosts' events.

See the [tagged-event drill-down](docs/images/tagged-burst.txt),
[burst summary](docs/images/bursts.txt), and
[successive collapse](docs/images/collapse-chain.txt) terminal captures, rendered
from synthetic data.

## Mouse and tmux

Mouse support is enabled by default. Press **F10** to turn it off for terminal
text selection and copying without activating the TUI's controls. Press **F10**
again to restore mouse support. The header shows **Mouse on** or **Mouse off**.
The shortcut works in dialogs and while loading; keyboard controls remain
available. Each launch starts with mouse support enabled.
See the [terminal capture with mouse support disabled](docs/images/mouse-disabled.txt).

With mouse support on:

| Gesture | Action |
| --- | --- |
| Wheel | Scroll three rows, keeping the selected event while it is visible |
| Left click / drag | Select an event |
| Double click / right click | Inspect the clicked event |
| Click a tab or its `<` / `>` overflow control | Switch tabs |
| Click a function button, menu choice, or timeframe/filter row | Open or activate that control |
| Click a form field | Focus it for typing |
| Click **Enter apply** / **Esc close** in a dialog | Apply / close |

The wheel scrolls help, record details, and bookmarks. In forms, it moves between
fields. An open dialog receives mouse input; clicks outside it do not change the
timeline.

Enable mouse input in `~/.tmux.conf`:

```tmux
set -g mouse on
```

To enable it in an existing tmux server:

```sh
tmux set -g mouse on
```

The application uses SGR mouse reporting, including coordinates beyond column
223, and reports dragging without tracking pointer movement over the screen.
Mouse modes are restored on exit, errors, and panics. Normal tmux mouse
forwarding works without enabling passthrough.

If custom tmux bindings intercept clicks or the wheel, they must forward events
to applications with `send-keys -M` when `#{mouse_any_flag}` is set. F10 releases
the application's mouse capture, allowing tmux to handle selection. You can
also enter tmux copy mode with your prefix followed by `[`. Leave copy mode
before pressing F10 to restore application mouse support. Terminal selection
overrides, often Shift, depend on your terminal.

## Time and path filters

UTC is the default timezone. `--tz America/New_York` changes display and boundary
interpretation. F4 changes the timeframe interactively. Blank boundaries are
unbounded; a date-only begin means midnight and a date-only end includes the
entire day. Ambiguous or nonexistent local times are rejected.

Press **g** to open Goto, enter a date/time such as `2026-09-16T12:34:56Z`,
and press **Enter**. Goto centers the first event at or after that time that
matches the current timeframe and filters. It also reveals a destination inside
a collapsed burst. Press `''` to return to the previous position.

Goto accepts `YYYY-MM-DD`, local date/times such as `2026-09-16T12:34` or
`2026-09-16 12:34:56`, and timestamps with `Z` or an explicit offset such as
`2026-09-16T12:34:56+02:00`. Without an offset, it uses the displayed timezone;
a date alone means midnight. Fractional seconds advance to the next whole second
when necessary. Ambiguous or nonexistent local times require an explicit offset.
Invalid input stays in the dialog for correction; Escape closes it. If no event
matches, the timeline stays at its current position.

The existing `gg`, `gt`, and `gT` shortcuts still work: in an empty Goto field,
press `g` for the beginning, `t` for the next tab, or `T` for the previous tab.
See the [Goto terminal capture](docs/images/goto.txt).

ENTER/F3 displays all four record timestamps in ISO 8601 format with the selected
timezone's offset, for example `2026-09-16T12:34:56+00:00`. All formatted field
values share one column. The original bodyfile line remains available below the
formatted details.

```sh
nix run path:. -- --after 2026-09-01 --before 2026-09-03 \
  --tz UTC timeline.body
```

F5 combines an optional inclusion regex, any number of exclusion regexes, host
selection, and MACB timestamp types. Exclusions are ORed; the other conditions
are combined with AND. Invalid regexes leave the working view intact. The syntax
is Rust regex syntax, without lookaround or backreferences.

Nonpositive timestamps are hidden by default. Enable them in F5 when needed.
Exclusions apply as pages are fetched from your current position, within the
selected timeframe. Adding or removing one does not build a full filtered
database. Use `x` or `X` to exclude a pathname or directory, `u` to undo the most
recent exclusion, or F5 to edit individual patterns.

`X` opens a directory editor prefilled from the selected entry. Each Backspace
moves up one directory; it stops at `/`, a drive root, or a network share root.
You can type to edit the path, use Delete to erase one character, or Ctrl+U to
clear it. Enter excludes that directory and its descendants; Escape cancels.
Directory paths are treated literally, so regex characters need no escaping.

Filter changes keep the selected event in view. If it is excluded, the highlight
moves to the next remaining event, or the preceding event at the end of the
timeline. Undo also restores your place after a filter produces no matches.

Searches run in the background and Escape cancels them. A very restrictive
filter can still require searching much of the selected timeframe to fill a
page or establish that there are no matches. Tag views reuse a separate tag
index; changing path or host filters does not rebuild it.

## UAC and hostnames

Passing an extracted UAC directory selects its `bodyfile/bodyfile.txt`.
Hostname discovery checks collected hostname command output, `uac.log`, and
collected `etc/hostname` files. The collection directory or bodyfile filename is
the fallback.

For UAC sources, **Enter/F3** also shows MD5 and SHA-1 values found in
`hash_executables/hash*.md5` and `hash_executables/hash*.sha1`. Each value includes
the manifest pathname and line number. Matching uses the exact pathname within
that source's package, so identical paths on different hosts stay separate.
The original bodyfile's first field is shown unchanged.

GNU checksum output and BSD/OpenSSL formats are supported, including spaces and
GNU-escaped pathnames. Identical values are deduplicated; conflicting values
remain visible. Missing optional manifests do not prevent inspection.
The first lookup builds a disk index; later lookups reuse it and refresh changed
manifests. Matching source lines are verified before their cached hashes are shown.
This also works when resuming an existing investigation or opening
`bodyfile/bodyfile.txt` directly.
See the [UAC Details terminal capture](docs/images/uac-details.txt).

```sh
# Repeat --host for explicit input-specific labels
nix run path:. -- --host /cases/server-a web-01 /cases/server-a

# Legacy bodyfiles with hostnames stored in their first field
nix run path:. -- --host-from-md5 combined.body
```

An explicit host override takes precedence over legacy mode. Legacy mode falls
back to the source label for empty, `0`, or `-` first fields. Neither mode changes
the original field. Shell globs work when expanded by the shell. Archives and
recursive discovery of collection containers are not supported.

## IOC YAML

On a new investigation, `--ioc-file` selects the rules. Otherwise,
`~/ioc.filenames.yaml` is loaded if present. Resumed workspaces use their saved
rules unless a replacement is explicitly supplied or loaded through F2.

```yaml
- filename: example-implant
  exclusion_path:
    - /approved/install/
- path: /tmp/example-payload
  exclusion_hash:
    - known-benign-hash
```

Filename and path matches are exact and case-sensitive. Filename extraction
recognizes `/` and `\`. Exclusion paths use prefix matching, including prefixes
that end within a directory name, matching the existing scanner's convention.
Hash exclusions are exact. A hit from any nonexcluded rule is sufficient.

Matching records automatically receive IOC. A manual tag—including an explicit
clear—overrides automation and survives rule reloads. F2 → `a` restores automatic
assessment. Invalid or cancelled reloads retain the previous rules. The personal
IOC list is not included in this repository.

## Export and source recovery

F9 defaults to all tagged records in the investigation. Select particular tags
or `current` scope to restrict it to the active view.

Export writes:

- The original bodyfile records, once per selected physical record.
- `<filename>.metadata.jsonl`, containing a versioned source/rule header and
  per-record hostnames, assessments, origins, checksums, and source/output byte
  ranges.

Distinct duplicate records remain distinct. All original fields, escaping,
encoding bytes, and line endings are preserved. If a source's final record lacks
a newline and another record follows it in the export, an extra separator is
inserted outside its original-byte range. Sidecar rule references are zero-based
indexes into the header's IOC rules.

Every exported record is checked against its import checksum. Existing output
files and source files are never replaced. A changed or unavailable source
prevents export of its records.

```sh
# Find source IDs and malformed-record locations
nix run path:. -- --workspace /analysis/case-one --diagnostics

# Relink a moved source, requiring the same complete-file SHA-256
nix run path:. -- --workspace /analysis/case-one \
  --relink 1 /new/location/bodyfile.txt
```

Relinking preserves record identities and annotations. Changed content must be
imported as a new source; annotations are never silently reassigned to it.

## Development

```sh
nix develop .
cargo run -- timeline.body
cargo build --release --locked
cargo test --locked
cargo test --locked --test viewport
cargo test --locked --test mouse
cargo test --locked --test bursts
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
# Optional integration check: Unix, Python 3, and tmux
python3 tests/support/tmux_mouse.py target/debug/timeliner-tui
nix build path:.
nix flake check path:.
```

The flake pins the development toolchain and package set. `Cargo.lock` pins Rust
dependencies. The flake checks build and test the package and run formatting and
Clippy checks.

Source modules separate bodyfile parsing, IOC rules, source access, SQLite
storage, indexed queries, export, background workers, and the terminal UI.
`src/theme.rs` defines the palettes; `src/ui/render.rs` handles presentation.
`src/ui/mouse.rs` handles pointer input and the controls registered during rendering.
`src/uac.rs` indexes and looks up collected executable hashes for the Details view.
`src/burst.rs` detects dense seconds and builds display pages containing events
or fold summaries; raw queries and exports continue to use `src/query.rs`.
`src/navigation.rs` calculates timezone-aware boundaries and finds time or tag
jump destinations.
`cargo run --example appearance -- dark` emits a JSON cell capture of synthetic
timeline data; append `details`, `uac-details`, `filters`, `tags`, `views`, `marks`,
or `goto` to preview dialogs, or `selection`, `directories`, and `zero-size` to preview
timeline markers.
Integration tests under `tests/` exercise evidence preservation, filtering,
assessment state, CLI behavior, and terminal rendering.
The tmux check uses a temporary socket, configuration, and investigation. It
sends mouse input through a real tmux client into a split pane and verifies
selection, scrolling, inspection, tab switching, resizing, F10 toggling, copying
text with mouse capture off, and terminal cleanup.

## Performance and current boundaries

The design targets 10–100 million source records, with up to four timestamp events
per record. SQLite caches, record buffers, and rendered rows are bounded.
Tag-view indexes and deduplicated export selections use disk storage; scrolling
uses indexed cursors instead of increasing SQL offsets. Path and host filters
are evaluated during pagination without copying matching events to disk.

Workspace size can exceed source size substantially. Choose local storage with
room for the main index, tag indexes, and temporary exports. Broad rule reloads
use an atomic transaction and can temporarily grow the SQLite WAL.

Run the deterministic benchmark on an **unused directory**:

```sh
cargo run --release --bin timeliner-bench -- \
  --records 1000000 --directory /analysis/bench-1m

# Repeat query measurements without reimporting
cargo run --release --bin timeliner-bench -- \
  --reuse --records 1000000 --directory /analysis/bench-1m
```

It reports import throughput, index size, process memory, p95 indexed page-query
latency, exclusion add/remove latency, and sparse path-search time. It retains
the generated data for inspection. Increase `--records` to `10000000` or
`100000000` on appropriately sized storage. See [benchmark results](docs/benchmarks.md)
for measured results and their limits.

The parser accepts integer epoch seconds and unsigned 64-bit sizes, matching the
original Timeliner's supported bodyfiles. Unsupported or malformed records are
reported with source offsets. Records larger than 16 MiB are reported as malformed
without loading the whole record into memory. Source bytes remain untouched.
