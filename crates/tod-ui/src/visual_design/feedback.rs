//! Feedback from the page rendered as the text of a user turn (design 7.2):
//! the comment first, then each selection's selector, text snippet and size.

use super::server::Feedback;

const SNIPPET: usize = 120;

pub fn render(fb: &Feedback) -> String {
    let mut out = String::new();
    let comment = fb.comment.trim();
    if !comment.is_empty() {
        out.push_str(comment);
        out.push_str("\n\n");
    }
    if fb.full_page {
        out.push_str("(Full-page screenshot.)\n");
    } else if fb.selections.is_empty() && fb.boxed.is_none() {
        out.push_str("(No element selected.)\n");
    }
    for s in &fb.selections {
        let text: String = s.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let text = if text.chars().count() > SNIPPET {
            format!("{}...", text.chars().take(SNIPPET).collect::<String>())
        } else {
            text
        };
        out.push_str(&format!(
            "- `{}` ({:.0} x {:.0})",
            s.selector, s.rect.w, s.rect.h
        ));
        if !text.is_empty() {
            out.push_str(&format!(": \"{text}\""));
        }
        out.push('\n');
    }
    if let Some(b) = &fb.boxed {
        out.push_str(&format!(
            "Region: {:.0} x {:.0} at ({:.0}, {:.0})\n",
            b.w, b.h, b.x, b.y
        ));
    }
    if let Some(note) = &fb.note {
        out.push_str(&format!("({note})\n"));
    }
    out.push_str(&format!(
        "Viewport: {:.0} x {:.0}\n",
        fb.viewport.w, fb.viewport.h
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual_design::server::{Rect, Selection, Viewport};

    #[test]
    fn comment_first_then_selections() {
        let fb = Feedback {
            comment: "Make it bigger".into(),
            selections: vec![Selection {
                selector: "#a > b".into(),
                tag: "b".into(),
                classes: vec![],
                text: "Hello   world".into(),
                rect: Rect { x: 1.0, y: 2.0, w: 200.0, h: 120.0 },
                scope: String::new(),
                outer_html: String::new(),
            }],
            boxed: None,
            viewport: Viewport { w: 800.0, h: 600.0, scroll_x: 0.0, scroll_y: 0.0 },
            ..Default::default()
        };
        let t = render(&fb);
        assert!(t.starts_with("Make it bigger"));
        assert!(t.contains("`#a > b` (200 x 120): \"Hello world\""));
    }
}
