# Performance measurements

Measured on 2026-09-16 using optimized Rust builds on Linux x86_64, an
Intel Xeon Gold 6240 CPU, approximately 63 GiB RAM, and a local filesystem.
These are development-machine measurements, not isolated laboratory results;
other builds ran during parts of the import measurements.

## Baseline before interactive exclusions

The deterministic generator produces four distinct timestamp events per source
record, spread across a year. Each query measurement retrieves 100 rows; p95 is
taken from 200 cursor queries. The path-filter measurement constructs a new disk
cache for `/file42\.log$`. These baseline results describe the earlier
implementation that cached complete path-filter results.

| Source records | Timestamp events | Import | Page retrieval p95 | First path filter | Peak benchmark RSS | Main index |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1,000,000 | 4,000,000 | 41.18 s | 0.69 ms | 0.43 s | 42.4 MiB | 254.4 MiB |
| 10,000,000 | 40,000,000 | 528.85 s | 0.69 ms | 4.90 s | 42.5 MiB | 2.51 GiB |

The 1M page result was measured with `--reuse` after fixing cursor bounds.
Its initial query shape incorrectly let SQLite scan from the timeframe boundary.
Both query bounds now use `(epoch, record)` tuples. A regression test checks
SQLite's query plan to ensure both bounds use the full compound key.

RSS excludes the operating system's filesystem page cache. These measurements
do not establish cold-disk latency or performance for every filter selectivity.
Index sizes exclude source bodyfiles, membership caches, and temporary files.

## Interactive exclusions

The updated query path checks path and host filters while fetching timeline
pages. Only tag membership uses a reusable disk index. A fresh run with
1,000,000 source records and 4,000,000 timestamp events measured:

| Operation | Result |
| --- | ---: |
| Retrieve 100 rows, p95 | 0.48 ms |
| Add a directory exclusion and retrieve 100 rows, p95 | 0.55 ms |
| Remove the exclusion and retrieve 100 rows, p95 | 0.35 ms |
| Maximum candidates checked for an excluded page | 112 |
| New filter-cache files written | 0 bytes |
| Sparse `/file42\.log$` search | 15.58 s |
| Import | 39.64 s |

The exclusion measurements cover 200 add/remove pairs at different timeline
positions. Each exclusion removes one of 32 synthetic host directories. These
are local, warm-cache query timings; they exclude terminal transport and drawing.

Sparse searches are a trade-off: the test requests 100 rows but only 40 events
match that filename, so establishing the result requires examining all four
million events. This is slower than the old full-filter cache for that workload.
Restricting the timeframe reduces the search range, and Escape cancels a search.
The fast exclusion timings do not imply that arbitrary regexes or empty-result
searches finish in milliseconds.

## Terminal responsiveness

For 100 alternating `j`/`k` inputs in a local 120×30 PTY:

| Source records | Input to first terminal update, p95 | Application RSS |
| ---: | ---: | ---: |
| 1,000,000 | 1.73 ms | 8.6 MiB |
| 10,000,000 | 1.83 ms | 8.6 MiB |

The 10M workspace reopened to its first timeline rows in approximately 75 ms.
These figures measure warm local terminal output, not remote SSH latency or the
time a terminal emulator takes to draw.

A separate PTY workflow check exercised tagging, details, bookmarks, forked and
smart tabs, bodyfile export, autosave, and restoration of terminal settings.
Automated rendering tests cover 120-, 80-, and 60-column layouts.

## Reproduce

```sh
cargo build --release --locked
target/release/timeliner-bench --records 1000000 \
  --directory /analysis/bench-1m
target/release/timeliner-bench --records 10000000 \
  --directory /analysis/bench-10m

# Query-only rerun; the original index remains available
target/release/timeliner-bench --reuse --records 10000000 \
  --directory /analysis/bench-10m
```

A fresh benchmark requires a nonexistent destination directory. `--reuse`
validates the expected record count and skips import. The current benchmark
reports `exclusion_add_p95_ms`, `exclusion_remove_p95_ms`,
`exclusion_candidates_max`, and `new_filter_cache_bytes` as well as page and
sparse-search timings. Path filters no longer reuse legacy membership caches.

## Validation limits

The implementation targets 10–100 million records, but **100M has not been
benchmarked**. Available storage was insufficient for that source, index, and
scratch-space workload. Validate it on a larger volume before claiming measured
support at that scale.

The earlier 10M workload met the planned process-memory and indexed-page latency
targets. The interactive-exclusion change has been measured at 1M records; its
timings must not be presented as new 10M measurements. The earlier local PTY
figures above have not been remeasured for this change.
