//! The Cloud VMs window: the workspace's tod-managed Blaxel sandboxes and
//! whether each is billed as active, in standby, or in some other state.
//! Blaxel's console does not show that. Refresh asks Blaxel again (off the UI
//! thread); the golden sandbox is listed apart from the rest.

use crate::app::CloudVmsWindowControl;
use crate::ui::off_thread::off_thread;
use crate::ui::selectable_text::selectable_text;
use crate::ui::style;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Context, FocusHandle, Focusable, InteractiveElement, IntoElement, KeyBinding,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, actions, div,
    px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::{Disableable, TitleBar, h_flex, v_flex};
use std::path::PathBuf;
use tod_sandbox::vms::{Billing, Vm};

const CONTEXT: &str = "CloudVms";

actions!(cloud_vms, [CloudVmsClose]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", CloudVmsClose, Some(CONTEXT))]);
}

pub struct CloudVmsView {
    focus_handle: FocusHandle,
    root: PathBuf,
    vms: Vec<Vm>,
    loading: bool,
    error: Option<String>,
    /// Clock time (UTC) of the last successful refresh.
    refreshed: Option<String>,
    window_control: CloudVmsWindowControl,
}

/// `2026-10-28T14:03:11Z` as `2026-10-28 14:03 UTC`; anything else as it is.
fn created_text(raw: Option<&str>) -> String {
    match raw {
        Some(s) if s.len() >= 16 && s.as_bytes()[10] == b'T' => format!("{} {} UTC", &s[..10], &s[11..16]),
        Some(s) => s.to_string(),
        None => "unknown".to_string(),
    }
}

fn labels_text(vm: &Vm) -> String {
    if vm.labels.is_empty() {
        return "none".to_string();
    }
    vm.labels.iter().map(|(k, v)| if v.is_empty() { k.clone() } else { format!("{k}={v}") }).collect::<Vec<_>>().join(", ")
}

impl CloudVmsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, root: PathBuf, control: CloudVmsWindowControl) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);
        let mut this = Self {
            focus_handle,
            root,
            vms: Vec::new(),
            loading: false,
            error: None,
            refreshed: None,
            window_control: control,
        };
        this.refresh(cx);
        this
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        cx.notify();
        let root = self.root.clone();
        off_thread(
            cx,
            move || tod_store::fleet::sandbox::list_vms(&root).map_err(|e| format!("{e:#}")),
            |this, result, cx| {
                this.loading = false;
                match result {
                    Ok(vms) => {
                        this.vms = vms;
                        this.error = None;
                        let secs = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        this.refreshed =
                            Some(format!("{:02}:{:02}:{:02}", secs / 3600 % 24, secs / 60 % 60, secs % 60));
                    }
                    Err(err) => this.error = Some(err),
                }
                cx.notify();
            },
        );
    }

    fn render_row(&self, index: usize, vm: &Vm, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let billing = vm.billing.label();
        let cell = |id: &str, text: String, window: &mut Window, cx: &mut Context<Self>| {
            selectable_text(SharedString::from(format!("{id}-{index}")), text, window, cx)
        };
        let billing_cell = style::text_dense(div()).w(px(130.)).flex_shrink_0().child(cell("vm-billing", billing, window, cx));
        let billing_cell = match vm.billing {
            Billing::Active => billing_cell.text_color(style::color::text_muted()),
            Billing::Standby => billing_cell,
            Billing::Other(_) => billing_cell.text_color(gpui::red()),
        };
        style::row_wrapped(h_flex().id(("vm-row", index)))
            .w_full()
            .items_start()
            .child(style::text_dense(div()).w(px(260.)).flex_shrink_0().child(cell("vm-name", vm.name.clone(), window, cx)))
            .child(
                style::text_dense_muted(div())
                    .w(px(150.))
                    .flex_shrink_0()
                    .child(cell("vm-created", created_text(vm.created_at.as_deref()), window, cx)),
            )
            .child(billing_cell)
            .child(
                style::text_dense_muted(div())
                    .flex_1()
                    .min_w_0()
                    .child(cell("vm-labels", labels_text(vm), window, cx)),
            )
            .into_any_element()
    }

    fn render_section(
        &self,
        title: &'static str,
        vms: Vec<(usize, &Vm)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .child(style::text_dense_muted(div()).px(style::space::INSET).pt(style::space::RELATED).child(title))
            .children(vms.into_iter().map(|(i, vm)| self.render_row(i, vm, window, cx)))
            .into_any_element()
    }
}

impl Focusable for CloudVmsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for CloudVmsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let vms = self.vms.clone();
        let golden: Vec<(usize, &Vm)> = vms.iter().enumerate().filter(|(_, v)| v.golden).collect();
        let others: Vec<(usize, &Vm)> = vms.iter().enumerate().filter(|(_, v)| !v.golden).collect();
        let status = if self.loading {
            "Refreshing…".to_string()
        } else if let Some(at) = &self.refreshed {
            format!(
                "{} managed · refreshed {at} UTC · billing lags a few minutes",
                vms.len()
            )
        } else {
            String::new()
        };
        let empty = vms.is_empty() && !self.loading && self.error.is_none();
        let golden_section = (!golden.is_empty()).then(|| self.render_section("Golden", golden, window, cx));
        let others_section = (!others.is_empty()).then(|| self.render_section("Sandboxes", others, window, cx));
        style::panel(v_flex())
            .key_context(CONTEXT)
            .track_focus(&self.focus_handle)
            .size_full()
            .on_action(cx.listener(|this, _: &CloudVmsClose, _, cx| this.window_control.close(cx)))
            .child(
                TitleBar::new().child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap(style::space::RELATED)
                        .child("Cloud VMs")
                        .child(div().flex_1())
                        .child(
                            Button::new("cloud-vms-close")
                                .label("Close")
                                .ghost()
                                .compact()
                                .on_click(cx.listener(|this, _, _, cx| this.window_control.close(cx))),
                        ),
                ),
            )
            .child(
                style::panel_header(h_flex())
                    .items_center()
                    .child(
                        Button::new("cloud-vms-refresh")
                            .label("Refresh")
                            .outline()
                            .compact()
                            .disabled(self.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    )
                    .child(style::text_dense_muted(div()).child(status)),
            )
            .when_some(self.error.clone(), |el, err| {
                el.child(
                    div().p(style::space::INSET).child(
                        selectable_text("cloud-vms-error", format!("Could not list the VMs: {err}"), window, cx)
                            .text_color(gpui::red()),
                    ),
                )
            })
            .child(
                style::text_dense_muted(h_flex())
                    .px(style::space::INSET)
                    .py(style::space::RELATED)
                    .child(div().w(px(260.)).flex_shrink_0().child("Name"))
                    .child(div().w(px(150.)).flex_shrink_0().child("Created"))
                    .child(div().w(px(130.)).flex_shrink_0().child("Billing"))
                    .child(div().flex_1().child("Labels")),
            )
            .child(
                v_flex()
                    .id("cloud-vms-rows")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(empty, |el| {
                        el.child(
                            style::text_dense_muted(div())
                                .p(style::space::INSET)
                                .child("No tod-managed sandboxes."),
                        )
                    })
                    .children(golden_section)
                    .children(others_section),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn created_is_shortened() {
        assert_eq!(created_text(Some("2026-10-28T14:03:11Z")), "2026-10-28 14:03 UTC");
        assert_eq!(created_text(None), "unknown");
    }
}
