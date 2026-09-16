# Performance measurements

Measured on 2026-09-16 using an optimized Rust build on Linux x86_64, an
Intel Xeon Gold 6240 CPU, approximately 63 GiB RAM, and a local filesystem.
These are development-machine measurements, not isolated laboratory results;
other builds ran during parts of the import measurements.

The deterministic generator produces four distinct timestamp events per source
record, spread across a year. Each query measurement retrieves 100 rows; p95 is
taken from 200 cursor queries. The path-filter measurement constructs a new disk
cache for `/file42\.log$`.

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
validates the expected record count and skips import; its path-filter result may
reuse an existing membership cache and must not be interpreted as a cold build.

## Validation limits

The implementation targets 10–100 million records, but **100M has not been
benchmarked**. Available storage was insufficient for that source, index, and
scratch-space workload. Validate it on a larger volume before claiming measured
support at that scale.

The tested 10M workload meets the planned process-memory and indexed-page
latency targets. The local PTY test meets the cached-navigation target. First
import and filter construction are separate background operations and are not
included in those interactive latency figures.
