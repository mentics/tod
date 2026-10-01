//! Screenshots over the Chrome DevTools Protocol (design 7.4).
//!
//! Chrome is launched with `--remote-debugging-port=0` (`Browser::debug_args`)
//! and writes the port it chose to `DevToolsActivePort` in our profile. We list
//! its targets over HTTP, pick the page whose title carries our token prefix
//! (never the first page: Chrome has other targets), open that page's websocket
//! and send `Page.captureScreenshot`. Everything here blocks, so call it off
//! the UI thread; the websocket client runs on a short-lived runtime.

use std::path::Path;
use std::time::Duration;

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use super::server::Feedback;

/// A region in CSS pixels, relative to the document (scroll already added).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Clip {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug)]
pub enum CdpError {
    /// No window is open, so there is nothing to capture.
    NoWindow,
    Msg(String),
}

impl std::fmt::Display for CdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CdpError::NoWindow => write!(f, "the design window is not open"),
            CdpError::Msg(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for CdpError {}

fn err<E: std::fmt::Display>(what: &str) -> impl FnOnce(E) -> CdpError + '_ {
    move |e| CdpError::Msg(format!("{what}: {e}"))
}

/// The longest side a screenshot keeps; larger ones are scaled down.
pub const MAX_SIDE: u32 = 1568;
const TIMEOUT: Duration = Duration::from_secs(15);

/// What to capture for `fb`: `None` for no screenshot, `Some(None)` for the
/// whole page, `Some(Some(clip))` for a region (the dragged box, else the
/// bounds of the selected elements).
pub fn clip_for(fb: &Feedback) -> Option<Option<Clip>> {
    if fb.full_page {
        return Some(None);
    }
    let (sx, sy) = (fb.viewport.scroll_x, fb.viewport.scroll_y);
    let rect = if let Some(b) = &fb.boxed {
        Some((b.x, b.y, b.x + b.w, b.y + b.h))
    } else {
        fb.selections.iter().map(|s| &s.rect).fold(None, |acc: Option<(f64, f64, f64, f64)>, r| {
            let (x1, y1, x2, y2) = (r.x, r.y, r.x + r.w, r.y + r.h);
            Some(match acc {
                None => (x1, y1, x2, y2),
                Some((a, b, c, d)) => (a.min(x1), b.min(y1), c.max(x2), d.max(y2)),
            })
        })
    };
    let (x1, y1, x2, y2) = rect?;
    if x2 - x1 < 1.0 || y2 - y1 < 1.0 {
        return None;
    }
    Some(Some(Clip { x: x1 + sx, y: y1 + sy, w: x2 - x1, h: y2 - y1 }))
}

/// The port on the first line of `DevToolsActivePort`.
pub fn parse_active_port(contents: &str) -> Option<u16> {
    contents.lines().next()?.trim().parse().ok()
}

/// The websocket URL of the page whose title starts with `prefix`.
pub fn pick_target(targets: &Value, prefix: &str) -> Option<String> {
    targets.as_array()?.iter().find_map(|t| {
        let is_page = t.get("type")?.as_str()? == "page";
        let title = t.get("title")?.as_str()?;
        (is_page && title.starts_with(prefix))
            .then(|| t.get("webSocketDebuggerUrl")?.as_str().map(str::to_string))
            .flatten()
    })
}

/// Shrink `png` so its longest side is at most `max`, keeping the aspect ratio.
/// Smaller images are returned unchanged.
pub fn downscale_png(png: &[u8], max: u32) -> Result<Vec<u8>, CdpError> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map_err(err("decode screenshot"))?;
    if img.width().max(img.height()) <= max {
        return Ok(png.to_vec());
    }
    let small = img.resize(max, max, image::imageops::FilterType::Lanczos3);
    let mut out = std::io::Cursor::new(Vec::new());
    small
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(err("encode screenshot"))?;
    Ok(out.into_inner())
}

/// A PNG of our page, downscaled to [`MAX_SIDE`]. `None` is the whole page.
pub fn capture_png(
    profile: &Path,
    title_prefix: &str,
    clip: Option<Clip>,
) -> Result<Vec<u8>, CdpError> {
    let file = std::fs::read_to_string(profile.join("DevToolsActivePort"))
        .map_err(err("read DevToolsActivePort"))?;
    let port = parse_active_port(&file)
        .ok_or_else(|| CdpError::Msg("DevToolsActivePort has no port".into()))?;
    let targets: Value = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .new_agent()
        .get(format!("http://127.0.0.1:{port}/json"))
        .call()
        .map_err(err("list browser targets"))?
        .body_mut()
        .read_json()
        .map_err(err("read browser targets"))?;
    let ws = pick_target(&targets, title_prefix)
        .ok_or_else(|| CdpError::Msg("the design page is not among the browser's targets".into()))?;
    // A short-lived runtime on its own thread, so the caller may be on any
    // thread (including one already inside a runtime).
    let png = std::thread::scope(|s| {
        s.spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(err("start runtime"))?
                .block_on(async {
                    tokio::time::timeout(TIMEOUT, screenshot(&ws, clip))
                        .await
                        .map_err(|_| CdpError::Msg("the browser did not answer in time".into()))?
                })
        })
        .join()
        .unwrap_or_else(|_| Err(CdpError::Msg("screenshot thread panicked".into())))
    })?;
    downscale_png(&png, MAX_SIDE)
}

async fn screenshot(ws_url: &str, clip: Option<Clip>) -> Result<Vec<u8>, CdpError> {
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_url)
        .await
        .map_err(err("connect to the page"))?;
    let mut next_id = 0u64;
    let mut call = |method: &'static str, params: Value| {
        next_id += 1;
        (next_id, json!({"id": next_id, "method": method, "params": params}).to_string())
    };
    let clip = match clip {
        Some(c) => c,
        None => {
            let (id, msg) = call("Page.getLayoutMetrics", json!({}));
            let r = request(&mut ws, id, msg).await?;
            let size = &r["cssContentSize"];
            Clip {
                x: 0.0,
                y: 0.0,
                w: size["width"].as_f64().unwrap_or(0.0).ceil(),
                h: size["height"].as_f64().unwrap_or(0.0).ceil(),
            }
        }
    };
    if clip.w < 1.0 || clip.h < 1.0 {
        return Err(CdpError::Msg("the page has no size".into()));
    }
    // Chrome multiplies a clip by the display's pixel ratio, so ask for the
    // inverse: one CSS pixel is then one image pixel on any display.
    let (id, msg) = call(
        "Runtime.evaluate",
        json!({"expression": "window.devicePixelRatio", "returnByValue": true}),
    );
    let dpr = request(&mut ws, id, msg).await?["result"]["value"]
        .as_f64()
        .filter(|d| *d > 0.0)
        .unwrap_or(1.0);
    let (id, msg) = call(
        "Page.captureScreenshot",
        json!({
            "format": "png",
            "captureBeyondViewport": true,
            "clip": {"x": clip.x, "y": clip.y, "width": clip.w, "height": clip.h, "scale": 1.0 / dpr},
        }),
    );
    let r = request(&mut ws, id, msg).await?;
    let data = r["data"]
        .as_str()
        .ok_or_else(|| CdpError::Msg("the browser returned no image".into()))?;
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(err("decode image"))
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Send `msg` and wait for the reply with `id`, skipping events.
async fn request(ws: &mut Ws, id: u64, msg: String) -> Result<Value, CdpError> {
    ws.send(Message::text(msg)).await.map_err(err("send"))?;
    while let Some(m) = ws.next().await {
        let Message::Text(t) = m.map_err(err("receive"))? else {
            continue;
        };
        let v: Value = serde_json::from_str(&t).map_err(err("parse reply"))?;
        if v["id"].as_u64() == Some(id) {
            if let Some(e) = v.get("error") {
                return Err(CdpError::Msg(format!("the browser refused: {e}")));
            }
            return Ok(v["result"].clone());
        }
    }
    Err(CdpError::Msg("the browser closed the connection".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_design::server::{Rect, Selection, Viewport};

    fn sel(x: f64, y: f64, w: f64, h: f64) -> Selection {
        Selection {
            selector: "x".into(),
            tag: "div".into(),
            classes: vec![],
            text: String::new(),
            rect: Rect { x, y, w, h },
            scope: String::new(),
            outer_html: String::new(),
        }
    }

    #[test]
    fn port_is_the_first_line() {
        assert_eq!(parse_active_port("51234\n/devtools/browser/abc\n"), Some(51234));
        assert_eq!(parse_active_port(""), None);
    }

    #[test]
    fn picks_our_page_not_the_first() {
        let t = json!([
            {"type": "service_worker", "title": "tod design abc", "webSocketDebuggerUrl": "ws://sw"},
            {"type": "page", "title": "Other", "webSocketDebuggerUrl": "ws://other"},
            {"type": "page", "title": "tod design abc12345 - Mockup", "webSocketDebuggerUrl": "ws://ours"},
        ]);
        assert_eq!(pick_target(&t, "tod design abc12345").as_deref(), Some("ws://ours"));
        assert_eq!(pick_target(&t, "tod design zzz"), None);
    }

    #[test]
    fn region_is_the_box_with_scroll_added() {
        let fb = Feedback {
            boxed: Some(Rect { x: 10.0, y: 20.0, w: 200.0, h: 120.0 }),
            viewport: Viewport { w: 800.0, h: 600.0, scroll_x: 5.0, scroll_y: 100.0 },
            selections: vec![sel(0.0, 0.0, 1.0, 1.0)],
            ..Default::default()
        };
        assert_eq!(clip_for(&fb), Some(Some(Clip { x: 15.0, y: 120.0, w: 200.0, h: 120.0 })));
    }

    #[test]
    fn region_without_a_box_is_the_selections_bounds() {
        let fb = Feedback {
            selections: vec![sel(10.0, 10.0, 50.0, 20.0), sel(100.0, 40.0, 30.0, 30.0)],
            ..Default::default()
        };
        assert_eq!(clip_for(&fb), Some(Some(Clip { x: 10.0, y: 10.0, w: 120.0, h: 60.0 })));
    }

    #[test]
    fn full_page_and_no_region() {
        let fb = Feedback { full_page: true, ..Default::default() };
        assert_eq!(clip_for(&fb), Some(None));
        assert_eq!(clip_for(&Feedback::default()), None);
    }

    #[test]
    fn downscale_keeps_aspect_and_leaves_small_images() {
        let img = image::RgbaImage::from_pixel(3000, 1500, image::Rgba([1, 2, 3, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let out = downscale_png(png.get_ref(), 1500).unwrap();
        let d = image::load_from_memory(&out).unwrap();
        assert_eq!((d.width(), d.height()), (1500, 750));
        assert_eq!(downscale_png(&out, 1500).unwrap(), out);
    }

    /// Real Chrome (path in `TOD_TEST_CHROME`, temp profile): region and
    /// full-page captures of a page with a 200x120 box and a 3000px height.
    #[test]
    #[ignore]
    fn real_chrome_region_and_full_page() {
        use std::io::{Read, Write};
        let Some(chrome) = std::env::var_os("TOD_TEST_CHROME") else { return };
        let page = "<html><head><title>tod design abc12345</title></head><body style='margin:0;height:3000px;background:#fff'><div style='position:absolute;left:100px;top:50px;width:200px;height:120px;background:#e33'>box</div></body></html>";
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for c in listener.incoming().flatten() {
                let mut c = c;
                let mut b = [0u8; 2048];
                let _ = c.read(&mut b);
                let _ = write!(c, "HTTP/1.1 200 OK
Content-Type: text/html
Content-Length: {}
Connection: close

{page}", page.len());
            }
        });
        let profile = std::env::temp_dir().join(format!("tod-cdp-test-{}", std::process::id()));
        let mut child = std::process::Command::new(chrome)
            .arg(format!("--app=http://127.0.0.1:{port}/"))
            .arg(format!("--user-data-dir={}", profile.display()))
            .args(["--no-first-run", "--no-default-browser-check", "--remote-debugging-port=0", "--window-size=900,700"])
            .spawn()
            .unwrap();
        let mut region = Err(CdpError::NoWindow);
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(250));
            region = capture_png(&profile, "tod design abc12345", Some(Clip { x: 100.0, y: 50.0, w: 200.0, h: 120.0 }));
            if region.is_ok() { break; }
        }
        let full = capture_png(&profile, "tod design abc12345", None);
        let _ = child.kill();
        let region = image::load_from_memory(&region.unwrap()).unwrap().to_rgba8();
        assert_eq!((region.width(), region.height()), (200, 120));
        // Red everywhere, up to resampling and colour management.
        for (x, y) in [(2, 2), (197, 2), (2, 117), (197, 117)] {
            let p = region.get_pixel(x, y).0;
            assert!(p[0] > 200 && p[1] < 90 && p[2] < 90, "{x},{y}: {p:?}");
        }
        let full = image::load_from_memory(&full.unwrap()).unwrap();
        println!("full page {}x{}", full.width(), full.height());
        assert_eq!(full.height(), 3000.min(MAX_SIDE));
        let _ = std::fs::remove_dir_all(&profile);
    }
}
