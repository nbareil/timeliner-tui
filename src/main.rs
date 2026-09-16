use anyhow::{Result, bail};
use clap::Parser;
use fs2::FileExt;
use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
};
use timeliner_tui::{ioc::Rules, model::boundary, source, store::Store, ui, worker::Workers};

#[derive(Parser)]
#[command(
    version,
    about = "Browse, annotate, and export large forensic bodyfile timelines"
)]
struct Cli {
    /// Bodyfiles or extracted UAC directories.
    inputs: Vec<PathBuf>,
    /// Persistent investigation directory; omit inputs to resume.
    #[arg(long)]
    workspace: Option<PathBuf>,
    #[arg(long)]
    ioc_file: Option<PathBuf>,
    #[arg(long)]
    after: Option<String>,
    #[arg(long)]
    before: Option<String>,
    #[arg(long)]
    tz: Option<chrono_tz::Tz>,
    /// True-color appearance; F11 switches themes in the interface.
    #[arg(long, value_enum)]
    theme: Option<timeliner_tui::theme::Theme>,
    /// Override a source's hostname: --host SOURCE HOST (repeatable).
    #[arg(long, num_args = 2, value_names = ["SOURCE", "HOST"])]
    host: Vec<String>,
    /// Explicit legacy mode: read hostnames from nonempty first fields.
    #[arg(long)]
    host_from_md5: bool,
    /// Validate and relink a workspace source: --relink SOURCE_ID BODYFILE.
    #[arg(long, num_args = 2, value_names = ["SOURCE_ID", "BODYFILE"])]
    relink: Vec<String>,
    /// Build/update the workspace index without opening the terminal interface.
    #[arg(long)]
    index_only: bool,
    /// Print source IDs and parse diagnostics without opening the interface.
    #[arg(long)]
    diagnostics: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut inputs = Vec::new();
    for path in &cli.inputs {
        let canonical = path.canonicalize()?;
        let host = cli
            .host
            .chunks_exact(2)
            .find(|pair| PathBuf::from(&pair[0]).canonicalize().ok().as_ref() == Some(&canonical))
            .map(|pair| pair[1].as_str());
        let input = source::resolve(path, host, cli.host_from_md5)?;
        if !inputs.iter().any(|i: &source::Input| i.path == input.path) {
            inputs.push(input);
        }
    }
    if inputs.is_empty() && cli.workspace.is_none() {
        bail!("Provide bodyfiles/UAC directories or --workspace to resume an investigation");
    }
    let root = cli
        .workspace
        .unwrap_or_else(|| source::default_workspace(&inputs));
    if inputs.is_empty() && !root.join("index.sqlite").is_file() {
        bail!("No existing investigation at {}", root.display());
    }
    fs::create_dir_all(&root)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("workspace.lock"))?;
    lock.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("Workspace is already open: {}", root.display()))?;
    let existing = root.join("index.sqlite").exists();
    let store = Store::open(&root)?;
    for pair in cli.relink.chunks_exact(2) {
        store.relink(pair[0].parse()?, &PathBuf::from(&pair[1]))?;
    }
    if cli.diagnostics {
        for s in store.sources()? {
            println!("#{} {} {}", s.id, s.host, s.path.display());
        }
        println!("{}", store.diagnostics(1000)?);
        for warning in store.source_warnings()? {
            eprintln!("{warning}");
        }
        return Ok(());
    }
    if !cli.relink.is_empty() && inputs.is_empty() && !cli.index_only {
        println!("Source links validated and updated.");
        return Ok(());
    }
    let ioc_path = cli.ioc_file.or_else(|| {
        if existing {
            return None;
        }
        let path = PathBuf::from(std::env::var_os("HOME")?).join("ioc.filenames.yaml");
        path.is_file().then_some(path)
    });
    let rules = ioc_path.as_deref().map(Rules::load).transpose()?;
    let session = store.session()?;
    let theme = cli.theme.unwrap_or(session.theme);
    let tz = cli.tz.unwrap_or(session.timezone.parse()?);
    let initial = if cli.after.is_some() || cli.before.is_some() {
        let mut view = session
            .views
            .get(session.active)
            .cloned()
            .unwrap_or_default();
        if let Some(after) = cli.after {
            view.after = boundary(&after, tz, false)?;
        }
        if let Some(before) = cli.before {
            view.before = boundary(&before, tz, true)?;
        }
        view.top = None;
        view.selected = 0;
        timeliner_tui::model::validate_view(&view)?;
        Some(view)
    } else {
        None
    };
    drop(store);
    let workers = Workers::start(
        root.clone(),
        inputs,
        rules,
        initial,
        cli.tz.map(|tz| tz.to_string()),
        cli.theme,
    );
    if cli.index_only {
        loop {
            match workers.messages.recv()? {
                timeliner_tui::worker::Message::Ready(session, warnings) => {
                    workers
                        .commands
                        .send(timeliner_tui::worker::Command::Save(session))?;
                    println!("Indexed workspace: {}", root.display());
                    if !warnings.is_empty() {
                        eprintln!("{warnings}");
                    }
                    break;
                }
                timeliner_tui::worker::Message::Progress(_, message) => eprintln!("{message}"),
                timeliner_tui::worker::Message::Error(error) => {
                    workers.shutdown();
                    bail!("{error}");
                }
                _ => {}
            }
        }
        workers.shutdown();
    } else {
        let result = ui::run(&workers, theme);
        workers.shutdown();
        result?;
    }
    Ok(())
}
