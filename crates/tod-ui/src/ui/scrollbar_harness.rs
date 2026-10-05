//! Drives the real scrollbar over a real virtualized list, headless: drags the
//! thumb, spins the wheel, and checks where the list really ends up.

use crate::ui::row_scrollbar::RowScrollbar;
use gpui::prelude::*;
use gpui::{
    Context, IntoElement, ListAlignment, ListState, Modifiers, MouseButton, Render, ScrollDelta,
    ScrollWheelEvent, TestAppContext, TouchPhase, VisualTestContext, Window, div, list, point, px,
};
use gpui_component::scroll::{Scrollbar, ScrollbarHandle};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

const VIEW_W: f32 = 400.;
const VIEW_H: f32 = 600.;

struct Harness {
    list: ListState,
    heights: Rc<RefCell<Vec<f32>>>,
    row: RowScrollbar,
}

impl Render for Harness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let list_el = list(
            self.list.clone(),
            cx.processor(|this, ix: usize, _, _| {
                let h = this.heights.borrow().get(ix).copied().unwrap_or(10.);
                div().w_full().h(px(h)).into_any_element()
            }),
        )
        .size_full();
        let bar = Scrollbar::vertical(&self.row);
        div()
            .relative()
            .w(px(VIEW_W))
            .h(px(VIEW_H))
            .child(list_el)
            .child(
                div()
                    .occlude()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .w(px(16.))
                    .child(bar),
            )
    }
}

struct Rig<'a> {
    cx: &'a mut VisualTestContext,
    list: ListState,
    heights: Rc<RefCell<Vec<f32>>>,
    row: RowScrollbar,
}

fn rig<'a>(cx: &'a mut TestAppContext, heights: Vec<f32>) -> Rig<'a> {
    cx.update(gpui_component::init);
    cx.update(|cx| {
        gpui_component::Theme::set_scrollbar_mode(gpui_component::scroll::ScrollbarMode::Always, cx)
    });
    let list = ListState::new(heights.len(), ListAlignment::Top, px(1000.));
    let heights = Rc::new(RefCell::new(heights));
    let row = RowScrollbar::new(&list);
    let (_, vcx) = cx.add_window_view({
        let list = list.clone();
        let heights = heights.clone();
        let row = row.clone();
        move |_, _| Harness { list, heights, row }
    });
    let mut r = Rig { cx: vcx, list, heights, row };
    r.frame();
    r
}

impl Rig<'_> {
    fn grow(&mut self, n: usize, h: f32) {
        let old = self.heights.borrow().len();
        self.heights.borrow_mut().extend(std::iter::repeat(h).take(n));
        self.list.splice(old..old, n);
    }
    fn shrink(&mut self, to: usize) {
        let old = self.heights.borrow().len();
        self.heights.borrow_mut().truncate(to);
        self.list.splice(to..old, 0);
    }
    fn frame(&mut self) {
        for _ in 0..2 {
            self.cx.update(|window, cx| window.draw(cx).clear(cx));
        }
    }
    fn handle(&self) -> Box<dyn ScrollbarHandle> {
        Box::new(self.row.clone())
    }
    /// Where the thumb is along its travel, 0..1.
    fn pct(&self) -> f32 {
        let h = self.handle();
        let max = h.content_size().height.as_f32() - h.viewport_bounds().size.height.as_f32();
        if max <= 0. {
            return 0.;
        }
        let o = h.offset().y.as_f32();
        (-o / max).clamp(0., 1.)
    }
    fn thumb_len(&self) -> f32 {
        let h = self.handle();
        (VIEW_H * h.viewport_bounds().size.height.as_f32() / h.content_size().height.as_f32())
            .max(48.)
    }
    fn visible(&self) -> bool {
        let h = self.handle();
        h.content_size().height > h.viewport_bounds().size.height
    }
    /// True pixel distance from the top of the content to the viewport top.
    fn true_top(&self) -> f32 {
        let t = self.list.logical_scroll_top();
        let above: f32 = self.heights.borrow().iter().take(t.item_ix).sum();
        above + t.offset_in_item.as_f32()
    }
    fn total(&self) -> f32 {
        self.heights.borrow().iter().sum()
    }
    fn true_frac(&self) -> f32 {
        let max = self.total() - VIEW_H;
        if max <= 0. { 0. } else { (self.true_top() / max).clamp(0., 1.) }
    }
    fn at_true_end(&self) -> bool {
        self.true_top() + VIEW_H >= self.total() - 1.
    }
    fn at_true_top(&self) -> bool {
        self.true_top() <= 0.5
    }
    fn down(&mut self, y: f32) {
        self.cx
            .simulate_mouse_down(point(px(VIEW_W - 8.), px(y)), MouseButton::Left, Modifiers::default());
    }
    fn mv(&mut self, y: f32) {
        std::thread::sleep(Duration::from_millis(12));
        self.cx.simulate_mouse_move(
            point(px(VIEW_W - 8.), px(y)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        self.frame();
    }
    fn up(&mut self, y: f32) {
        self.cx
            .simulate_mouse_up(point(px(VIEW_W - 8.), px(y)), MouseButton::Left, Modifiers::default());
        self.frame();
        self.frame();
    }
    fn wheel(&mut self, dy: f32) {
        self.cx.simulate_event(ScrollWheelEvent {
            position: point(px(100.), px(100.)),
            delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        });
        self.frame();
    }
    /// Grab the thumb at its current top, drag it to `target`, release.
    fn drag_to(&mut self, target: f32, report: &mut Vec<String>) {
        let travel = VIEW_H - self.thumb_len();
        let grab = 5.;
        let y0 = self.pct() * travel + grab;
        let y1 = if target >= 1. { VIEW_H + 40. } else if target <= 0. { -40. } else { target * travel + grab };
        self.down(y0);
        let mut max_err = 0f32;
        for i in 1..=20 {
            let y = y0 + (y1 - y0) * i as f32 / 20.;
            self.mv(y);
            let expect = ((y - grab) / travel).clamp(0., 1.);
            max_err = max_err.max((self.pct() - expect).abs());
        }
        self.up(y1);
        report.push(format!(
            "  drag to {target:.2}: thumb-vs-mouse err {max_err:.3}; released: pct={:.3} true_frac={:.3} at_end={} at_top={} visible={}",
            self.pct(),
            self.true_frac(),
            self.at_true_end(),
            self.at_true_top(),
            self.visible()
        ));
    }
}

fn lcg(seed: &mut u64) -> f32 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    ((*seed >> 33) as f32) / (1u64 << 31) as f32
}

fn shapes() -> Vec<(&'static str, Vec<f32>)> {
    let mut s = 7u64;
    vec![
        ("5x300 (barely overflows)", vec![300.; 5]),
        ("3 rows, one 5000px", vec![100., 5000., 100.]),
        ("50x100", vec![100.; 50]),
        ("10000x40", vec![40.; 10000]),
        ("2000 random 20..800", (0..2000).map(|_| 20. + lcg(&mut s) * 780.).collect()),
        (
            "400 rows w/ rare 6000px",
            (0..400).map(|i| if i % 97 == 50 { 6000. } else { 60. }).collect(),
        ),
        ("1000 tiny then 1 huge", {
            let mut v = vec![24.; 1000];
            v.push(30000.);
            v
        }),
    ]
}

/// What the thumb must do on every shape of list.
fn check(cx: &mut TestAppContext) {
    for (name, heights) in shapes() {
        let mut r = rig(cx, heights);
        let mut log = Vec::new();
        let ctx = |what: &str| format!("{name}: {what}");
        assert!(r.visible(), "{}", ctx("scrollbar must show when the list overflows"));
        
        assert_eq!(r.pct(), 0., "{}", ctx("thumb starts at the top"));

        r.drag_to(1.0, &mut log);
        assert!(r.at_true_end(), "{}", ctx("dragging to the bottom must reach the real end"));
        assert!(r.pct() > 0.999, "{}", ctx("thumb sits at the bottom at the end"));
        assert!(r.visible(), "{}", ctx("scrollbar still shows at the end"));

        for target in [0.5, 0.25] {
            let before = r.pct();
            r.drag_to(target, &mut log);
            assert!(
                (r.pct() - target).abs() < 0.03,
                "{}",
                ctx(&format!("thumb should stay where it was dropped ({target}), was {before}, now {}", r.pct()))
            );
            assert!(r.visible(), "{}", ctx("scrollbar must not vanish mid-list"));
        }

        r.drag_to(0.0, &mut log);
        assert!(r.at_true_top(), "{}", ctx("dragging to the top must reach the real top"));
        assert_eq!(r.pct(), 0., "{}", ctx("thumb at the top"));

        // Wheel: the thumb only ever moves with the content.
        r.drag_to(1.0, &mut log);
        let mut last = r.pct();
        for _ in 0..10 {
            r.wheel(300.);
            assert!(r.pct() <= last + 1e-4, "{}", ctx("wheel up moved the thumb down"));
            last = r.pct();
        }
        for _ in 0..200 {
            r.wheel(-1000.);
            assert!(r.pct() >= last - 1e-4, "{}", ctx("wheel down moved the thumb up"));
            last = r.pct();
        }
        assert!(r.at_true_end() && r.pct() > 0.999, "{}", ctx("wheel to the end reaches it"));
        for l in log {
            println!("{name}: {l}");
        }
    }
}

#[gpui::test]
fn row_scrollbar_behaves_on_every_shape(cx: &mut TestAppContext) {
    check(cx);
}

/// A transcript grows while it is being read: rows are added at the end.
#[gpui::test]
fn row_scrollbar_survives_growth(cx: &mut TestAppContext) {
    let mut r = rig(cx, vec![80.; 200]);
    let mut log = Vec::new();

    // At the end, new rows keep the end in view (the owner scrolls to it).
    r.drag_to(1.0, &mut log);
    assert!(r.at_true_end());
    for _ in 0..20 {
        r.grow(3, 150.);
        r.list.scroll_to_end();
        r.frame();
        assert!(r.visible());
        assert!(r.pct() > 0.999, "thumb follows the tail, got {}", r.pct());
    }

    // Mid-list, new rows do not move the view and the thumb stays put.
    r.drag_to(0.4, &mut log);
    let (top, pct) = (r.true_top(), r.pct());
    r.grow(50, 60.);
    r.frame();
    assert!((r.true_top() - top).abs() < 1., "rows added below do not scroll the list");
    assert!(r.pct() <= pct + 1e-3, "adding rows below only shrinks the thumb's place");

    // Rows arrive during a drag; a drag to the bottom still ends at the end.
    let travel = VIEW_H - r.thumb_len();
    let y0 = r.pct() * travel + 5.;
    r.down(y0);
    for i in 1..=10 {
        r.mv(y0 + (VIEW_H + 40. - y0) * i as f32 / 10.);
        r.grow(5, 400.);
        r.frame();
        assert!(r.visible(), "scrollbar vanished mid-drag");
    }
    r.up(VIEW_H + 40.);
    assert!(r.at_true_end(), "drag to the bottom while rows arrive must end at the end");
}

/// The window is resized: the thumb still reaches both ends.
#[gpui::test]
fn row_scrollbar_after_content_shrinks(cx: &mut TestAppContext) {
    let mut r = rig(cx, vec![100.; 100]);
    let mut log = Vec::new();
    r.drag_to(0.6, &mut log);
    r.shrink(95);
    r.frame();
    r.drag_to(1.0, &mut log);
    assert!(r.at_true_end());
    r.shrink(3);
    r.frame();
    assert!(!r.visible() || r.at_true_end() || r.at_true_top());
}

// ---- The real transcript list, with real wrapped text ----

mod real {
    use super::*;
    use crate::ui::transcript_list::{Entry, EntryKind, TranscriptList};
    use gpui::Entity;

    struct Host {
        list: Entity<TranscriptList>,
    }

    impl Render for Host {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(VIEW_W)).h(px(VIEW_H)).child(self.list.clone())
        }
    }

    fn entries(n: usize) -> Vec<Entry> {
        let mut seed = 11u64;
        (0..n)
            .map(|i| {
                let words = match i % 7 {
                    0 => 3,
                    1 => 400,
                    2 => 40,
                    _ => 5 + (lcg(&mut seed) * 120.) as usize,
                };
                let body = (0..words)
                    .map(|w| format!("word{}", (w * 7 + i) % 97))
                    .collect::<Vec<_>>()
                    .join(" ");
                Entry {
                    kind: if i % 2 == 0 { EntryKind::User } else { EntryKind::Agent },
                    body,
                    parts: Vec::new(),
                    label: None,
                    summary: None,
                    live: false,
                    images: Vec::new(),
                }
            })
            .collect()
    }

    #[gpui::test]
    fn transcript_scrollbar_reaches_both_ends_with_real_text(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(|cx| {
            gpui_component::Theme::set_scrollbar_mode(gpui_component::scroll::ScrollbarMode::Always, cx)
        });
        for n in [4usize, 60, 1500] {
            let (host, vcx) = cx.add_window_view(|window, cx| {
                let _ = window;
                let list = cx.new(|_| TranscriptList::new());
                Host { list }
            });
            let list = host.read_with(vcx, |h, _| h.list.clone());
            list.update(vcx, |l, cx| l.set_entries(entries(n), cx));
            let state = list.read_with(vcx, |l, _| l.list_state());
            let bar = list.read_with(vcx, |l, _| l.scrollbar_handle());
            for _ in 0..3 {
                vcx.update(|window, cx| window.draw(cx).clear(cx));
            }
            let pct = |bar: &RowScrollbar| {
                let max = bar.content_size().height.as_f32() - bar.viewport_bounds().size.height.as_f32();
                if max <= 0. { 0. } else { (-bar.offset().y.as_f32() / max).clamp(0., 1.) }
            };
            let frame = |vcx: &mut VisualTestContext| {
                for _ in 0..3 {
                    vcx.update(|window, cx| window.draw(cx).clear(cx));
                }
            };
            // Opens at the end for a new transcript.
            let overflow = bar.content_size().height > bar.viewport_bounds().size.height;
            println!("n={n}: overflow={overflow} pct_at_open={:.3}", pct(&bar));
            if !overflow {
                continue;
            }
            assert!(pct(&bar) > 0.999, "n={n}: transcript opens at its end, thumb at the bottom");

            let drag = |vcx: &mut VisualTestContext, bar: &RowScrollbar, to: f32| {
                let h = bar.content_size().height.as_f32();
                let thumb = (VIEW_H * VIEW_H / h).max(48.);
                let travel = VIEW_H - thumb;
                let y0 = pct(bar) * travel + 5.;
                let y1 = if to >= 1. { VIEW_H + 40. } else if to <= 0. { -40. } else { to * travel + 5. };
                vcx.simulate_mouse_down(point(px(VIEW_W - 8.), px(y0)), MouseButton::Left, Modifiers::default());
                for i in 1..=20 {
                    std::thread::sleep(Duration::from_millis(12));
                    let y = y0 + (y1 - y0) * i as f32 / 20.;
                    vcx.simulate_mouse_move(point(px(VIEW_W - 8.), px(y)), Some(MouseButton::Left), Modifiers::default());
                    frame(vcx);
                }
                vcx.simulate_mouse_up(point(px(VIEW_W - 8.), px(y1)), MouseButton::Left, Modifiers::default());
                frame(vcx);
            };
            drag(vcx, &bar, 0.0);
            let t = state.logical_scroll_top();
            assert!(t.item_ix == 0 && t.offset_in_item.as_f32() < 0.5, "n={n}: top reached, got {t:?}");
            assert!(pct(&bar) < 1e-3);
            drag(vcx, &bar, 0.5);
            let p = pct(&bar);
            println!("n={n}: dropped at 0.5 -> thumb {p:.3}");
            assert!((p - 0.5).abs() < 0.03, "n={n}: thumb stays where dropped, got {p}");
            drag(vcx, &bar, 1.0);
            assert_eq!(state.is_scrolled_to_end().unwrap_or(true), true, "n={n}: real end");
            assert!(pct(&bar) > 0.999, "n={n}: thumb at bottom, got {}", pct(&bar));
            let t = state.logical_scroll_top();
            let last = state.bounds_for_item(state.item_count() - 1);
            println!("n={n}: end top={t:?} last bounds={last:?}");
            if let Some(b) = last {
                assert!(b.bottom().as_f32() <= VIEW_H + 1. && b.bottom().as_f32() >= VIEW_H - 1.5, "n={n}: last row bottom flush with viewport, got {b:?}");
            }
        }
    }
}
