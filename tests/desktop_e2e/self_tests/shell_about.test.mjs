import assert from "node:assert/strict";
import test from "node:test";

import { aboutMetadataReady } from "../scenarios/shell_about.mjs";

test("About requires visible exact metadata matching the Rust projection", () => {
  const expected = { version: "3.0.0", codename: "LYNX" };
  const ready = {
    overlay: "about", dialog_count: 1, dialog_visible: true, aria_modal: "true",
    focus_inside_dialog: true, title: "moyAIについて", fatal_count: 0,
    about: { product_name: "moyAI", ...expected },
    rows: [
      { label: "バージョン", value: "3.0.0", visible: true },
      { label: "コードネーム", value: "LYNX", visible: true },
    ],
  };
  assert.equal(aboutMetadataReady(ready, expected), true);
  for (const change of [
    (value) => { value.rows[1].value = "old codename"; },
    (value) => { value.rows[0].value = "2.1.1"; },
    (value) => { value.rows[1].visible = false; },
    (value) => { value.rows.push(value.rows[1]); },
    (value) => { value.about.codename = "old codename"; },
    (value) => { value.focus_inside_dialog = false; },
    (value) => { value.dialog_count = 2; },
  ]) {
    const invalid = structuredClone(ready);
    change(invalid);
    assert.equal(aboutMetadataReady(invalid, expected), false);
  }
});
