import assert from "node:assert/strict";
import test from "node:test";
import { createHash } from "node:crypto";
import {
  REVIEW_SUBMIT_RAW, REVIEW_SUBMIT_PROPOSAL, REVIEW_SUBMIT_EDITED, REVIEW_SUBMIT_REPLY,
  createPromptReviewSubmitScenario, reviewSubmitLedgerValid, reviewSubmitOpenedFailures,
  reviewSubmitCancelledFailures, reviewSubmitCompletedFailures,
} from "../scenarios/prompt_review_submit.mjs";

const expected={workspacePath:"C:\\fixture",draftTarget:{workspacePath:"C:\\fixture",sessionId:null,ownerGeneration:"3"},runTarget:{workspacePath:"C:\\fixture",sessionId:null,runtimeOwnerToken:"idle:3",permissionConfirmationId:null,expectedState:{kind:"idle",admissionRevision:"0",latestTurnId:null}},composerCommitGeneration:"0"};
const ledger=prompts=>prompts.map(prompt=>({route:"responses",method:"POST",pathname:"/v1/responses",response_status:200,response_phase:"completed",contract:{pass:true,input_text_sha256:createHash("sha256").update(prompt).digest("hex")}}));
function opened(draft=REVIEW_SUBMIT_PROPOSAL){return {
  projection:{workspace_path:expected.workspacePath,draft_target:structuredClone(expected.draftTarget),run_target:structuredClone(expected.runTarget),composer_commit_generation:"0",run_status_key:"idle",busy:false,navigation_loading:false,background_mutation_pending:false,async_polling_required:false,transcript_rows:[],overlay:"prompt_review",review_target:{...expected.draftTarget,requestId:"1",expectedState:expected.runTarget.expectedState},review_raw_text:REVIEW_SUBMIT_RAW},
  errors:0,dialog_count:1,dialog_visible:true,shell_inert:true,raw:REVIEW_SUBMIT_RAW,draft:{value:draft,visible:true,enabled:true},prompt:{value:REVIEW_SUBMIT_RAW,enabled:false},
  buttons:[{action:"cancel-review",visible:true,enabled:true},{action:"send-review-raw",visible:true,enabled:true},{action:"send-review-enhanced",visible:true,enabled:draft.trim().length>0}],
};}
function completed(choice="enhanced"){
  const dispatch=choice==="raw"?REVIEW_SUBMIT_RAW:REVIEW_SUBMIT_EDITED;
  return {projection:{workspace_path:expected.workspacePath,draft_target:{...expected.draftTarget,sessionId:"01ARZ3NDEKTSV4RRFFQ69G5FAV"},run_status_key:"completed",task_activity_state:"idle",busy:false,agent_tree_active:false,navigation_loading:false,post_run_refresh_pending:false,background_mutation_pending:false,async_polling_required:false,overlay:"none",review_target:null,transcript_rows:[{row_kind:"user",body:dispatch},{row_kind:"assistant",body:REVIEW_SUBMIT_REPLY}]},errors:0,dialog_count:0,shell_inert:false,prompt:{value:"",enabled:true},stop_visible:false,main_text:`${dispatch}\n${REVIEW_SUBMIT_REPLY}`};
}

test("review submit factories expose independent native visual gates",()=>{
  for(const choice of ["raw","enhanced"]){const c=createPromptReviewSubmitScenario({choice});assert.equal(c.id,`prompt-review.submit-${choice}`);assert.equal(c.manualGate,"pending");assert.equal(c.databaseRequired,true);}
  assert.throws(()=>createPromptReviewSubmitScenario({choice:"both"}),TypeError);
});
test("review empty enhanced draft forbids enhanced send but keeps original send",()=>{
  assert.deepEqual(reviewSubmitOpenedFailures(opened(""),expected,""),[]);
  const bad=opened("");bad.buttons.find(x=>x.action==="send-review-enhanced").enabled=true;
  assert.ok(reviewSubmitOpenedFailures(bad,expected,"").includes("button:send-review-enhanced"));
  const raw=opened("");raw.buttons.find(x=>x.action==="send-review-raw").enabled=false;
  assert.ok(reviewSubmitOpenedFailures(raw,expected,"").includes("button:send-review-raw"));
});
test("review local edit does not require pushing typed text into persisted projection",()=>{
  const value=opened(REVIEW_SUBMIT_EDITED);value.projection.review_draft_text=REVIEW_SUBMIT_PROPOSAL;
  assert.deepEqual(reviewSubmitOpenedFailures(value,expected,REVIEW_SUBMIT_EDITED),[]);
  value.draft.value=REVIEW_SUBMIT_PROPOSAL;
  assert.ok(reviewSubmitOpenedFailures(value,expected,REVIEW_SUBMIT_EDITED).includes("edited-draft"));
});
test("review raw and owner cannot drift while a review is open",()=>{
  for(const mutate of [v=>v.projection.draft_target.ownerGeneration="4",v=>v.projection.run_target.runtimeOwnerToken="other",v=>v.raw="wrong raw",v=>v.projection.review_target.sessionId="01ARZ3NDEKTSV4RRFFQ69G5FAV",v=>v.shell_inert=false]){
    const value=opened();mutate(value);assert.notDeepEqual(reviewSubmitOpenedFailures(value,expected,REVIEW_SUBMIT_PROPOSAL),[]);
  }
});
test("cancel requires both Main preservation and retired review state",()=>{
  const value=opened();Object.assign(value.projection,{overlay:"none",review_target:null,review_raw_text:"",review_draft_text:""});Object.assign(value,{dialog_count:0,shell_inert:false,prompt:{value:REVIEW_SUBMIT_RAW,enabled:true}});
  assert.deepEqual(reviewSubmitCancelledFailures(value,expected),[]);
  value.projection.review_draft_text=REVIEW_SUBMIT_EDITED;
  assert.ok(reviewSubmitCancelledFailures(value,expected).includes("cancel-review-state"));
});
test("provider oracle distinguishes raw and edited wire text and duplicate sends",()=>{
  const prompts=[REVIEW_SUBMIT_RAW,REVIEW_SUBMIT_RAW,REVIEW_SUBMIT_EDITED];
  assert.equal(reviewSubmitLedgerValid(ledger(prompts),prompts),true);
  assert.equal(reviewSubmitLedgerValid(ledger([...prompts,REVIEW_SUBMIT_EDITED]),prompts),false);
  assert.equal(reviewSubmitLedgerValid(ledger([REVIEW_SUBMIT_RAW,REVIEW_SUBMIT_RAW,REVIEW_SUBMIT_RAW]),prompts),false);
  const bad=ledger(prompts);bad[2].contract.pass=false;assert.equal(reviewSubmitLedgerValid(bad,prompts),false);
  bad[2].contract.pass=true;bad[2].response_phase="held";assert.equal(reviewSubmitLedgerValid(bad,prompts),false);
});
test("completed review verifies dispatched canonical text and real terminal DOM for both choices",()=>{
  for(const choice of ["raw","enhanced"]){const dispatch=choice==="raw"?REVIEW_SUBMIT_RAW:REVIEW_SUBMIT_EDITED;const rows=ledger([REVIEW_SUBMIT_RAW,REVIEW_SUBMIT_RAW,dispatch]);const options={choice,workspacePath:expected.workspacePath};
    assert.deepEqual(reviewSubmitCompletedFailures(completed(choice),rows,options),[]);
    for(const mutate of [v=>v.projection.transcript_rows[0].body="wrong choice",v=>v.stop_visible=true,v=>v.projection.run_status_key="running",v=>v.projection.workspace_path="C:\\other",v=>v.main_text="",v=>v.prompt.value=REVIEW_SUBMIT_RAW]){
      const value=completed(choice);mutate(value);assert.notDeepEqual(reviewSubmitCompletedFailures(value,rows,options),[]);
    }
  }
});

test("Enhance menu/palette variant retains the same bounded submit lifecycle",()=>{
  const value=createPromptReviewSubmitScenario({choice:"enhanced",enhanceEntries:"menu-palette"});
  assert.equal(value.id,"prompt-review.entries-enhanced");
  assert.equal(value.manualGate,"pending");
  for(const name of ["prepare","execute","requestGracefulExit","quiesce","cleanup"])assert.equal(typeof value[name],"function");
  assert.equal(createPromptReviewSubmitScenario({choice:"raw"}).id,"prompt-review.submit-raw");
  assert.throws(()=>createPromptReviewSubmitScenario({choice:"enhanced",enhanceEntries:"unknown"}));
});
