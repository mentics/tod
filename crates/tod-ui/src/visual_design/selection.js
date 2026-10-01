// Pure selection logic for the design bridge. No DOM globals, no dependencies:
// works on anything shaped like an element (tagName, id, classList/className,
// parentElement, children, attributes, getBoundingClientRect).
// Loaded in the page as window.TodSelect; required by node in its tests.
(function (root, factory) {
  var api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else root.TodSelect = api;
})(typeof self !== "undefined" ? self : this, function () {
  function rect(el) {
    var r = el.getBoundingClientRect();
    return { x: r.left, y: r.top, w: r.width, h: r.height };
  }
  // inner lies completely inside outer (edges may touch).
  function contains(outer, inner) {
    return inner.x >= outer.x && inner.y >= outer.y &&
      inner.x + inner.w <= outer.x + outer.w && inner.y + inner.h <= outer.y + outer.h;
  }
  function isAncestor(a, b) {
    for (var p = b.parentElement; p; p = p.parentElement) if (p === a) return true;
    return false;
  }
  // Elements whose box is completely inside `box`, keeping only the outermost
  // ones (an element is dropped when an ancestor of it is also selected).
  // `skip(el)` excludes html, body, the overlay and invisible things.
  function outermostInside(elements, box, skip) {
    var inside = [];
    for (var i = 0; i < elements.length; i++) {
      var el = elements[i];
      if (skip && skip(el)) continue;
      var r = rect(el);
      if (r.w <= 0 || r.h <= 0) continue;
      if (contains(box, r)) inside.push(el);
    }
    return inside.filter(function (el) {
      return !inside.some(function (o) { return o !== el && isAncestor(o, el); });
    });
  }
  function normBox(a, b) {
    return { x: Math.min(a.x, b.x), y: Math.min(a.y, b.y), w: Math.abs(a.x - b.x), h: Math.abs(a.y - b.y) };
  }
  function classesOf(el) {
    var c = el.classList ? Array.prototype.slice.call(el.classList) : String(el.className || "").split(/\s+/);
    return c.filter(Boolean);
  }
  function tag(el) { return String(el.tagName || "").toLowerCase(); }
  function cssEscape(s) { return String(s).replace(/[^a-zA-Z0-9_-]/g, "\\$&"); }
  function dataAttr(el) {
    var at = el.attributes || [];
    for (var i = 0; i < at.length; i++) {
      if (/^data-/.test(at[i].name) && !/^data-tod/.test(at[i].name) && at[i].value) return at[i];
    }
    return null;
  }
  function nth(el) {
    var p = el.parentElement;
    if (!p) return "";
    var same = Array.prototype.filter.call(p.children, function (c) { return tag(c) === tag(el); });
    return same.length > 1 ? ":nth-of-type(" + (same.indexOf(el) + 1) + ")" : "";
  }
  // One path segment, preferring a stable attribute.
  function segment(el) {
    if (el.id) return { s: "#" + cssEscape(el.id), stable: true };
    var d = dataAttr(el);
    if (d) return { s: tag(el) + "[" + d.name + '="' + String(d.value).replace(/"/g, '\\"') + '"]', stable: true };
    var cls = classesOf(el).slice(0, 2).map(function (c) { return "." + cssEscape(c); }).join("");
    return { s: tag(el) + cls + nth(el), stable: false };
  }
  // Short selector: walk up until a stable anchor (id / data-*), body, or 4 levels.
  function selectorFor(el) {
    var parts = [];
    for (var n = el; n && tag(n) !== "html" && tag(n) !== "body"; n = n.parentElement) {
      var seg = segment(n);
      parts.unshift(seg.s);
      if (seg.stable || parts.length >= 4) break;
    }
    return parts.join(" > ");
  }
  function breadcrumb(el) {
    var out = [];
    for (var n = el; n && tag(n) !== "html" && tag(n) !== "body"; n = n.parentElement) {
      var c = classesOf(n)[0];
      out.unshift(tag(n) + (n.id ? "#" + n.id : c ? "." + c : ""));
    }
    return out.slice(-4).join(" > ");
  }
  // Widen to the parent (never past body); narrow to the child toward `toward`
  // (the element the user last pointed at), else the first visible child.
  function widen(el) {
    var p = el.parentElement;
    return p && tag(p) !== "body" && tag(p) !== "html" ? p : el;
  }
  function narrow(el, toward) {
    if (toward && toward !== el && isAncestor(el, toward)) {
      var n = toward;
      while (n.parentElement !== el) n = n.parentElement;
      return n;
    }
    var kids = Array.prototype.filter.call(el.children || [], function (c) {
      var r = rect(c);
      return r.w > 0 && r.h > 0;
    });
    return kids[0] || el;
  }
  function scopeOf(el) {
    var kids = (el.children || []).length;
    if (tag(el) === "section" || tag(el) === "main" || kids > 6) return "region";
    return kids > 1 ? "container" : "element";
  }
  return { contains: contains, isAncestor: isAncestor, outermostInside: outermostInside, normBox: normBox,
    selectorFor: selectorFor, breadcrumb: breadcrumb, widen: widen, narrow: narrow, scopeOf: scopeOf, rect: rect };
});
