use anyhow::Result;
use clap::Parser;
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    time::Instant,
};
use timeliner_tui::{
    ioc::Rules,
    model::{EventKey, Predicate, View},
    query::{self, Cancellation},
    source,
    store::Store,
};

#[derive(Parser)]
#[command(
    about = "Deterministic bodyfile import/query benchmark. Use a dedicated empty directory."
)]
struct Args {
    #[arg(long, default_value = "1000000")]
    records: u64,
    #[arg(long)]
    directory: PathBuf,
    /// Rerun queries against this benchmark's existing index.
    #[arg(long)]
    reuse: bool,
}
fn main() -> Result<()> {
    let args = Args::parse();
    let source_path = args.directory.join("benchmark.bodyfile");
    if !args.reuse {
        std::fs::create_dir(&args.directory)?;
        let mut file = BufWriter::new(File::create(&source_path)?);
        let mut random = 1234_u64;
        for i in 0..args.records {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let epoch = 1_600_000_000 + (random % 31_536_000) as i64;
            writeln!(
                file,
                "0|/var/log/host{}/file{}.log|{}|-rw-r--r--|0|0|{}|{}|{}|{}|{}",
                i % 32,
                i % 100000,
                i,
                i % 65536,
                epoch,
                epoch + 1,
                epoch + 2,
                epoch + 3
            )?;
        }
        file.flush()?;
        drop(file);
    }
    let mut store = Store::open(&args.directory.join("workspace"))?;
    let import = if args.reuse {
        let count: i64 = store
            .conn
            .query_row("SELECT count(*) FROM records", [], |r| r.get(0))?;
        anyhow::ensure!(
            count as u64 == args.records,
            "Existing record count is {count}; supply matching --records"
        );
        None
    } else {
        let input = source::resolve(&source_path, None, false)?;
        let start = Instant::now();
        store.import(&input, &Rules::default(), |_, _| true)?;
        Some(start.elapsed().as_secs_f64())
    };
    let cancel = Cancellation::never();
    let view = View::default();
    let mut times = Vec::new();
    let mut reopened_reader_times = Vec::new();
    let mut exclusion_add_times = Vec::new();
    let mut exclusion_remove_times = Vec::new();
    let mut exclusion_candidates_max = 0;
    let cache_bytes = || -> Result<u64> {
        std::fs::read_dir(store.root.join("views"))?
            .try_fold(0, |total, entry| Ok(total + entry?.metadata()?.len()))
    };
    let cache_bytes_before = cache_bytes()?;
    for i in 0..200 {
        let start = Instant::now();
        query::page(
            &store,
            &view,
            query::Page {
                anchor: Some(EventKey {
                    epoch: 1_600_000_000 + i * 150_000,
                    record: 0,
                }),
                limit: 100,
                inclusive: true,
                ..query::Page::default()
            },
            &cancel,
            |_| {},
        )?;
        times.push(start.elapsed().as_secs_f64() * 1000.0);
        let excluded = View {
            predicate: Predicate {
                exclude: vec![format!("^/var/log/host{}/", i % 32)],
                ..Predicate::default()
            },
            ..View::default()
        };
        let start = Instant::now();
        query::page(
            &store,
            &excluded,
            query::Page {
                anchor: Some(EventKey {
                    epoch: 1_600_000_000 + i * 150_000,
                    record: 0,
                }),
                limit: 100,
                inclusive: true,
                ..query::Page::default()
            },
            &cancel,
            |count| exclusion_candidates_max = exclusion_candidates_max.max(count),
        )?;
        exclusion_add_times.push(start.elapsed().as_secs_f64() * 1000.0);
        let start = Instant::now();
        query::page(
            &store,
            &view,
            query::Page {
                anchor: Some(EventKey {
                    epoch: 1_600_000_000 + i * 150_000,
                    record: 0,
                }),
                limit: 100,
                inclusive: true,
                ..query::Page::default()
            },
            &cancel,
            |_| {},
        )?;
        exclusion_remove_times.push(start.elapsed().as_secs_f64() * 1000.0);
        // Compare the same query with the former per-request reader lifecycle.
        let start = Instant::now();
        {
            let reader = Store::reader(&store.root)?;
            query::page(
                &reader,
                &view,
                query::Page {
                    anchor: Some(EventKey {
                        epoch: 1_600_000_000 + i * 150_000,
                        record: 0,
                    }),
                    limit: 100,
                    inclusive: true,
                    ..query::Page::default()
                },
                &cancel,
                |_| {},
            )?;
        }
        reopened_reader_times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    reopened_reader_times.sort_by(f64::total_cmp);
    exclusion_add_times.sort_by(f64::total_cmp);
    exclusion_remove_times.sort_by(f64::total_cmp);
    let filtered = View {
        predicate: Predicate {
            include: "/file42\\.log$".into(),
            ..Predicate::default()
        },
        ..View::default()
    };
    let start = Instant::now();
    let mut filter_candidates = 0;
    let filtered_rows = query::page(
        &store,
        &filtered,
        query::Page {
            limit: 100,
            ..query::Page::default()
        },
        &cancel,
        |count| filter_candidates = count,
    )?;
    let filter = start.elapsed().as_secs_f64();
    println!(
        "{}",
        serde_json::json!({"records":args.records,"import_seconds":import,"records_per_second":import.map(|seconds|args.records as f64/seconds),
        "page_p95_ms":times[190],"reopened_reader_page_p95_ms":reopened_reader_times[190],"path_filter_seconds":filter,
        "path_filter_rows":filtered_rows.len(),"path_filter_candidates":filter_candidates,
        "exclusion_add_p95_ms":exclusion_add_times[190],"exclusion_remove_p95_ms":exclusion_remove_times[190],
        "exclusion_candidates_max":exclusion_candidates_max,"new_filter_cache_bytes":cache_bytes()?.saturating_sub(cache_bytes_before),
        "index_bytes":std::fs::metadata(args.directory.join("workspace/index.sqlite"))?.len(),
        "memory":std::fs::read_to_string("/proc/self/status").unwrap_or_default().lines().filter(|l|l.starts_with("VmHWM:")||l.starts_with("VmRSS:")).collect::<Vec<_>>()})
    );
    Ok(())
}
