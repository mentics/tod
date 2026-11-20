//! The open incidents (`doc/lifecycle/incidents.md`), fleet-wide: what each
//! is, which nodes are parked on it, and a Dismiss for one that is over or not
//! worth tracking. Opened from the status bar's Incidents button.

use crate::ui::selectable_text::selectable_text;
use gpui::{App, AppContext, Context, IntoElement, ParentElement, Render, Styled, Window, div, px};
use gpui_component::button::Button;
use gpui_component::{ActiveTheme, WindowExt, h_flex, v_flex};
use std::sync::Arc;
use tod_store::fleet::FleetStore;
use tod_store::incidents::{self, Incident, Op};

/// How many open incidents there are, for the status bar. A read that fails
/// shows as none here and is logged.
pub fn open_count(fleet: &FleetStore) -> usize {
    match fleet.read(|conn| Ok(incidents::list_open(conn)?.len())) {
        Ok(n) => n,
        Err(err) => {
            tracing::error!("could not read the open incidents: {err:#}");
            0
        }
    }
}

struct Row {
    incident: Incident,
    nodes: Vec<String>,
}

struct IncidentsDialog {
    fleet: Arc<FleetStore>,
    rows: Vec<Row>,
}

/// Opens the dialog.
pub fn open_incidents_dialog(window: &mut Window, cx: &mut App, fleet: Arc<FleetStore>) {
    let rows = fleet.read(|conn| {
        let nodes = tod_store::outline::repos::NodeRepo::new(conn);
        let mut rows = Vec::new();
        for incident in incidents::list_open(conn)? {
            let names = incidents::members(conn, incident.id)?
                .into_iter()
                .filter(|m| m.parked())
                .map(|m| {
                    nodes.get(m.node_id).ok().flatten().map(|n| n.title).unwrap_or_else(|| m.node_id.to_string())
                })
                .collect();
            rows.push(Row { incident, nodes: names });
        }
        Ok(rows)
    });
    let rows = match rows {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!("could not read the open incidents: {err:#}");
            return;
        }
    };
    let view = cx.new(|_| IncidentsDialog { fleet, rows });
    window.open_dialog(cx, move |d, _, _| {
        d.title("Incidents")
            .w(px(640.))
            .overlay(true)
            .overlay_closable(true)
            .keyboard(true)
            .close_button(true)
            .child(view.clone())
    });
}

impl Render for IncidentsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut body = v_flex().gap_3();
        if self.rows.is_empty() {
            body = body.child(div().text_sm().text_color(muted).child("No open incidents."));
        }
        for (i, row) in self.rows.iter().enumerate() {
            let id = row.incident.id;
            let status = if row.incident.notice {
                "notice".to_string()
            } else {
                format!("{}, {} failed probe(s)", row.incident.status, row.incident.probe_attempts)
            };
            let nodes = if row.nodes.is_empty() { "none parked".to_string() } else { row.nodes.join(", ") };
            let fleet = self.fleet.clone();
            body = body.child(
                v_flex()
                    .gap_1()
                    .child(
                        h_flex()
                            .justify_between()
                            .items_center()
                            .child(selectable_text(
                                format!("incident-title-{i}"),
                                format!("{} ({})", row.incident.summary, status),
                                window,
                                cx,
                            ))
                            .child(Button::new(("incident-dismiss", i)).outline().compact().label("Dismiss").on_click(
                                cx.listener(move |this, _, window, cx| {
                                    let fleet = fleet.clone();
                                    cx.background_executor()
                                        .spawn(async move {
                                            let op = Op::Dismiss { incident: id, now: chrono::Utc::now().timestamp_millis() };
                                            if let Err(err) = fleet.interview("user", tod_store::interview::InterviewCommand::Incident(op)) {
                                                tracing::error!("could not dismiss the incident: {err}");
                                            }
                                        })
                                        .detach();
                                    this.rows.retain(|r| r.incident.id != id);
                                    if this.rows.is_empty() {
                                        window.close_dialog(cx);
                                    }
                                    cx.notify();
                                }),
                            )),
                    )
                    .child(selectable_text(
                        format!("incident-nodes-{i}"),
                        format!("Parked: {nodes}"),
                        window,
                        cx,
                    ))
                    .child(selectable_text(
                        format!("incident-sample-{i}"),
                        row.incident.samples.first().map(|s| s.error.clone()).unwrap_or_default(),
                        window,
                        cx,
                    )),
            );
        }
        body
    }
}
