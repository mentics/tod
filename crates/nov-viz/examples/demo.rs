use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_flow::*;
use gpui_platform::application;
use nov_viz::{
    GraphController, Keymap, NavVisual, Projection, agent_response_sample, build_flow_graph, layout,
};

const BG: u32 = 0x09090b;
const HUD: u32 = 0x18181b;
const TEXT: u32 = 0xfafafa;
const TEXT_DIM: u32 = 0xa1a1aa;

struct DemoView {
    flow: Entity<FlowGraph>,
    state: Entity<FlowState>,
    minimap: Entity<Minimap>,
    controls: Entity<Controls>,
    visual: Entity<NavVisual>,
    graph: nov_viz::NovGraph,
    nav: GraphController,
    keymap: Keymap,
    focus_handle: FocusHandle,
    keys_down: HashSet<String>,
    container: (f32, f32),
    last_tick: Instant,
    _camera: Task<()>,
    _intercept: Subscription,
}

impl DemoView {
    fn push_visual(&mut self, cx: &mut Context<Self>) {
        let vis = self.nav.visual();
        self.visual.update(cx, |slot, cx| {
            *slot = vis;
            cx.notify();
        });
        self.flow.update(cx, |_, cx| cx.notify());
        self.minimap.update(cx, |_, cx| cx.notify());
    }

    fn size_of(window: &Window) -> (f32, f32) {
        let size = window.viewport_size();
        (size.width.as_f32(), size.height.as_f32())
    }

    fn on_key(
        &mut self,
        keystroke: &Keystroke,
        is_held: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.container = Self::size_of(window);
        let (width, height) = self.container;

        if self.nav.editing.is_some() {
            if let Some(command) = self.keymap.command_for(keystroke) {
                let flow = self.state.clone();
                let handled = flow.update(cx, |state, _| {
                    self.nav
                        .handle(command, is_held, state, &mut self.graph, width, height)
                });
                if handled {
                    self.push_visual(cx);
                    cx.notify();
                    return true;
                }
            }
            if is_held {
                return true;
            }
            let key: &str = keystroke.key.as_ref();
            if key == "backspace" {
                self.nav.type_backspace();
                cx.notify();
                return true;
            }
            if key == "space" {
                self.nav.type_char(' ');
                cx.notify();
                return true;
            }
            if let Some(ch) = typed_char(keystroke) {
                self.nav.type_char(ch);
                cx.notify();
                return true;
            }
            return true;
        }

        let Some(command) = self.keymap.command_for(keystroke) else {
            return false;
        };
        let flow = self.state.clone();
        let handled = flow.update(cx, |state, _| {
            self.nav
                .handle(command, is_held, state, &mut self.graph, width, height)
        });
        if handled {
            self.push_visual(cx);
            cx.notify();
        }
        handled
    }

    fn tick_camera(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f32().clamp(0.001, 0.05);
        self.last_tick = now;
        if self.nav.pan == (0.0, 0.0) && self.nav.zoom_held == 0.0 {
            return;
        }
        let (width, height) = self.container;
        let flow = self.state.clone();
        flow.update(cx, |state, _| {
            self.nav.tick_camera(state, width, height, dt);
        });
        self.minimap.update(cx, |_, cx| cx.notify());
        self.flow.update(cx, |_, cx| cx.notify());
    }
}

fn typed_char(keystroke: &Keystroke) -> Option<char> {
    if keystroke.modifiers.control || keystroke.modifiers.alt || keystroke.modifiers.platform {
        return None;
    }
    if let Some(ref s) = keystroke.key_char {
        let mut chars = s.chars();
        let ch = chars.next()?;
        if chars.next().is_none() && !ch.is_control() {
            return Some(ch);
        }
    }
    let key = keystroke.key.as_str();
    if key.len() == 1 {
        let ch = key.chars().next()?;
        if keystroke.modifiers.shift {
            return Some(ch.to_ascii_uppercase());
        }
        return Some(ch);
    }
    None
}

impl Focusable for DemoView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for DemoView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.container = Self::size_of(window);
        let hud = self.nav.hud_line();
        let editing = self.nav.editing.is_some();
        let edit_buffer = self.nav.edit_buffer.clone();

        div()
            .id("nov-viz-demo")
            .key_context("NovViz")
            .track_focus(&self.focus_handle)
            .size_full()
            .relative()
            .bg(gpui::rgb(BG))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, cx| {
                let key: &str = event.keystroke.key.as_ref();
                this.keys_down.remove(key);
                this.nav.release_key(key);
                cx.notify();
            }))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                if !event.modifiers.shift {
                    this.nav.clear_camera_held();
                    cx.notify();
                }
            }))
            .child(self.flow.clone())
            .child(
                div()
                    .absolute()
                    .top(px(12.0))
                    .left(px(12.0))
                    .right(px(12.0))
                    .px_3()
                    .py_2()
                    .bg(gpui::rgb(HUD))
                    .rounded_md()
                    .text_xs()
                    .text_color(gpui::rgb(TEXT_DIM))
                    .child(hud),
            )
            .when(editing, |el| {
                el.child(
                    div()
                        .absolute()
                        .top(px(44.0))
                        .right(px(12.0))
                        .px_3()
                        .py_2()
                        .bg(gpui::rgb(HUD))
                        .border_1()
                        .border_color(gpui::rgb(0xe8a87c))
                        .rounded_md()
                        .child(
                            div()
                                .text_sm()
                                .text_color(gpui::rgb(TEXT))
                                .child(format!("{edit_buffer}|")),
                        ),
                )
            })
            .child(
                div()
                    .absolute()
                    .bottom(px(16.0))
                    .left(px(16.0))
                    .child(self.controls.clone()),
            )
            .child(
                div()
                    .absolute()
                    .bottom(px(16.0))
                    .right(px(16.0))
                    .child(self.minimap.clone()),
            )
    }
}

fn main() {
    application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1200.0), px(800.0)), cx);

        cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("Nov Viz Demo — agent response fixture".into()),
                    ..Default::default()
                }),
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let graph = agent_response_sample();
                let laid_out = layout(&graph, Projection::Flow).expect("ELK layout");
                let visual = cx.new(|_| NavVisual::default());
                let (state, flow) = build_flow_graph(&laid_out, visual.clone(), cx);

                state.update(cx, |state, _| {
                    state.fit_view(64.0, 1200.0, 800.0);
                });

                let mut nav = GraphController::new();
                state.update(cx, |flow_state, _| {
                    nav.seed_cursor(flow_state);
                    nav.sync_flow_selection(flow_state);
                });
                visual.update(cx, |slot, _| *slot = nav.visual());

                let minimap =
                    cx.new(|_| Minimap::new(state.clone()).container_bounds(1200.0, 800.0));
                let controls =
                    cx.new(|_| Controls::new(state.clone()).container_size(1200.0, 800.0));

                cx.new(|cx| {
                    let focus_handle = cx.focus_handle();
                    focus_handle.focus(window, cx);

                    let camera = cx.spawn(async move |this: WeakEntity<DemoView>, cx| loop {
                        cx.background_executor()
                            .timer(Duration::from_millis(16))
                            .await;
                        if this
                            .update(cx, |this, cx| {
                                this.tick_camera(cx);
                            })
                            .is_err()
                        {
                            break;
                        }
                    });

                    let entity = cx.weak_entity();
                    let intercept = cx.intercept_keystrokes({
                        let entity = entity.clone();
                        move |event, window, cx| {
                            let Some(entity) = entity.upgrade() else {
                                return;
                            };
                            entity.update(cx, |this, cx| {
                                let key = event.keystroke.key.clone();
                                let is_held = !this.keys_down.insert(key);
                                let handled =
                                    this.on_key(&event.keystroke, is_held, window, cx);
                                if handled {
                                    cx.stop_propagation();
                                }
                            });
                        }
                    });

                    DemoView {
                        flow,
                        state,
                        minimap,
                        controls,
                        visual,
                        graph,
                        nav,
                        keymap: Keymap::default(),
                        focus_handle,
                        keys_down: HashSet::new(),
                        container: (1200.0, 800.0),
                        last_tick: Instant::now(),
                        _camera: camera,
                        _intercept: intercept,
                    }
                })
            },
        )
        .expect("open demo window");

        cx.on_window_closed(|cx, _window_id| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}
