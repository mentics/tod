// Selection overlay for the design bridge (design 7.1, 7.3). Drawn in a shadow
// root on a top-level host so the mockup's CSS cannot touch it. Pure logic is
// in selection.js (window.TodSelect). Started by bridge.js: TodOverlay.start(base).
(function () {
  var S = window.TodSelect;
  function start(base) {
    var host = document.createElement("div");
    host.setAttribute("data-tod-overlay", "");
    host.style.cssText = "all:initial;position:fixed;inset:0;z-index:2147483647;pointer-events:none";
    var root = host.attachShadow({ mode: "open" });
    root.innerHTML =
      "<style>.o{position:fixed;border:2px solid #3b82f6;background:rgba(59,130,246,.12);pointer-events:none;box-sizing:border-box}" +
      ".h{border-style:dashed;background:none}.box{border:1px dashed #f59e0b;background:rgba(245,158,11,.08)}" +
      ".bar{position:fixed;right:12px;bottom:12px;font:12px system-ui;background:#111;color:#eee;padding:6px 8px;border-radius:6px;pointer-events:auto;display:flex;gap:6px;align-items:center}" +
      ".bar button{font:inherit;cursor:pointer}.crumb{position:fixed;left:12px;bottom:12px;font:12px ui-monospace,monospace;background:#111;color:#9cf;padding:4px 8px;border-radius:6px;pointer-events:none;display:none}" +
      ".cm{position:fixed;width:280px;pointer-events:auto;font:12px system-ui;background:#111;color:#eee;padding:6px;border-radius:6px;display:none}" +
      ".cm textarea{width:100%;height:60px;box-sizing:border-box}</style>" +
      '<div class="crumb"></div><div class="bar"><button class="pick">Pick (Alt+P)</button><button class="wide">[ widen</button><button class="narrow">] narrow</button><button class="full">Full page</button></div>' +
      '<div class="cm"><textarea placeholder="Comment, Ctrl+Enter to send, Esc to cancel"></textarea></div>';
    document.documentElement.appendChild(host);
    var crumb = root.querySelector(".crumb"), pickBtn = root.querySelector(".pick");
    var cm = root.querySelector(".cm"), ta = root.querySelector("textarea");
    var picking = false, selected = [], hover = null, anchor = null, drag = null, boxRect = null, sentBox = null;
    var marks = [];

    function ours(el) { return el === host || (el && el.closest && el.closest("[data-tod-overlay]")); }
    function skip(el) {
      var t = el.tagName.toLowerCase();
      return t === "html" || t === "body" || t === "script" || t === "style" || t === "head" || ours(el);
    }
    function draw(rects, cls) {
      rects.forEach(function (r) {
        var d = document.createElement("div");
        d.className = "o " + (cls || "");
        d.style.cssText = "left:" + r.x + "px;top:" + r.y + "px;width:" + r.w + "px;height:" + r.h + "px";
        root.appendChild(d); marks.push(d);
      });
    }
    function render(preview) {
      marks.forEach(function (m) { m.remove(); }); marks = [];
      draw(selected.map(S.rect));
      if (hover && picking && !drag) draw([S.rect(hover)], "h");
      if (drag && boxRect) draw([boxRect], "box");
      if (preview) draw(preview.map(S.rect), "h");
      var last = selected[selected.length - 1];
      crumb.style.display = last ? "block" : "none";
      if (last) crumb.textContent = S.breadcrumb(last) + "  ([ widen, ] narrow, Esc clear)";
    }
    function setPick(on) {
      picking = on; pickBtn.style.fontWeight = on ? "bold" : "normal";
      if (!on) { hover = null; drag = null; }
      render();
    }
    function target(x, y) {
      var el = document.elementFromPoint(x, y);
      return el && !skip(el) ? el : null;
    }
    function inRect(box) {
      return S.outermostInside(Array.prototype.slice.call(document.body.querySelectorAll("*")), box, skip);
    }
    function openComment() {
      var last = selected[selected.length - 1] || null, r = last ? S.rect(last) : sentBox;
      if (!r) return;
      cm.style.display = "block";
      cm.style.left = Math.max(8, Math.min(r.x, innerWidth - 300)) + "px";
      cm.style.top = Math.max(8, Math.min(r.y + r.h + 6, innerHeight - 110)) + "px";
      ta.focus();
    }
    function closeComment() { cm.style.display = "none"; ta.value = ""; }
    function clear() { selected = []; sentBox = null; closeComment(); render(); }
    function widen() { var i = selected.length - 1; if (i >= 0) { selected[i] = S.widen(selected[i]); render(); } }
    function narrow() { var i = selected.length - 1; if (i >= 0) { selected[i] = S.narrow(selected[i], anchor); render(); } }
    function text(el) { return (el.textContent || "").replace(/\s+/g, " ").trim().slice(0, 200); }
    // Hide the overlay and wait two frames so it is not in the capture, post,
    // then show it again.
    function post(body) {
      host.style.visibility = "hidden";
      var shown = function () { host.style.visibility = ""; };
      requestAnimationFrame(function () { requestAnimationFrame(function () {
        fetch(base + "__tod/feedback", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) })
          .then(function () { clear(); shown(); }, shown);
      }); });
    }
    function submit() {
      var body = {
        comment: ta.value,
        selections: selected.map(function (el) {
          return { selector: S.selectorFor(el), tag: el.tagName.toLowerCase(), classes: Array.prototype.slice.call(el.classList),
            text: text(el), rect: S.rect(el), scope: S.scopeOf(el), outerHtml: el.outerHTML.slice(0, 2000) };
        }),
        viewport: { w: innerWidth, h: innerHeight, scrollX: scrollX, scrollY: scrollY }
      };
      if (sentBox) body.box = sentBox;
      post(body);
    }
    function submitFullPage() {
      post({ comment: ta.value, selections: [], fullPage: true,
        viewport: { w: innerWidth, h: innerHeight, scrollX: scrollX, scrollY: scrollY } });
    }

    pickBtn.addEventListener("click", function () { setPick(!picking); });
    root.querySelector(".wide").addEventListener("click", widen);
    root.querySelector(".narrow").addEventListener("click", narrow);
    root.querySelector(".full").addEventListener("click", submitFullPage);
    ta.addEventListener("keydown", function (e) {
      e.stopPropagation();
      if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) { e.preventDefault(); submit(); }
      else if (e.key === "Escape") { e.preventDefault(); closeComment(); }
    });
    document.addEventListener("keydown", function (e) {
      if (e.altKey && (e.key === "p" || e.key === "P")) { e.preventDefault(); setPick(!picking); return; }
      if (!picking || cm.style.display === "block") return;
      if (e.key === "Escape") clear();
      else if (e.key === "[") widen();
      else if (e.key === "]") narrow();
    }, true);
    document.addEventListener("mousemove", function (e) {
      if (!picking) return;
      if (drag) {
        boxRect = S.normBox(drag, { x: e.clientX, y: e.clientY });
        render(boxRect.w > 4 || boxRect.h > 4 ? inRect(boxRect) : null);
      } else { hover = target(e.clientX, e.clientY); render(); }
    }, true);
    document.addEventListener("mousedown", function (e) {
      if (!picking || e.button !== 0 || ours(e.target) || cm.style.display === "block") return;
      e.preventDefault(); drag = { x: e.clientX, y: e.clientY }; boxRect = null;
    }, true);
    document.addEventListener("mouseup", function (e) {
      if (!picking || !drag) return;
      e.preventDefault();
      var b = boxRect, d = drag; drag = null; boxRect = null;
      if (b && (b.w > 4 || b.h > 4)) {
        var found = inRect(b);
        selected = e.shiftKey ? selected.concat(found) : found; sentBox = b;
      } else {
        var el = target(d.x, d.y);
        if (el) { anchor = el; selected = e.shiftKey ? selected.concat([el]) : [el]; sentBox = null; }
      }
      render(); if (selected.length) openComment();
    }, true);
    document.addEventListener("click", function (e) {
      if (picking && !ours(e.target)) { e.preventDefault(); e.stopPropagation(); }
    }, true);
  }
  window.TodOverlay = { start: start };
})();
