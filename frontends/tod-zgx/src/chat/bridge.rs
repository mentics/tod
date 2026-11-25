use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
};

use super::{Context, Session, save};
use serde_json::{Value as Json, json};
use uuid::Uuid;
use zgx_desktop::TaskSpawner;
use zgx_script::{ScriptHost, Shared, Value};

use super::acp::{self, Agent, Command, Config, Event, Harness, PermissionOption, ToolCall};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
enum Block {
    Text { id: String, text: String },
    Thought { id: String, text: String },
    Tools { id: String, tools: Vec<ToolCall> },
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Message {
    id: String,
    user: bool,
    /// Who answered (an assistant's reply).
    harness: Harness,
    /// The seam where the chat moved from one agent to another.
    switch: Option<(Harness, Harness)>,
    /// The model that wrote it (an assistant's reply).
    model: String,
    text: String,
    at: String,
    blocks: Vec<Block>,
    streaming: bool,
    started: f64,
    worked: Option<f64>,
}

impl Message {
    #[cfg(feature = "live")]
    pub(super) fn from_tod(
        harness: Harness,
        model: Option<&str>,
        turn: tod_store::conversation::Turn,
    ) -> Self {
        use tod_store::conversation::{ReplyPart, TurnRole};
        let id = format!("turn-{}", turn.seq);
        let user = turn.role == TurnRole::User;
        let mut blocks = turn
            .parts
            .into_iter()
            .enumerate()
            .map(|(index, part)| {
                let id = format!("{id}-{index}");
                match part {
                    ReplyPart::Text { text } => Block::Text { id, text },
                    ReplyPart::Thought { text } => Block::Thought { id, text },
                    ReplyPart::Tool {
                        id: tool_id,
                        title,
                        status,
                    } => Block::Tools {
                        id,
                        tools: vec![ToolCall {
                            id: tool_id,
                            title: Some(title),
                            status: Some(status),
                            ..Default::default()
                        }],
                    },
                }
            })
            .collect::<Vec<_>>();
        if !user
            && !turn.body.is_empty()
            && !blocks
                .iter()
                .any(|block| matches!(block, Block::Text { .. }))
        {
            blocks.push(Block::Text {
                id: format!("{id}-answer"),
                text: turn.body.clone(),
            });
        }
        let at = chrono::DateTime::from_timestamp_millis(turn.created_at)
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format("%H:%M")
                    .to_string()
            })
            .unwrap_or_default();
        Self {
            id,
            user,
            harness,
            switch: None,
            model: model.unwrap_or_default().into(),
            text: turn.body,
            at,
            blocks,
            streaming: false,
            started: turn.created_at as f64 / 1000.,
            worked: None,
        }
    }

    pub(super) fn opening(&self) -> Option<&str> {
        self.user.then_some(&self.text)
    }
}

struct Ask {
    request: u64,
    tool: ToolCall,
    options: Vec<PermissionOption>,
}

/// An agent that is running but not the one the chat is on.
struct Slot {
    harness: Harness,
    agent: Option<Agent>,
    state: &'static str,
    /// Why it is offline.
    error: Option<String>,
    config: Config,
    /// How many of the chat's messages its session has seen.
    seen: usize,
}

struct State {
    /// The agent the chat is on; its fields (`agent` .. `seen`) are the active
    /// slot's, swapped with a parked one on a switch.
    harness: Harness,
    parked: Vec<Slot>,
    seen: usize,
    agent: Option<Agent>,
    context: Context,
    id: Uuid,
    cwd: PathBuf,
    /// "loading" until the agent opens its session, "ready", "offline" if it
    /// could not start.
    state: &'static str,
    notice: Option<String>,
    config: Config,
    /// The access the page shows (supervised, edits, auto, full) and plan mode;
    /// together they are one ACP mode.
    access: &'static str,
    plan: bool,
    messages: Vec<Message>,
    running: bool,
    ask: Option<Ask>,
    todos: Vec<(String, String)>,
}

pub(super) struct Chat {
    state: RefCell<State>,
    path: PathBuf,
    generation: Cell<u64>,
    updated: zgx::reactive::Signal<u64>,
    tasks: TaskSpawner,
    runtime: tokio::runtime::Handle,
    active: Shared,
    cwd: Shared,
    title: Shared,
    session: Shared,
    messages: Shared,
    catalog: Shared,
}

/// The branches of the repository at `path`, the current one first (the page's
/// `chat.refs`: `{ name, current }`). Empty when it is no repository.
fn git_refs(path: &std::path::Path) -> Vec<Json> {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let current = git(&["branch", "--show-current"]);
    let mut names: Vec<String> = git(&[
        "for-each-ref",
        "--sort=-committerdate",
        "--format=%(refname:short)",
        "refs/heads",
    ])
    .lines()
    .map(str::to_string)
    .collect();
    if !current.is_empty() {
        names.retain(|n| *n != current);
        names.insert(0, current.clone());
    }
    names
        .into_iter()
        .map(|name| json!({ "current": name == current, "name": name }))
        .collect()
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0., |d| d.as_secs_f64())
}

fn clock() -> String {
    chrono::Local::now().format("%b %-d, %-I:%M %p").to_string()
}

fn tool_row(call: &ToolCall) -> Json {
    let kind = call.kind.as_deref().unwrap_or("other");
    let title = call.title.clone().unwrap_or_default();
    let input = call.raw_input.as_ref();
    let field = |name: &str| {
        input
            .and_then(|i| i.get(name))
            .and_then(Json::as_str)
            .map(str::to_string)
    };
    let (ds_kind, verb) = match kind {
        "read" => ("read", "Read"),
        "edit" => ("edit", "Edit"),
        "delete" => ("edit", "Delete"),
        "move" => ("edit", "Move"),
        "search" => ("search", "Search"),
        "execute" => ("exec", "Run"),
        "think" => ("thought", "Thinking"),
        "fetch" => ("web", "Fetch"),
        _ => ("exec", "Tool"),
    };
    let target = call
        .path
        .clone()
        .or_else(|| field("file_path"))
        .or_else(|| field("command"))
        .or_else(|| field("pattern"))
        .or_else(|| field("url"))
        .unwrap_or_else(|| title.clone());
    let command = field("command").or_else(|| {
        call.raw_input
            .as_ref()
            .map(|i| serde_json::to_string_pretty(i).unwrap_or_default())
    });
    let mut row = json!({
        "id": call.id,
        "kind": ds_kind,
        "verb": verb,
        "target": target,
        "failed": call.status.as_deref() == Some("failed"),
        "live": matches!(call.status.as_deref(), Some("pending" | "in_progress")),
    });
    if let Some(command) = command {
        row["call"] = command.into();
    }
    if let Some(output) = &call.output {
        row["output"] = output.clone().into();
    }
    row
}

fn merge(into: &mut ToolCall, update: ToolCall) {
    macro_rules! keep {
        ($($f:ident),*) => { $( if update.$f.is_some() { into.$f = update.$f; } )* };
    }
    keep!(title, kind, status, raw_input, output, path);
}

impl State {
    fn accepts(&self, id: Option<&str>) -> Result<(), String> {
        if id.and_then(|id| Uuid::parse_str(id).ok()) == Some(self.id) {
            Ok(())
        } else {
            Err("The selected chat changed.".into())
        }
    }

    /// The reply being written, started if there is none.
    fn reply(&mut self) -> &mut Message {
        if !self.messages.last().is_some_and(|m| !m.user && m.streaming) {
            let id = Uuid::new_v4().to_string();
            let model = self.model_label();
            self.messages.push(Message {
                id,
                user: false,
                harness: self.harness,
                switch: None,
                model,
                text: String::new(),
                at: String::new(),
                blocks: Vec::new(),
                streaming: true,
                started: now_secs(),
                worked: None,
            });
        }
        self.messages.last_mut().expect("just pushed")
    }

    fn apply(&mut self, event: Event) {
        match event {
            Event::Ready(config) => {
                self.state = "ready";
                self.take_config(config);
            }
            Event::Config(config) => self.take_config(config),
            Event::Chunk { thought, text } => {
                let id = Uuid::new_v4().to_string();
                let reply = self.reply();
                if thought {
                    match reply.blocks.last_mut() {
                        Some(Block::Thought { text: t, .. }) => t.push_str(&text),
                        _ => reply.blocks.push(Block::Thought { id, text }),
                    }
                } else {
                    reply.text.push_str(&text);
                    match reply.blocks.last_mut() {
                        Some(Block::Text { text: t, .. }) => t.push_str(&text),
                        _ => reply.blocks.push(Block::Text { id, text }),
                    }
                }
            }
            Event::Tool(call) => {
                let id = Uuid::new_v4().to_string();
                let reply = self.reply();
                // An update to a call already shown, wherever it is.
                for block in &mut reply.blocks {
                    if let Block::Tools { tools, .. } = block
                        && let Some(existing) = tools.iter_mut().find(|t| t.id == call.id)
                    {
                        merge(existing, call);
                        return;
                    }
                }
                match reply.blocks.last_mut() {
                    Some(Block::Tools { tools, .. }) => tools.push(call),
                    _ => reply.blocks.push(Block::Tools {
                        id,
                        tools: vec![call],
                    }),
                }
            }
            Event::Plan(entries) => self.todos = entries,
            Event::Permission {
                request,
                tool,
                options,
            } => {
                self.ask = Some(Ask {
                    request,
                    tool,
                    options,
                })
            }
            Event::TurnEnded { stop_reason } => {
                self.finish();
                if stop_reason != "end_turn" && stop_reason != "cancelled" {
                    self.notice = Some(format!("The turn ended: {stop_reason}"));
                } else {
                    // This session has now seen everything said so far.
                    self.seen = self.messages.len();
                }
            }
            Event::Error(error) => {
                self.finish();
                self.state = "offline";
                self.notice = Some(error);
            }
        }
    }

    /// Events from every running agent: the active one's change the chat, a
    /// parked one's only update its slot.
    fn apply_from(&mut self, from: Harness, event: Event) {
        if from == self.harness {
            return self.apply(event);
        }
        let Some(slot) = self.parked.iter_mut().find(|s| s.harness == from) else {
            return;
        };
        match event {
            Event::Ready(config) => {
                slot.state = "ready";
                slot.config = config;
            }
            Event::Config(config) => slot.config = config,
            Event::Error(error) => {
                slot.state = "offline";
                slot.error = Some(error);
            }
            _ => {}
        }
    }

    /// Moves the chat to another agent: a new session of its own, brought up
    /// to date by `handoff` on the next turn.
    fn activate(&mut self, to: Harness) -> Result<(), String> {
        if to == self.harness {
            return Ok(());
        }
        if self.running {
            return Err("a turn is running; switch once it ends".into());
        }
        let i = self
            .parked
            .iter()
            .position(|s| s.harness == to)
            .ok_or_else(|| format!("{} is not available", to.label()))?;
        if self.parked[i].state == "offline" {
            let why = self.parked[i].error.clone().unwrap_or_default();
            return Err(format!("{} is not running: {why}", to.label()));
        }
        let slot = &mut self.parked[i];
        std::mem::swap(&mut self.harness, &mut slot.harness);
        std::mem::swap(&mut self.agent, &mut slot.agent);
        std::mem::swap(&mut self.state, &mut slot.state);
        std::mem::swap(&mut self.config, &mut slot.config);
        std::mem::swap(&mut self.seen, &mut slot.seen);
        slot.error = None;
        self.notice = None;
        Ok(())
    }

    /// What the agent now taking a turn has not seen of the chat, as text to
    /// put ahead of the person's message (Zeron does the same on a provider
    /// switch: the transcript replayed, newest first within a budget).
    /// None when it has seen everything.
    fn handoff(&self) -> Option<String> {
        const BUDGET: usize = 60_000;
        const CLIP: usize = 8_000;
        let unseen = &self.messages[self.seen.min(self.messages.len())..];
        let said: Vec<&Message> = unseen.iter().filter(|m| m.switch.is_none()).collect();
        if said.is_empty() {
            return None;
        }
        let mut lines = Vec::new();
        let mut left = BUDGET;
        for m in said.iter().rev().filter(|m| !m.text.trim().is_empty()) {
            let who = if m.user { "User" } else { m.harness.label() };
            let mut text: String = m.text.trim().chars().take(CLIP).collect();
            if m.text.trim().chars().count() > CLIP {
                text.push('…');
            }
            let line = format!("{who}: {text}");
            if line.len() > left {
                break;
            }
            left -= line.len();
            lines.push(line);
        }
        lines.reverse();
        let intro = "Below is the conversation so far, including turns from earlier agent sessions. Continue from the transcript and answer the user's new message.";
        Some(format!(
            "{intro}\n\n{}\n\n---\nThe user's new message:\n",
            lines.join("\n\n")
        ))
    }

    /// A person's message sent: shown at once, and run.
    fn start_turn(
        &mut self,
        id: Option<String>,
        text: String,
        picks: Vec<Command>,
    ) -> Result<(), String> {
        if self.running {
            return Err(
                "Wait for the current turn to finish before sending another message.".into(),
            );
        }
        let agent = self.agent.clone().ok_or("the agent is not running")?;
        if self.state != "ready" {
            return Err(self
                .notice
                .clone()
                .unwrap_or_else(|| format!("{} is not ready yet", self.harness.label())));
        }
        let id = id.unwrap_or_else(|| Uuid::new_v4().to_string());
        self.notice = None;
        let context = if self.seen == 0 {
            format!(
                "Discussion context: {}\n{}\n\n",
                self.context.title, self.context.description
            )
        } else {
            String::new()
        };
        let prompt = format!("{context}{}{text}", self.handoff().unwrap_or_default());
        // Send before recording the turn so a broken adapter leaves the draft retryable.
        for pick in picks {
            agent.send(pick)?;
        }
        agent.send(Command::Prompt(prompt))?;
        // The seam, where the chat moved to another agent.
        if let Some(last) = self
            .messages
            .iter()
            .rev()
            .find(|m| m.switch.is_none())
            .map(|m| m.harness)
            && last != self.harness
        {
            let sid = Uuid::new_v4().to_string();
            self.messages.push(Message {
                id: sid,
                user: false,
                harness: self.harness,
                switch: Some((last, self.harness)),
                model: String::new(),
                text: String::new(),
                at: String::new(),
                blocks: Vec::new(),
                streaming: false,
                started: now_secs(),
                worked: None,
            });
        }
        self.running = true;
        self.messages.push(Message {
            id,
            user: true,
            harness: self.harness,
            switch: None,
            model: String::new(),
            text,
            at: clock(),
            blocks: Vec::new(),
            streaming: false,
            started: now_secs(),
            worked: None,
        });
        Ok(())
    }

    fn take_config(&mut self, config: Config) {
        if let Some(mode) = &config.mode {
            if let Some(access) = self.harness.access_of(mode) {
                self.access = access;
            }
            if self.harness == Harness::Claude {
                self.plan = mode == "plan";
            }
        }
        if self.harness == Harness::Codex
            && let Some(plan) = config.plan
        {
            self.plan = plan;
        }
        self.config = Config {
            mode: config.mode.or(self.config.mode.take()),
            plan: config.plan.or(self.config.plan),
            ..config
        };
    }

    /// What the page chose (a `chat.configure` patch, or the pick a send
    /// carries) as the commands that make the agent match it. A model of
    /// another agent moves the chat to it first.
    fn configure(&mut self, patch: &Json) -> Result<Vec<Command>, String> {
        let mut commands = Vec::new();
        let mut moved = false;
        let mut model = None;
        if let Some(value) = patch["model"].as_str() {
            model = Some(match value.split_once('/') {
                Some((h, id)) if Harness::from_id(h).is_some() => {
                    (Harness::from_id(h).unwrap_or(self.harness), id)
                }
                _ => (self.harness, value),
            });
        }
        if let Some((to, _)) = model
            && to != self.harness
        {
            self.activate(to)?;
            moved = true;
        }
        if let Some((_, id)) = model
            && self.config.model.as_deref() != Some(id)
            && self.config.models.iter().any(|m| m.id == id)
            && !self.config.model_option.is_empty()
        {
            self.config.model = Some(id.to_string());
            commands.push(Command::SetConfig {
                id: self.config.model_option.clone(),
                value: id.to_string(),
            });
        }
        if let Some(effort) = patch["effort"].as_str()
            && self.config.effort.as_deref() != Some(effort)
            && self.config.efforts.iter().any(|(id, _)| id == effort)
            && !self.config.effort_option.is_empty()
        {
            self.config.effort = Some(effort.to_string());
            commands.push(Command::SetConfig {
                id: self.config.effort_option.clone(),
                value: effort.to_string(),
            });
        }
        // A new session starts from the chat's access and plan, not its own.
        let mut access_changed = moved;
        let mut plan_changed = moved && self.plan;
        if let Some(access) = patch["access"].as_str().and_then(|a| {
            ["supervised", "edits", "auto", "full"]
                .into_iter()
                .find(|n| *n == a)
        }) {
            access_changed |= self.access != access;
            self.access = access;
        }
        if let Some(plan) = patch["plan"].as_bool() {
            plan_changed |= self.plan != plan;
            self.plan = plan;
        }
        if access_changed || plan_changed {
            let access = self.harness.mode_for(self.access).unwrap_or("default");
            match self.harness {
                Harness::Claude => {
                    let mode = if self.plan { "plan" } else { access };
                    self.config.mode = Some(mode.to_string());
                    commands.push(Command::SetMode(mode.to_string()));
                }
                Harness::Codex => {
                    if access_changed {
                        self.config.mode = Some(access.to_string());
                        commands.push(Command::SetMode(access.to_string()));
                    }
                    if plan_changed {
                        self.config.plan = Some(self.plan);
                        commands.push(Command::SetConfig {
                            id: "collaboration_mode".into(),
                            value: if self.plan { "plan" } else { "default" }.into(),
                        });
                    }
                }
            }
        }
        Ok(commands)
    }

    fn finish(&mut self) {
        self.running = false;
        self.ask = None;
        if let Some(reply) = self.messages.last_mut().filter(|m| !m.user && m.streaming) {
            reply.streaming = false;
            reply.worked = Some((now_secs() - reply.started).max(0.));
            reply.at = clock();
        }
    }

    fn message_json(&self, m: &Message) -> Json {
        if let Some((from, to)) = m.switch {
            return json!({
                "id": m.id,
                "role": "switch",
                "from": from.label(),
                "to": to.label(),
                "from_mark": from.mark(),
                "to_mark": to.mark(),
            });
        }
        if m.user {
            let waiting = self.running && self.messages.last().is_some_and(|l| l.id == m.id);
            let mut j =
                json!({ "id": m.id, "role": "user", "text": m.text, "at": m.at, "copy": m.text });
            if waiting {
                j["waiting"] = "working".into();
                j["since"] = m.started.into();
            }
            return j;
        }
        let blocks: Vec<Json> = m
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let last = i + 1 == m.blocks.len() && m.streaming;
                match b {
                    Block::Text { id, text } => json!({ "id": id, "kind": "text", "text": text }),
                    Block::Thought { id, text } => json!({
                        "id": id,
                        "kind": "tools",
                        "live": last,
                        "tools": [{ "id": format!("{id}t"), "kind": "thought", "verb": "Thinking", "prose": true, "call": text, "live": last }],
                    }),
                    Block::Tools { id, tools } => json!({
                        "id": id,
                        "kind": "tools",
                        "live": last,
                        "tools": tools.iter().map(tool_row).collect::<Vec<_>>(),
                    }),
                }
            })
            .collect();
        let mut j = json!({
            "id": m.id,
            "role": "assistant",
            "text": m.text,
            "model": m.model,
            "streaming": m.streaming,
            "started": m.started,
            "blocks": blocks,
        });
        if !m.streaming {
            j["at"] = m.at.clone().into();
            j["copy"] = m.text.clone().into();
            if let Some(worked) = m.worked {
                j["worked"] = worked.into();
            }
        }
        j
    }

    fn model_label(&self) -> String {
        let id = self.config.model.as_deref().unwrap_or(self.harness.label());
        self.config
            .models
            .iter()
            .find(|m| m.id == id)
            .map_or(id.to_string(), |m| m.name.clone())
    }

    fn session_json(&self) -> Json {
        let mut s = json!({
            "id": self.id,
            "state": if self.messages.is_empty() && self.state == "loading" { "loading" } else { "ready" },
            "title": self.context.title,
            "cwd": self.cwd,
            // The project and device the entry seeds (ui/acp_chat.luau), so the
            // composer's location chip names where this chat works.
            "project": "p",
            "device": "mac",
            "harness": self.harness.id(),
            "agent": self.harness.id(),
            "model": self.config.model.as_ref().map(|m| format!("{}/{m}", self.harness.id())),
            "effort": self.config.effort,
            "model_label": self.model_label(),
            "access": self.access,
            "plan": self.plan,
            "running": self.running,
            "queue": [],
            "tasks": [],
            "todos": self.todos.iter().map(|(text, status)| json!({ "text": text, "content": text, "status": status })).collect::<Vec<_>>(),
        });
        if let Some(ask) = &self.ask {
            let title = ask
                .tool
                .title
                .clone()
                .unwrap_or_else(|| "Allow this?".into());
            let command = ask
                .tool
                .raw_input
                .as_ref()
                .and_then(|i| i.get("command"))
                .and_then(Json::as_str)
                .map(str::to_string);
            s["ask"] = json!({
                "request": ask.request.to_string(),
                "kind": "approval",
                "approval": { "title": title, "command": command, "cwd": self.cwd },
            });
        }
        if let Some(notice) = &self.notice {
            s["notice"] = json!({ "text": notice, "kind": "error" });
        }
        s
    }

    /// Every agent's state and settings, the active one's first-hand.
    fn slots(&self) -> Vec<(Harness, &'static str, &Config)> {
        Harness::ALL
            .into_iter()
            .filter_map(|h| {
                if h == self.harness {
                    Some((h, self.state, &self.config))
                } else {
                    self.parked
                        .iter()
                        .find(|s| s.harness == h)
                        .map(|s| (h, s.state, &s.config))
                }
            })
            .collect()
    }

    fn catalog_json(&self) -> Json {
        let slots = self.slots();
        let mut models = Vec::new();
        for (h, state, config) in &slots {
            if *state == "offline" {
                continue;
            }
            for m in &config.models {
                let mut row = json!({
                    "value": format!("{}/{}", h.id(), m.id),
                    "label": m.name,
                    "harness": h.id(),
                    "default": config.model.as_deref() == Some(m.id.as_str()),
                });
                if !config.efforts.is_empty() {
                    row["efforts"] = config
                        .efforts
                        .iter()
                        .map(|(id, name)| json!({ "value": id, "label": name }))
                        .collect();
                    row["effort"] = config.effort.clone().into();
                }
                models.push(row);
            }
        }
        json!({
            "state": if slots.iter().all(|(_, s, _)| *s == "offline") { "failed" } else { "ready" },
            "complete": slots.iter().all(|(_, s, _)| *s != "loading"),
            "harnesses": slots.iter().map(|(h, ..)| json!({ "id": h.id(), "label": h.label(), "mark": h.mark(), "auto": true })).collect::<Vec<_>>(),
            "models": models,
        })
    }
}

fn value(json: Json) -> Value {
    Value::from_json(&json.to_string()).unwrap_or(Value::Nil)
}

impl Chat {
    fn publish(&self) {
        let state = self.state.borrow();
        self.session.set(value(state.session_json()));
        let messages: Vec<Json> = state
            .messages
            .iter()
            .map(|m| state.message_json(m))
            .collect();
        self.messages.set(value(Json::Array(messages)));
        self.catalog.set(value(state.catalog_json()));
    }
}

fn args(v: &Value) -> Json {
    serde_json::from_str(&v.to_json()).unwrap_or(Json::Null)
}

impl State {
    fn new(context: Context, session: Session) -> Self {
        let cwd = context.cwd.clone().unwrap_or_default();
        let harness = session.harness;
        Self {
            id: session.id,
            context,
            cwd,
            harness,
            parked: Vec::new(),
            seen: 0,
            agent: None,
            state: "loading",
            notice: None,
            config: Config::default(),
            access: "supervised",
            plan: false,
            messages: session.messages,
            running: false,
            ask: None,
            todos: Vec::new(),
        }
    }
}

impl Chat {
    pub(super) fn running(&self) -> bool {
        self.state.borrow().running
    }

    pub(super) fn select(self: &Rc<Self>, context: Context, session: Session) {
        let id = session.id;
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.cwd.set(value(json!(context.cwd)));
        self.title.set(value(json!(context.title)));
        let cwd = context.cwd.clone();
        let mut state = State::new(context, session);
        let Some(cwd) = cwd else {
            state.state = "ready";
            *self.state.borrow_mut() = state;
            self.publish();
            self.active.set(value(json!(id)));
            return;
        };
        if !cwd.is_dir() {
            state.state = "ready";
            state.notice = Some(format!(
                "The task working directory is unavailable: {}",
                cwd.display()
            ));
            *self.state.borrow_mut() = state;
            self.publish();
            self.active.set(value(json!(id)));
            return;
        }

        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        for harness in Harness::ALL {
            let (agent, mut own) = acp::spawn(&self.runtime, cwd.clone(), harness);
            let tx = tx.clone();
            self.runtime.spawn(async move {
                while let Some(event) = own.recv().await {
                    if tx.send((harness, event)).is_err() {
                        break;
                    }
                }
            });
            if harness == state.harness {
                state.agent = Some(agent);
            } else {
                state.parked.push(Slot {
                    harness,
                    agent: Some(agent),
                    state: "loading",
                    error: None,
                    config: Config::default(),
                    seen: 0,
                });
            }
        }
        drop(tx);
        *self.state.borrow_mut() = state;
        self.publish();
        self.active.set(value(json!(id)));

        let weak = Rc::downgrade(self);
        self.tasks.spawn(async move {
            while let Some((harness, first)) = events.recv().await {
                let Some(chat) = weak.upgrade() else { return };
                let mut state = chat.state.borrow_mut();
                if chat.generation.get() != generation {
                    return;
                }
                let was_running = state.running;
                state.apply_from(harness, first);
                while let Ok((harness, event)) = events.try_recv() {
                    state.apply_from(harness, event);
                }
                let finished = was_running && !state.running;
                drop(state);
                if finished {
                    chat.persist();
                }
                chat.publish();
            }
        });
    }

    fn persist(&self) {
        let mut state = self.state.borrow_mut();
        let mut messages = state.messages.clone();
        for message in &mut messages {
            message.streaming = false;
        }
        let session = Session {
            id: state.id,
            harness: state.harness,
            updated: chrono::Utc::now().timestamp_millis(),
            messages,
        };
        match save(&self.path, &state.context.key, &session) {
            Ok(()) => {
                drop(state);
                self.updated.update(|revision| *revision += 1);
            }
            Err(error) => state.notice = Some(format!("Could not save chat history. {error}")),
        }
    }
}

pub(super) fn install(
    host: &ScriptHost,
    tasks: TaskSpawner,
    runtime: tokio::runtime::Handle,
    path: PathBuf,
    updated: zgx::reactive::Signal<u64>,
) -> Rc<Chat> {
    let chat = Rc::new(Chat {
        state: RefCell::new(State::new(Context::default(), Session::new())),
        session: host.shared("chat.session", Value::Nil),
        messages: host.shared("chat.messages", Value::List(Rc::from([]))),
        catalog: host.shared(
            "chat.catalog",
            value(json!({ "state": "looking", "harnesses": [], "models": [] })),
        ),
        active: host.shared("tod.chat.active", ""),
        cwd: host.shared("app.cwd", ""),
        title: host.shared("tod.chat.title", ""),
        tasks,
        generation: Cell::new(0),
        runtime,
        path,
        updated,
    });

    let c = chat.clone();
    host.action("chat.open", move |_| c.publish());
    let c = chat.clone();
    host.action("chat.catalog", move |_| c.publish());
    // The catalog of another device: there is only this one.
    let c = chat.clone();
    host.action("chat.catalog_on", move |_| c.publish());
    // The `/` and `$` lists: none yet (ACP's `available_commands_update` could fill them).
    host.async_action("chat.invocations", move |_| async move {
        Ok(value(json!({ "state": "ready", "rows": [] })))
    });

    let c = chat.clone();
    host.async_action("chat.send", move |req| {
        let c = c.clone();
        async move {
            let req = args(&req);
            let text = req["text"].as_str().unwrap_or_default().trim().to_string();
            if text.is_empty() {
                return Err("Enter a message first.".into());
            }
            if req["attachments"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
            {
                return Err("This chat only sends text messages.".into());
            }
            {
                let mut state = c.state.borrow_mut();
                state.accepts(req["session"].as_str())?;
                let picks = state.configure(&req)?;
                state.start_turn(req["message_id"].as_str().map(str::to_string), text, picks)?;
            }
            c.persist();
            c.publish();
            Ok(Value::Nil)
        }
    });

    let c = chat.clone();
    host.async_action("chat.interrupt", move |request| {
        let c = c.clone();
        async move {
            let state = c.state.borrow();
            state.accepts(args(&request).as_str())?;
            let agent = state.agent.as_ref().ok_or("the agent is not running")?;
            agent.send(Command::Cancel)?;
            Ok(Value::Nil)
        }
    });

    let c = chat.clone();
    host.async_action("chat.respond", move |req| {
        let c = c.clone();
        async move {
            let req = args(&req);
            let decision = req["decision"].as_str().unwrap_or_default();
            let request: u64 = req["request"]
                .as_str()
                .and_then(|r| r.parse().ok())
                .ok_or("no such request")?;
            let (agent, option) = {
                let state = c.state.borrow();
                state.accepts(req["id"].as_str())?;
                let ask = state
                    .ask
                    .as_ref()
                    .filter(|a| a.request == request)
                    .ok_or("that request was already answered")?;
                let wanted: &[&str] = match decision {
                    "once" => &["allow_once", "allow_always"],
                    "always" => &["allow_always", "allow_once"],
                    "deny" => &["reject_once", "reject_always"],
                    other => return Err(format!("{other} is not an answer this panel handles")),
                };
                let option = wanted
                    .iter()
                    .find_map(|kind| ask.options.iter().find(|o| o.kind == *kind))
                    .map(|o| o.id.clone());
                (
                    state.agent.clone().ok_or("the agent is not running")?,
                    option,
                )
            };
            agent.send(Command::Permission { request, option })?;
            c.state.borrow_mut().ask = None;
            c.publish();
            Ok(Value::Nil)
        }
    });

    let c = chat.clone();
    host.async_action("chat.configure", move |req| {
        let c = c.clone();
        async move {
            let (agent, commands) = {
                let mut state = c.state.borrow_mut();
                let req = args(&req);
                state.accepts(req["id"].as_str())?;
                let commands = state.configure(&req)?;
                (
                    state.agent.clone().ok_or("the agent is not running")?,
                    commands,
                )
            };
            for command in commands {
                agent.send(command)?;
            }
            c.publish();
            Ok(Value::Nil)
        }
    });

    let c = chat.clone();
    host.async_action("chat.refs", move |_| {
        let c = c.clone();
        async move {
            let path = c
                .state
                .borrow()
                .context
                .cwd
                .clone()
                .ok_or("No working directory.")?;
            let refs = c
                .runtime
                .spawn_blocking(move || git_refs(&path))
                .await
                .map_err(|error| error.to_string())?;
            Ok(value(Json::Array(refs)))
        }
    });

    for name in ["chat.queue", "chat.warm", "chat.retry"] {
        host.async_action(name, move |_| async move {
            Err(format!("{name} is unavailable in this panel"))
        });
    }
    chat
}
