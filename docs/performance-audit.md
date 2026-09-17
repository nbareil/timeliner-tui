# Performance audit

Streaming joined queries reduced the measured 1M-record sparse search from
17.1 s to 5.9 s, about 2.9× faster. Page retrieval p95 fell from 0.594 ms to
0.245 ms. The query worker now retains its reader between requests.

Audited on 2026-09-16, starting from commit `59f911b`. The review covered
pagination, background workers, imports, IOC matching, burst detection, export,
UAC hash lookup, and rendering. Measurements focused on timeline queries using
the repository's deterministic benchmark.

## Changes

### Stream candidate records through one query

`src/query.rs::page_omitting` previously fetched batches of event keys, then ran
a separate prepared statement for each event's record. It also allocated host
and path strings for every candidate, including rejected matches. The 1M-record
sparse-search workload examined four million events to find 40 matches.

Pagination now streams an event-to-record join and borrows text from SQLite
until a row matches. It stops when the requested page is full. Composite cursor
bounds and an event-first join preserve ordered index access without sorting
the whole timeframe. Exclusive bursts still seek past their hidden events.
Tag views join their existing disk cache to the main index through a read-only
connection.

The query-plan regression test checks forward and reverse access for both
ordinary and cached views. Existing tests cover filtering, equal timestamps,
folds, cancellation, and exports. A new test checks indexed and legacy display
metadata through the tag-cache join.

### Keep the query worker's reader open

`src/worker.rs::Workers::start` previously opened and configured a reader for
every page, jump, or burst request. That discarded its SQLite page and prepared
statement caches after every request.

The query thread now opens its reader lazily and reuses it. Statements finish
between requests, so later queries see committed annotations. A new worker
regression test exercises tag insertion and removal, tag-cache repair, and
cancellation followed by another query.

The existing 32 MiB main-reader cache budget remains in effect, but its cached
pages can now remain resident between requests. Tag queries also have bounded
caches for the tag database and attached record database. This tradeoff can
increase idle application memory after browsing compared with closing the
reader after every request.

## Measurements

See [performance measurements](benchmarks.md) for repeated baseline and
optimized results. The benchmark also reports a page query including a fresh
reader's open/configure/close cost, plus the sparse search's returned rows and
candidate count. These checks make the workload and connection overhead visible.

## Remaining opportunities

These are findings from code inspection, not measured speedup claims.

| Priority | Location | Finding and next measurement |
| --- | --- | --- |
| Medium | `src/export.rs::run`, `src/source.rs::read_record`, `src/store.rs::reference` | Export opens, seeks, and closes the source for each record; reference lookup executes the same SQL twice. Measure large exports before considering a bounded source-handle cache and a single reference lookup. Preserve checksum and source-change verification. |
| Medium | `src/query.rs::cache`, `update_caches_for_records` | Initial tag-cache construction fetches paths and hosts even though only tag membership is cached. Batch repair prepares statements repeatedly. Measure first-open and batch-tag workloads with dense annotations before changing cache construction. |
| Medium | `src/burst.rs::detect`, `restore` | Detection and restoration read each candidate and allocate host/path strings. Restoring overlapping saved ranges can revisit the same events. Measure dense bursts and many saved folds; pagination improvements do not establish faster burst detection. |
| Low | `src/ioc.rs::Rules::matches` | Path normalization allocates for each matching rule, including rules without path exclusions. Measure imports and reloads with realistic overlapping IOC rules before moving normalization outside the rule loop. |

Import already streams bounded batches, and the UI draws only when marked dirty.
Normal timeline rendering uses visible rows. No import, export, or rendering
speedup is claimed here.

## Session resume follow-up: 2026-09-17

The initial audit missed a sparse saved-view workload: a few matching rows at
the saved position followed by a long excluded interval. Startup waited to fill
the entire prefetch buffer before displaying those available rows.

Ordinary browsing now returns a partial page after checking 4,096 candidates
when an event or fold summary is available. The UI keeps scrolling enabled past
a partial page and fills missing screen rows in the background. Background
requests stop once the visible window is full, preserve the selected event,
and can be cancelled or superseded by navigation. Scrolling into an active fill
reuses its request instead of restarting the search. At the end of the timeline,
earlier rows fill the remaining space when available.
Summaries beyond the scanned interval wait until that
interval has been searched. Empty searches and navigation requiring complete
context continue until they find their destination or exhaust the range.
Raw queries and exports still retrieve complete pages.

A read-only check of the reported workspace using debug code returned its nine
initial rows after 4,096 candidates in 52 ms. Refreshing its saved folds took
381 ms separately. This was one local measurement, not an end-to-end terminal
benchmark. Regression tests exercise reopening, both cursor directions, later
matches across excluded gaps, and partial pages containing saved folds.

A subsequent read-only check of the current saved position exercised the whole
50-row window: it filled automatically in 12.8 s using debug code, after checking
1,700,309 candidates. Rows appeared progressively during the search; the work
stopped at 50 visible rows. This measurement includes refreshing saved folds and
does not include terminal rendering.

## Debug startup follow-up: 2026-09-17

The next comparison pinned one copy of the current saved session and queried
the workspace read-only. Its active view had 31 exclusions and 17 saved folds.
Fetching 50 display rows required 1,621,570 candidate checks. The query and fold
code from `59f911b2c72a40ea98593a3a662b84e3451789ec` returned the same 50
event/summary keys as the new code.

| Query implementation and build | Restore folds and fetch 50 rows |
| --- | ---: |
| `59f911b`, original unoptimized debug settings | 25.86 s |
| `1709379`, original unoptimized debug settings | 11.44 s |
| Combined exclusions and fold-range pruning, unoptimized | 10.28 s |
| Same query changes, development optimization level 1 | 2.47 s, 2.47 s |

These are sequential measurements with warm filesystem caches, including reader
setup and fold restoration. They do not reproduce the user's historical instant
load: the snapshot is the current saved position, not the position from that
earlier run.

The changes are:

- Compile multiple exclusions into a `RegexSet`, retaining separate matching
  if their combined size exceeds the regex limit. Individual flags, Unicode,
  directory boundaries, and invalid-expression errors retain their behavior.
  Fold restoration fell from about 390 ms to 155 ms before compiler tuning.
- Remove exclusive folds outside the query bounds from the candidate loop.
  All 17 saved folds preceded this cursor; checking them again for each of
  1.6 million candidates served no purpose.
- Set development optimization to level 1. This optimizes the application and
  its dependencies, including bundled SQLite, at the existing
  `target/debug/timeliner-tui` path. Debug information, assertions, and overflow
  checks remain enabled. `CARGO_PROFILE_DEV_OPT_LEVEL=0 cargo build` restores
  an unoptimized build when needed for debugging.

A separate probe drove the current UI's initial request and background fills
until all 50 screen rows were available. It fell from 12.26 s before these
changes to 2.53–2.54 s afterward, with the initial row available in 63–82 ms.
Both versions checked 1,651,152 candidates across the partial requests.
This measures a filled viewport, including saved-fold restoration; command-line
source resolution, controller startup, and terminal rendering are excluded.
No saved session or evidence was modified by these probes.

## Validation and limits

Validation used the pinned Nix development environment:

```sh
nix develop . --command cargo test --locked --offline
nix develop . --command cargo fmt --all -- --check
nix develop . --command cargo clippy --locked --offline --all-targets -- -D warnings
nix develop . --command cargo build --release --locked --offline
python3 tests/support/tmux_mouse.py target/release/timeliner-tui
```

At the initial audit, all 212 tests passed, as did formatting, Clippy, the release
build, and the tmux integration check.

For the session-resume and background-fill fixes, all 222 tests passed, along with
formatting, Clippy, the debug build, and the tmux check using
`target/debug/timeliner-tui`.

For the debug startup follow-up, all 225 tests passed, including new comparisons
of combined and individual exclusion matching and the combined-size fallback.
Formatting, strict Clippy, the debug build, and the tmux integration check passed.

The measurements use local synthetic data and a read-only saved workspace, with
warm filesystem caches. Sparse search remains proportional to the candidates
examined. Cold storage, dense tag views, end-to-end terminal latency, and
10M/100M workloads were not measured
for this change. The database schema and evidence-verification rules are
unchanged.
