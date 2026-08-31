use crate::ui::actionable::chrome_control_with_shortcut_in_context;
use crate::ui::key_context::NOT_INPUT;
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Context, Corner, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window, actions, anchored,
    deferred, div, px, rems,
};
use gpui_component::button::Button;
use gpui_component::{ActiveTheme, IconName, Selectable, StyledExt, v_flex};

actions!(
    app_nav,
    [
        AppNavToggle,
        ShellGoTasks,
        ShellGoSettings,
        ShellGoDatabase,
        AppNavSelectUp,
        AppNavSelectDown,
        AppNavConfirm,
        AppNavCancel,
    ]
);

const APP_NAV_POPUP_CONTEXT: &str = "AppNavPopup";
const APP_NAV_ITEMS: [&str; 3] = ["Tasks", "Settings", "Database"];

/// Shared handler for `` ` `` / app-nav toggle — attach via [`HasAppNav::bind_app_nav_toggle`].
pub fn on_app_nav_toggle<V: Render + HasAppNav + 'static>(
    this: &mut V,
    _: &AppNavToggle,
    window: &mut Window,
    cx: &mut Context<V>,
) {
    this.toggle_app_nav(window, cx);
}

pub fn register_app_nav_keyboard_bindings(cx: &mut gpui::App) {
    let context = Some(NOT_INPUT);
    let popup_context = Some(APP_NAV_POPUP_CONTEXT);
    cx.bind_keys([
        gpui::KeyBinding::new("`", AppNavToggle, context),
        gpui::KeyBinding::new("ctrl-1", ShellGoTasks, context),
        gpui::KeyBinding::new("ctrl-comma", ShellGoSettings, context),
        gpui::KeyBinding::new("enter", AppNavConfirm, popup_context),
        gpui::KeyBinding::new("escape", AppNavCancel, popup_context),
        gpui::KeyBinding::new("up", AppNavSelectUp, popup_context),
        gpui::KeyBinding::new("down", AppNavSelectDown, popup_context),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppDestination {
    Tasks,
    Settings,
    Database,
}

struct AppNavPopup {
    focus_handle: FocusHandle,
    action_context: FocusHandle,
    selected_index: usize,
}

impl AppNavPopup {
    fn new(selected_index: usize, action_context: FocusHandle, cx: &mut App) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            action_context,
            selected_index,
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let action: Box<dyn gpui::Action> = match self.selected_index {
            0 => Box::new(ShellGoTasks),
            1 => Box::new(ShellGoSettings),
            _ => Box::new(ShellGoDatabase),
        };
        self.action_context.focus(window);
        window.dispatch_action(action, cx);
        cx.emit(DismissEvent);
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
        self.action_context.focus(window);
    }

    fn select_up(&mut self, _: &AppNavSelectUp, _: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.selected_index = if self.selected_index == 0 {
            APP_NAV_ITEMS.len() - 1
        } else {
            self.selected_index - 1
        };
        cx.notify();
    }

    fn select_down(&mut self, _: &AppNavSelectDown, _: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.selected_index = (self.selected_index + 1) % APP_NAV_ITEMS.len();
        cx.notify();
    }
}

impl EventEmitter<DismissEvent> for AppNavPopup {}

impl Focusable for AppNavPopup {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AppNavPopup {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let radius = cx.theme().radius.min(px(8.));
        v_flex()
            .id("app-nav-popup")
            .key_context(APP_NAV_POPUP_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(|this, _: &AppNavConfirm, window, cx| {
                this.confirm(window, cx);
            }))
            .on_action(cx.listener(|this, _: &AppNavCancel, window, cx| {
                this.dismiss(window, cx);
            }))
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, window, cx| {
                this.dismiss(window, cx);
            }))
            .popover_style(cx)
            .text_color(cx.theme().popover_foreground)
            .child(
                v_flex()
                    .id("app-nav-items")
                    .p_1()
                    .gap_y_0p5()
                    .min_w(rems(8.))
                    .children(APP_NAV_ITEMS.iter().enumerate().map(|(ix, label)| {
                        let selected = self.selected_index == ix;
                        let group = SharedString::from(format!("app-nav-item-{ix}"));
                        div()
                            .id(("app-nav-item", ix))
                            .group(group.clone())
                            .text_sm()
                            .h(px(26.))
                            .px(px(8.))
                            .rounded(radius)
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .when(selected, |el| {
                                el.bg(cx.theme().accent)
                                    .text_color(cx.theme().accent_foreground)
                            })
                            .when(!selected, |el| {
                                el.group_hover(group, |el| {
                                    el.bg(cx.theme().accent)
                                        .text_color(cx.theme().accent_foreground)
                                })
                            })
                            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                cx.stop_propagation();
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.selected_index = ix;
                                this.confirm(window, cx);
                            }))
                            .on_hover(cx.listener(move |this, hovered, _, cx| {
                                if *hovered {
                                    this.selected_index = ix;
                                    cx.notify();
                                }
                            }))
                            .child(*label)
                    })),
            )
    }
}

pub struct AppNavMenu {
    open: bool,
    menu: Option<Entity<AppNavPopup>>,
    return_focus: Option<FocusHandle>,
    _subscription: Option<Subscription>,
}

impl Default for AppNavMenu {
    fn default() -> Self {
        Self {
            open: false,
            menu: None,
            return_focus: None,
            _subscription: None,
        }
    }
}

impl AppNavMenu {
    pub fn close(&mut self) {
        self.open = false;
        self.menu = None;
        self._subscription = None;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn menu_focused(&self, window: &Window, cx: &App) -> bool {
        self.menu
            .as_ref()
            .is_some_and(|menu| menu.read(cx).focus_handle(cx).is_focused(window))
    }

    pub fn ensure_menu<V: Render + HasAppNav + 'static>(
        &mut self,
        current: Option<AppDestination>,
        return_focus: FocusHandle,
        cx: &mut Context<V>,
    ) {
        if self.menu.is_some() {
            return;
        }
        let selected_index = nav_item_index(current);
        let menu = cx.new(|cx| AppNavPopup::new(selected_index, return_focus, cx));
        self._subscription = Some(cx.subscribe(&menu, |view, _, _: &DismissEvent, cx| {
            view.app_nav_mut().return_focus = None;
            view.app_nav_mut().close();
            cx.notify();
        }));
        self.menu = Some(menu);
    }

    pub fn focus_menu(&self, window: &mut Window, cx: &mut App) {
        if let Some(menu) = self.menu.clone() {
            menu.update(cx, |menu, cx| {
                menu.focus_handle(cx).focus(window);
            });
        }
    }

    pub fn dismiss_menu(&mut self, window: &mut Window) {
        self.restore_return_focus(window);
        self.close();
    }

    /// Return keyboard focus to the handle stored when the menu was opened.
    pub fn restore_return_focus(&mut self, window: &mut Window) {
        if let Some(focus) = self.return_focus.take() {
            window.focus(&focus);
        }
    }
}

pub trait HasAppNav {
    fn app_nav_mut(&mut self) -> &mut AppNavMenu;

    fn app_nav_current(&self) -> Option<AppDestination>;

    fn app_nav_fallback_focus(&self) -> FocusHandle;

    fn open_app_nav(&mut self, window: &mut Window, cx: &mut Context<Self>)
    where
        Self: Render + HasAppNav + Sized + 'static,
    {
        let return_focus = window
            .focused(cx)
            .unwrap_or_else(|| self.app_nav_fallback_focus());
        self.app_nav_mut().return_focus = Some(return_focus.clone());
        self.app_nav_mut().open = true;
        let current = self.app_nav_current();
        self.app_nav_mut().ensure_menu(current, return_focus, cx);
        cx.notify();
        cx.on_next_frame(window, |this, window, cx| {
            this.app_nav_mut().focus_menu(window, cx);
            cx.notify();
        });
    }

    fn close_app_nav(&mut self, window: &mut Window, cx: &mut Context<Self>)
    where
        Self: Render + HasAppNav + Sized + 'static,
    {
        if !self.app_nav_mut().is_open() {
            return;
        }
        self.app_nav_mut().dismiss_menu(window);
        cx.notify();
    }

    fn toggle_app_nav(&mut self, window: &mut Window, cx: &mut Context<Self>)
    where
        Self: Render + HasAppNav + Sized + 'static,
    {
        if self.app_nav_mut().is_open() {
            self.close_app_nav(window, cx);
            return;
        }
        self.open_app_nav(window, cx);
    }

    /// Attach `` ` `` toggle handling. Call on every view root that implements [`HasAppNav`].
    fn bind_app_nav_toggle(&self, element: gpui::Div, cx: &mut Context<Self>) -> gpui::Div
    where
        Self: Render + Sized + 'static,
    {
        element.on_action(cx.listener(on_app_nav_toggle::<Self>))
    }

    fn render_app_nav(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement
    where
        Self: Render + HasAppNav + Sized + 'static,
    {
        let menu_open = self.app_nav_mut().is_open();
        let menu_focused = self.app_nav_mut().menu_focused(window, cx);
        let menu = self.app_nav_mut().menu.clone();

        div()
            .id("app-nav-menu")
            .relative()
            .child(chrome_control_with_shortcut_in_context(
                Button::new("app-nav-trigger")
                    .icon(IconName::Menu)
                    .compact()
                    .selected(menu_open || menu_focused)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_app_nav(window, cx);
                    })),
                window,
                &AppNavToggle,
                None,
                cx,
            ))
            .when(menu_open, |el| {
                el.when_some(menu, |el, menu| {
                    el.child(
                        deferred(
                            anchored()
                                .anchor(Corner::TopLeft)
                                .snap_to_window_with_margin(px(8.))
                                .child(div().occlude().mt_1().child(menu)),
                        )
                        .with_priority(1),
                    )
                })
            })
    }
}

fn nav_item_index(current: Option<AppDestination>) -> usize {
    match current {
        Some(AppDestination::Settings) => 1,
        Some(AppDestination::Database) => 2,
        Some(AppDestination::Tasks) | None => 0,
    }
}
