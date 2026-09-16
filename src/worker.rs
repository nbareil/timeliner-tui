use crate::{
    export::{self, Export},
    ioc::Rules,
    model::{EventKey, Row, Session, Tag, View},
    query::{self, Cancellation},
    source::Input,
    store::Store,
    theme::Theme,
};
use anyhow::Result;
use chrono::TimeZone;
use chrono_tz::Tz;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
};

#[derive(Clone)]
pub struct PageRequest {
    pub generation: u64,
    pub view: View,
    pub anchor: Option<EventKey>,
    pub reverse: bool,
    pub inclusive: bool,
    pub limit: usize,
}

pub enum Command {
    Tag(i64, Option<Tag>),
    Save(Session),
    Detail(i64, Tz),
    Diagnostics,
    Export(Export),
    Reload(PathBuf),
    Stop,
}

pub enum Message {
    Ready(Session, String),
    Page(u64, Vec<Row>),
    Progress(u64, String),
    Changed,
    Detail(String),
    Status(String),
    Error(String),
    QueryError(u64, String),
}

pub struct Workers {
    pub commands: Sender<Command>,
    pub pages: Sender<PageRequest>,
    pub messages: Receiver<Message>,
    pub clock: Arc<AtomicU64>,
    pub jobs: Arc<AtomicU64>,
    stopping: Arc<AtomicBool>,
    handles: Vec<thread::JoinHandle<()>>,
}

impl Workers {
    pub fn start(
        root: PathBuf,
        inputs: Vec<Input>,
        initial_rules: Option<Rules>,
        initial: Option<View>,
        timezone: Option<String>,
        theme: Option<Theme>,
    ) -> Self {
        let (commands, rx) = mpsc::channel();
        let (pages, page_rx) = mpsc::channel::<PageRequest>();
        let (messages_tx, messages) = mpsc::channel();
        let clock = Arc::new(AtomicU64::new(0));
        let jobs = Arc::new(AtomicU64::new(0));
        let stopping = Arc::new(AtomicBool::new(false));
        let controller_stopping = stopping.clone();
        let thread_root = root.clone();
        let tx = messages_tx.clone();
        let job_clock = jobs.clone();
        let controller = thread::spawn(move || {
            let result = (|| -> Result<()> {
                let mut store = Store::open(&thread_root)?;
                let rules = initial_rules.clone().unwrap_or(store.rules()?);
                // Existing workspaces need a full re-evaluation if explicitly supplied new rules.
                if initial_rules.is_some() {
                    let generation = job_clock.load(Ordering::Relaxed);
                    store.reload_rules(&rules, |n| {
                        let _ = tx.send(Message::Progress(
                            0,
                            format!("Applying IOC rules: {n} records"),
                        ));
                        job_clock.load(Ordering::Relaxed) == generation
                            && !controller_stopping.load(Ordering::Relaxed)
                    })?;
                }
                store.save_rules(&rules)?;
                let mut imports = inputs;
                for pending in store.pending_inputs()? {
                    if !imports.iter().any(|input| input.path == pending.path) {
                        imports.push(pending);
                    }
                }
                for (index, input) in imports.iter().enumerate() {
                    let generation = job_clock.load(Ordering::Relaxed);
                    if controller_stopping.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    store.import(input, &rules, |done, total| {
                        let percent = if total == 0 {
                            100.0
                        } else {
                            done as f64 / total as f64 * 100.0
                        };
                        let _ = tx.send(Message::Progress(
                            0,
                            format!(
                                "Indexing {}/{}: {percent:.1}% · {:.1}/{:.1} MiB · {}",
                                index + 1,
                                imports.len(),
                                done as f64 / 1_048_576.0,
                                total as f64 / 1_048_576.0,
                                input.host
                            ),
                        ));
                        job_clock.load(Ordering::Relaxed) == generation
                    })?;
                }
                let mut session = store.session()?;
                if let Some(view) = initial {
                    session.views[session.active] = view;
                }
                if let Some(timezone) = timezone {
                    session.timezone = timezone;
                }
                if let Some(theme) = theme {
                    session.theme = theme;
                }
                let warnings = store.source_warnings()?.join(" · ");
                tx.send(Message::Ready(session, warnings))?;
                while let Ok(command) = rx.recv() {
                    let result = match command {
                        Command::Stop => break,
                        Command::Save(session) => store.save_session(&session),
                        Command::Tag(id, tag) => (|| -> Result<()> {
                            let revision = store.revision()?;
                            store.set_tag(id, tag)?;
                            query::update_caches(&store, id, revision)?;
                            tx.send(Message::Changed)?;
                            Ok(())
                        })(),
                        Command::Detail(id, timezone) => {
                            (|| -> Result<()> {
                                let detail = store.detail(id)?;
                                let labels = [
                                    "Hash / original first field",
                                    "Path",
                                    "Inode",
                                    "Mode",
                                    "UID",
                                    "GID",
                                    "Size",
                                    "Access time",
                                    "Modification time",
                                    "Change time",
                                    "Birth time",
                                ];
                                let mut text = format!(
                                    "Host: {}\nSource #{}: {}\nLine: {}  Byte offset: {}\nTag: {} ({})\nIOC rules: {:?}\n\n",
                                    detail.host,
                                    detail.source.id,
                                    detail.source.path.display(),
                                    detail.line,
                                    detail.offset,
                                    detail.tag.label(),
                                    if detail.manual { "manual" } else { "automatic" },
                                    detail.matches
                                );
                                for (index, (label, value)) in
                                    labels.iter().zip(&detail.record.fields).enumerate()
                                {
                                    let value = if index >= 7 {
                                        let epoch = detail.record.times[index - 7];
                                        timezone
                                            .timestamp_opt(epoch, 0)
                                            .single()
                                            .map(|date| date.to_rfc3339())
                                            .unwrap_or_else(|| {
                                                format!("{epoch} (outside supported date range)")
                                            })
                                    } else {
                                        value.clone()
                                    };
                                    text.push_str(&format!("{label}: {value}\n"));
                                }
                                let rules = store.rules()?;
                                for index in &detail.matches {
                                    if let Some(rule) = rules.rules.get(*index) {
                                        text.push_str(&format!(
                                            "\nIOC rule {index}: {}\n",
                                            serde_json::to_string(rule)?
                                        ));
                                    }
                                }
                                text.push_str("\nOriginal record (display escaped; export preserves bytes):\n");
                                text.push_str(&String::from_utf8_lossy(&detail.raw));
                                tx.send(Message::Detail(text))?;
                                Ok(())
                            })()
                        }
                        Command::Diagnostics => store
                            .diagnostics(200)
                            .and_then(|text| Ok(tx.send(Message::Detail(text))?)),
                        Command::Export(export) => {
                            let generation = job_clock.load(Ordering::Relaxed);
                            if controller_stopping.load(Ordering::Relaxed) {
                                continue;
                            }
                            let cancel = Cancellation {
                                clock: job_clock.clone(),
                                generation,
                            };
                            (|| -> Result<()> {
                                let reader = Store::reader(&thread_root)?;
                                let count = export::run(&reader, &export, &cancel, |n| {
                                    let _ = tx.send(Message::Progress(
                                        0,
                                        format!("Exporting: {n} records"),
                                    ));
                                })?;
                                tx.send(Message::Status(format!(
                                    "Exported {count} records to {}",
                                    export.output.display()
                                )))?;
                                Ok(())
                            })()
                        }
                        Command::Reload(path) => (|| -> Result<()> {
                            let rules = Rules::load(&path)?;
                            let generation = job_clock.load(Ordering::Relaxed);
                            if controller_stopping.load(Ordering::Relaxed) {
                                return Ok(());
                            }
                            store.reload_rules(&rules, |n| {
                                let _ = tx.send(Message::Progress(
                                    0,
                                    format!("Matching IOC rules: {n} records"),
                                ));
                                job_clock.load(Ordering::Relaxed) == generation
                            })?;
                            tx.send(Message::Changed)?;
                            tx.send(Message::Status(format!(
                                "Loaded {} IOC rules",
                                rules.rules.len()
                            )))?;
                            Ok(())
                        })(),
                    };
                    if let Err(error) = result {
                        let _ = tx.send(Message::Error(format!("{error:#}")));
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                let _ = tx.send(Message::Error(format!("{error:#}")));
            }
        });
        let page_clock = clock.clone();
        let query_thread = thread::spawn(move || {
            while let Ok(mut request) = page_rx.recv() {
                while let Ok(newer) = page_rx.try_recv() {
                    request = newer;
                }
                let cancel = Cancellation {
                    clock: page_clock.clone(),
                    generation: request.generation,
                };
                let result = (|| -> Result<Vec<Row>> {
                    let store = Store::reader(&root)?;
                    query::page(
                        &store,
                        &request.view,
                        query::Page {
                            anchor: request.anchor,
                            reverse: request.reverse,
                            inclusive: request.inclusive,
                            limit: request.limit,
                        },
                        &cancel,
                        |count| {
                            if count % 65536 == 0 {
                                let _ = messages_tx.send(Message::Progress(
                                    request.generation,
                                    format!("Filtering: {count} records scanned"),
                                ));
                            }
                        },
                    )
                })();
                if cancel.check().is_err() {
                    continue;
                }
                match result {
                    Ok(rows) => {
                        let _ = messages_tx.send(Message::Page(request.generation, rows));
                    }
                    Err(error) => {
                        let _ = messages_tx.send(Message::QueryError(
                            request.generation,
                            format!("{error:#}"),
                        ));
                    }
                }
            }
        });
        Self {
            commands,
            pages,
            messages,
            clock,
            jobs,
            stopping,
            handles: vec![controller, query_thread],
        }
    }
    pub fn cancel(&self) {
        self.clock.fetch_add(1, Ordering::Relaxed);
        self.jobs.fetch_add(1, Ordering::Relaxed);
    }
    pub fn shutdown(self) {
        self.stopping.store(true, Ordering::Relaxed);
        self.cancel();
        let _ = self.commands.send(Command::Stop);
        drop(self.commands);
        drop(self.pages);
        for handle in self.handles {
            let _ = handle.join();
        }
    }
}
