// Injected into every served mockup. Reloads on `reload`, keeps scroll across
// reloads, follows `navigate`. Design: doc/ui/visual-design-browser.md section 4.
(function () {
  var script = document.currentScript;
  var base = script && script.getAttribute("data-base");
  if (!base) return;
  var KEY = "tod-design-scroll:" + base;
  try {
    var saved = sessionStorage.getItem(KEY);
    if (saved) {
      sessionStorage.removeItem(KEY);
      var p = JSON.parse(saved);
      var restore = function () { window.scrollTo(p.x, p.y); };
      restore();
      window.addEventListener("load", restore);
    }
  } catch (e) {}
  function reload() {
    try {
      sessionStorage.setItem(KEY, JSON.stringify({ x: window.scrollX, y: window.scrollY }));
    } catch (e) {}
    location.reload();
  }
  var es = new EventSource(base + "__tod/events");
  es.addEventListener("reload", reload);
  es.addEventListener("navigate", function () {
    try { sessionStorage.removeItem(KEY); } catch (e) {}
    location.href = base;
  });
  es.addEventListener("closed", function () { es.close(); });
})();
