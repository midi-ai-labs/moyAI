import assert from "node:assert/strict";
import test from "node:test";
import { catalogPropagationAccepted } from "../scenarios/hub_model_propagation.mjs";

function pair() {
  const before = { hub_id:"hub-one", settings_revision:"7", catalog:{revision:"10"} };
  for (const name of ["main","side_chat","approve"]) {
    before[`${name}_review`] = { reviewed_revision:"10",selection:{preferred_model_id:name} };
    before[`${name}_uses_default`] = name !== "main";
  }
  const after = structuredClone(before);
  after.status="connected"; after.catalog.revision="11";
  for (const name of ["main","side_chat","approve"]) after[`${name}_confirmation`]="review_required";
  return {before,after};
}

test("passive propagation requires the exact new catalog without rewriting any role selection", () => {
  const {before,after}=pair();
  assert.equal(catalogPropagationAccepted(before,after,"11"),true);
  for (const change of [
    value=>{value.catalog.revision="10";},
    value=>{value.settings_revision="8";},
    value=>{value.hub_id="another-hub";},
    value=>{value.approve_confirmation="confirmed";},
    value=>{value.side_chat_review.selection.preferred_model_id="silently-replaced";},
    value=>{value.main_uses_default=true;},
  ]) {
    const changed=structuredClone(after); change(changed);
    assert.equal(catalogPropagationAccepted(before,changed,"11"),false);
  }
});
