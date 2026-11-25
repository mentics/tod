//! A workbench slot for Zeron's session view and composer, backed by local ACP.

mod acp;
mod bridge;
mod stubs;

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

#[cfg(feature = "live")]
use std::cell::RefCell;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zgx::{compose::prelude::*, reactive::Signal};
use zgx_desktop::{Application, FontData, WindowContext};
use zgx_script::ScriptHost;

use crate::{PANEL, control};

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct Context {
    pub key: String,
    pub title: String,
    pub description: String,
    pub cwd: Option<PathBuf>,
    pub note: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Session {
    id: Uuid,
    harness: acp::Harness,
    updated: i64,
    messages: Vec<bridge::Message>,
}

impl Session {
    fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            harness: acp::Harness::Claude,
            updated: 0,
            messages: Vec::new(),
        }
    }

    fn title(&self) -> String {
        self.messages
            .iter()
            .find_map(bridge::Message::opening)
            .map(|message| message.lines().next().unwrap_or_default())
            .filter(|line| !line.is_empty())
            .map(|line| line.chars().take(90).collect())
            .unwrap_or_else(|| "New chat".into())
    }
}

#[cfg(feature = "live")]
impl Session {
    pub(crate) fn from_tod(
        conversation: tod_store::conversation::Conversation,
        turns: Vec<tod_store::conversation::Turn>,
    ) -> Self {
        let harness = match conversation.platform.as_deref() {
            Some("openai" | "codex") => acp::Harness::Codex,
            _ => acp::Harness::Claude,
        };
        Self {
            id: conversation.id,
            harness,
            updated: conversation.updated_at,
            messages: turns
                .into_iter()
                .map(|turn| bridge::Message::from_tod(harness, conversation.model.as_deref(), turn))
                .collect(),
        }
    }
}

fn folder(root: &Path, key: &str) -> PathBuf {
    let name: String = key
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    root.join(name)
}

fn save(root: &Path, key: &str, session: &Session) -> io::Result<()> {
    let dir = folder(root, key);
    fs::create_dir_all(&dir)?;
    let file = dir.join(format!("{}.json", session.id));
    let temporary = dir.join(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut output = options.open(&temporary)?;
        serde_json::to_writer(&mut output, session)?;
        output.flush()?;
        output.sync_all()?;
        drop(output);
        fs::rename(&temporary, file)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn history(root: &Path, key: &str) -> io::Result<Vec<Session>> {
    let entries = match fs::read_dir(folder(root, key)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut sessions = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            sessions.push(serde_json::from_reader::<_, Session>(fs::File::open(
                path,
            )?)?);
        }
    }
    sessions.sort_by_key(|session| std::cmp::Reverse((session.updated, session.id)));
    Ok(sessions)
}

#[derive(Clone, PartialEq, Eq)]
struct Placement {
    context: Context,
    session: Uuid,
    archived: bool,
}

#[derive(Clone)]
pub(crate) struct Slot {
    host: Rc<ScriptHost>,
    bridge: Rc<bridge::Chat>,
    path: PathBuf,
    placement: Signal<Option<Placement>>,
    visible: Signal<bool>,
    expanded: Signal<bool>,
    problem: Signal<String>,
    updated: Signal<u64>,
    scroll: Signal<f32>,
    #[cfg(feature = "live")]
    previous: Rc<RefCell<Vec<Session>>>,
}

impl Slot {
    pub fn new(
        cx: &WindowContext,
        runtime: tokio::runtime::Handle,
        path: PathBuf,
    ) -> Result<Self, String> {
        static UI: zgx_script::Bundle = zgx_script::include_bundle!("tod-chat");
        let host = Rc::new(
            ScriptHost::bundled(&cx.ui.runtime, &UI, "panel.luau")
                .storage(path.join("panel-state"))
                .large_heap_goal(150),
        );
        host.window_size(cx.viewport.clone());
        host.shared("platform", std::env::consts::OS);
        stubs::install(&host);
        let updated = cx.ui.signal(0);
        let bridge = bridge::install(
            &host,
            cx.tasks.clone(),
            runtime,
            path.clone(),
            updated.clone(),
        );
        host.reload()?;
        Ok(Self {
            host,
            bridge,
            path,
            placement: cx.ui.signal(None),
            visible: cx.ui.signal(false),
            expanded: cx.ui.signal(false),
            problem: cx.ui.signal(String::new()),
            updated,
            scroll: cx.ui.signal(0.),
            #[cfg(feature = "live")]
            previous: Default::default(),
        })
    }

    pub fn can_open(&self, key: &str) -> bool {
        if self.bridge.running()
            && self.placement.with(|placement| {
                placement
                    .as_ref()
                    .is_some_and(|item| item.context.key != key)
            })
        {
            self.visible.set(true);
            self.problem
                .set("Stop the current turn or let it finish before opening another item.".into());
            return false;
        }
        true
    }

    pub fn open(&self, mut context: Context, #[cfg(feature = "live")] previous: Vec<Session>) {
        if !self.can_open(&context.key) {
            return;
        }
        if context.cwd.is_none() {
            let working = folder(&self.path.join("work"), &context.key);
            match fs::create_dir_all(&working).and_then(|()| working.canonicalize()) {
                Ok(path) => context.cwd = Some(path),
                Err(error) => {
                    context.note = Some(format!(
                        "Could not create a chat working directory. {error}"
                    ));
                }
            }
        }
        #[cfg(feature = "live")]
        self.previous.replace(previous);
        self.visible.set(true);
        if self.placement.with(|placement| {
            placement
                .as_ref()
                .is_some_and(|item| item.context == context)
        }) {
            self.updated.update(|revision| *revision += 1);
            return;
        }
        self.expanded.set(false);
        self.scroll.set(0.);
        let initial = match history(&self.path, &context.key) {
            Ok(sessions) => sessions.into_iter().next().unwrap_or_else(Session::new),
            Err(error) => {
                self.select(context, Session::new(), false);
                self.problem
                    .set(format!("Could not read chat history. {error}"));
                return;
            }
        };
        #[cfg(feature = "live")]
        if initial.updated == 0 {
            let previous = self.previous.borrow().first().cloned();
            if let Some(previous) = previous {
                self.select(context, previous, true);
                return;
            }
        }
        self.select(context, initial, false);
    }

    fn select(&self, context: Context, session: Session, archived: bool) {
        if self.bridge.running() {
            self.problem
                .set("Stop the current turn or let it finish before switching sessions.".into());
            return;
        }
        self.problem.set(String::new());
        let id = session.id;
        self.host
            .shared("tod.chat.writable", false)
            .set(zgx_script::Value::Bool(!archived && context.cwd.is_some()));
        let mut launch = context.clone();
        if archived {
            launch.cwd = None;
        }
        self.bridge.select(launch, session);
        self.placement.set(Some(Placement {
            context,
            session: id,
            archived,
        }));
    }

    fn sessions(&self, key: &str) -> io::Result<Vec<(Uuid, String, bool)>> {
        let result: Vec<_> = history(&self.path, key)?
            .into_iter()
            .map(|session| (session.id, session.title(), false))
            .collect();
        #[cfg(feature = "live")]
        let result = result
            .into_iter()
            .chain(
                self.previous
                    .borrow()
                    .iter()
                    .map(|session| (session.id, session.title(), true)),
            )
            .collect();
        Ok(result)
    }

    fn restore(&self, context: Context, id: Uuid, archived: bool) {
        if archived {
            #[cfg(feature = "live")]
            {
                let session = self
                    .previous
                    .borrow()
                    .iter()
                    .find(|session| session.id == id)
                    .cloned();
                if let Some(session) = session {
                    self.select(context, session, true);
                }
            }
            return;
        }
        match history(&self.path, &context.key) {
            Ok(sessions) => match sessions.into_iter().find(|session| session.id == id) {
                Some(session) => self.select(context, session, false),
                None => {
                    self.problem
                        .set("That saved chat is no longer available.".into());
                }
            },
            Err(error) => {
                self.problem
                    .set(format!("Could not read chat history. {error}"));
            }
        }
    }

    pub fn view(&self) -> View {
        let slot = self.clone();
        let visible = self.visible.clone();
        let placement = self.placement.clone();
        let body = self.clone();
        let message = self.problem.clone();
        column()
            .id("chat-slot")
            .w(410.)
            .min_w(310.)
            .h_full()
            .gap(6.)
            .rounded(7.)
            .bg(rgb(PANEL))
            .reactive_style(move || {
                if visible.get() {
                    Styles::new()
                } else {
                    Styles::new().hidden()
                }
            })
            .child(switch_or_none(
                move || placement.get(),
                move |placement, _| placement.map(|placement| slot.header(placement)),
            ))
            .child(switch(
                move || message.get(),
                |message, _| {
                    if message.is_empty() {
                        div()
                    } else {
                        text(message).px(10.).text_wrap(true)
                    }
                },
            ))
            .child(overlay().w_full().grow().min_h(0.).child(body.host.view()))
    }

    fn header(&self, placement: Placement) -> View {
        let close = self.visible.clone();
        let toggle = self.expanded.clone();
        let create = self.clone();
        let context = placement.context.clone();
        let expanded = self.expanded.clone();
        let updated = self.updated.clone();
        let list = self.clone();
        let mut title = row()
            .items_center()
            .gap(5.)
            .child(text(placement.context.title.clone()).text_wrap(true).grow());
        if placement.context.cwd.is_some() {
            title = title.child(
                control("New chat")
                    .id("chat-new")
                    .on_click(move || create.select(context.clone(), Session::new(), false)),
            );
        }
        column()
            .p(8.)
            .gap(6.)
            .child(
                title
                    .child(
                        control("Sessions")
                            .id("chat-sessions")
                            .expanded_when({
                                let expanded = toggle.clone();
                                move || expanded.get()
                            })
                            .on_click(move || {
                                toggle.update(|value| *value = !*value);
                            }),
                    )
                    .child(control("Close").id("chat-close").on_click(move || {
                        close.set(false);
                    })),
            )
            .children(
                placement
                    .context
                    .note
                    .clone()
                    .map(|notice| text(notice).text_wrap(true)),
            )
            .children(
                placement
                    .archived
                    .then(|| text("Saved TOD conversation · Read only").text_wrap(true)),
            )
            .child(switch_or_none(
                move || (expanded.get(), updated.get()),
                move |(expanded, _), _| expanded.then(|| list.session_list(&placement)),
            ))
    }

    fn session_list(&self, placement: &Placement) -> View {
        let items = match self.sessions(&placement.context.key) {
            Ok(items) => items,
            Err(error) => {
                return text(format!("Could not read chat history. {error}")).text_wrap(true);
            }
        };
        if items.is_empty() {
            return text("No saved sessions yet.");
        }
        scroll(self.scroll.clone())
            .id("chat-history")
            .max_h(160.)
            .child(
                column()
                    .gap(3.)
                    .children(items.into_iter().map(|(id, title, archived)| {
                        let slot = self.clone();
                        let context = placement.context.clone();
                        let selected = id == placement.session;
                        let label = if selected {
                            format!("✓  {title}")
                        } else {
                            title
                        };
                        control(label)
                            .id(format!("chat-session-{id}"))
                            .w_full()
                            .on_click(move || {
                                if !selected {
                                    slot.restore(context.clone(), id, archived);
                                }
                            })
                    })),
            )
    }
}

pub(crate) fn with_fonts(mut app: Application) -> Result<Application, Box<dyn std::error::Error>> {
    for bytes in [
        &include_bytes!("../../chat/fonts/Geist.ttf")[..],
        include_bytes!("../../chat/fonts/Geist-Italic.ttf"),
        include_bytes!("../../chat/fonts/Geist-Medium.ttf"),
        include_bytes!("../../chat/fonts/Geist-SemiBold.ttf"),
        include_bytes!("../../chat/fonts/Geist-Bold.ttf"),
        include_bytes!("../../chat/fonts/GeistMono.ttf"),
        include_bytes!("../../chat/fonts/GeistMono-Medium.ttf"),
        include_bytes!("../../chat/fonts/InstrumentSerif-Regular.ttf"),
        include_bytes!("../../chat/fonts/InstrumentSerif-Italic.ttf"),
    ] {
        app = app.font(FontData::new(Arc::<[u8]>::from(bytes))?);
    }
    Ok(app)
}
