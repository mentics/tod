// Run: node crates/tod-ui/src/visual_design/selection.test.js
const assert = require("assert");
const S = require("./selection.js");
function el(tagName, r, o = {}, kids = []) {
  const e = { tagName: tagName.toUpperCase(), id: o.id || "", className: o.cls || "", children: kids, parentElement: null,
    attributes: o.attrs || [], getBoundingClientRect: () => ({ left: r[0], top: r[1], width: r[2], height: r[3] }) };
  kids.forEach((k) => (k.parentElement = e));
  return e;
}
const btn = el("button", [20, 20, 30, 10], { cls: "primary" });
const c1 = el("div", [10, 10, 100, 100], { cls: "card" }, [btn]);
const c2 = el("div", [10, 120, 100, 100], { cls: "card" });
const c3 = el("div", [300, 10, 100, 100], { cls: "card" });
const cards = el("div", [0, 0, 500, 300], { cls: "cards" }, [c1, c2, c3]);
const main = el("main", [0, 0, 600, 400], {}, [cards]);
const body = el("body", [0, 0, 800, 600], {}, [main]);
el("html", [0, 0, 800, 600], {}, [body]);
const all = [main, cards, c1, btn, c2, c3];

assert(S.contains({ x: 0, y: 0, w: 10, h: 10 }, { x: 0, y: 0, w: 10, h: 10 }));
assert(!S.contains({ x: 0, y: 0, w: 10, h: 10 }, { x: 5, y: 5, w: 10, h: 10 }));
// Outermost only: box covers c1 and c2 (button implied), not c3.
assert.deepStrictEqual(S.outermostInside(all, { x: 0, y: 0, w: 150, h: 240 }), [c1, c2]);
assert.deepStrictEqual(S.outermostInside(all, { x: 0, y: 0, w: 700, h: 500 }), [main]);
// Partial coverage selects nothing; skip filter honoured.
assert.deepStrictEqual(S.outermostInside(all, { x: 0, y: 0, w: 40, h: 40 }), []);
assert.deepStrictEqual(S.outermostInside(all, { x: 0, y: 0, w: 150, h: 240 }, (e) => e === c2), [c1]);
// Widen / narrow.
assert.strictEqual(S.widen(btn), c1);
assert.strictEqual(S.widen(c1), cards);
assert.strictEqual(S.widen(main), main, "never past body");
assert.strictEqual(S.narrow(cards, btn), c1);
assert.strictEqual(S.narrow(c1), btn);
// Selectors.
assert.strictEqual(S.selectorFor(c2), "main > div.cards > div.card:nth-of-type(2)");
const withId = el("div", [0, 0, 1, 1], { id: "hero" }, [el("p", [0, 0, 1, 1])]);
assert.strictEqual(S.selectorFor(withId.children[0]), "#hero > p");
const withData = el("section", [0, 0, 1, 1], { attrs: [{ name: "data-id", value: "x" }] });
assert.strictEqual(S.selectorFor(withData), 'section[data-id="x"]');
assert.strictEqual(S.breadcrumb(btn), "main > div.cards > div.card > button.primary");
assert.strictEqual(S.normBox({ x: 10, y: 20 }, { x: 4, y: 30 }).w, 6);
console.log("selection ok");
