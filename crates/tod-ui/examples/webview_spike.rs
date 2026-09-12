//! Spike: does gpui_component::webview::WebView compile/link on this platform?
//!
//! Run with: cargo check -p tod-ui --example webview_spike
//! (build/run requires a display; check is sufficient signal for the spike)

use gpui::{App, AppContext, Application, Bounds, WindowBounds, WindowOptions, px, size};
use gpui_component::webview::WebView;

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(600.0), px(400.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                // Build a raw wry WebView bound to this window and wrap it.
                let wry_webview = wry::WebViewBuilder::new()
                    .with_html("<html><body><h1>tod webview spike</h1></body></html>")
                    .build_as_child(window)
                    .expect("failed to construct wry WebView");

                let mut webview = WebView::new(wry_webview, window, cx);
                webview.load_url("data:text/html,<h1>hello from tod</h1>");

                cx.new(|_| webview)
            },
        )
        .expect("failed to open window");
    });
}
