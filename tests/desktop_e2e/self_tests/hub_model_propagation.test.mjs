import assert from "node:assert/strict";
import test from "node:test";
import { catalogPropagationAccepted } from "../scenarios/hub_model_propagation.mjs";

function pair() {
  const before = { hub_id:"hub-one", settings_revision:"7", catalog:{revision:"10"} };
  for (const name of ["main","side_chat","approve"]) {
    before[`${name}_review`] = { hub_id:"hub-one",reviewed_revision:"10",selection:{preferred_model_id:name} };
    before[`${name}_uses_default`] = name !== "main";
  }
  const after = structuredClone(before);
  after.status="connected"; after.catalog.revision="11"; after.settings_revision="8";
  after.recommended_side_chat_selection={preferred_model_id:"new-default"};
  after.recommended_approve_selection=null;
  after.main_review.reviewed_revision="11";
  after.side_chat_review={hub_id:"hub-one",reviewed_revision:"11",selection:{preferred_model_id:"new-default"}};
  after.approve_review.reviewed_revision="11";
  for (const name of ["main","side_chat","approve"]) {
    after[`${name}_confirmation`]="confirmed";
    after[`${name}_auto_update`]={status:"compared"};
  }
  return {before,after};
}

test("passive propagation follows the exact new catalog: explicit choices stay, Hub defaults move", () => {
  const {before,after}=pair();
  assert.equal(catalogPropagationAccepted(before,after,"11"),true);
  for (const change of [
    value=>{value.catalog.revision="10";},
    value=>{value.hub_id="another-hub";},
    value=>{value.approve_confirmation="review_required";},
    value=>{value.main_review.selection.preferred_model_id="silently-replaced";},
    value=>{value.side_chat_review.selection.preferred_model_id="side_chat";},
    value=>{value.main_review.reviewed_revision="10";},
    value=>{value.main_uses_default=true;},
    value=>{value.main_auto_update=null;},
  ]) {
    const changed=structuredClone(after); change(changed);
    assert.equal(catalogPropagationAccepted(before,changed,"11"),false);
  }
});

test("a role whose choice no longer fits keeps its review unchanged until the user saves", () => {
  const {before,after}=pair();
  after.side_chat_review=structuredClone(before.side_chat_review);
  after.side_chat_confirmation="review_required";
  after.side_chat_auto_update=null;
  assert.equal(catalogPropagationAccepted(before,after,"11",{side_chat:"review_required"}),true);
  assert.equal(catalogPropagationAccepted(before,after,"11"),false);
  const allBlocked=structuredClone(before);
  Object.assign(allBlocked,{status:"connected",catalog:{revision:"11"},main_confirmation:"review_required",side_chat_confirmation:"review_required",approve_confirmation:"review_required"});
  const blocked={main:"review_required",side_chat:"review_required",approve:"review_required"};
  assert.equal(catalogPropagationAccepted(before,allBlocked,"11",blocked),true);
  assert.equal(catalogPropagationAccepted(before,{...allBlocked,settings_revision:"8"},"11",blocked),false);
});
