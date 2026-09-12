import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { createMcpReceiverPermissionScenario } from "../scenarios/mcp_receiver_live.mjs";
import { receiverPermissionPlan, receiverPermissionScenarioId, receiverPermissionMainUnchanged,
  receiverPermissionLedgerReady, receiverPermissionPendingFailures, receiverPermissionTerminalFailures } from "../scenarios/mcp_receiver_permission.mjs";
import { createChatToolContinuationProviderScript } from "../drivers/scripted_provider.mjs";

const JOB = "01ARZ3NDEKTSV4RRFFQ69G5FAV", PROFILE = "01ARZ3NDEKTSV4RRFFQ69G5FAW", CONFIRM = "01ARZ3NDEKTSV4RRFFQ69G5FAX", SESSION = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
function ledger(phase) {
  const row = (role, held) => ({ route:"chat_completions", method:"POST", pathname:"/v1/chat/completions", query_present:false,
    contract:{pass:true,role},response_phase:held?"held":"completed",response_status:held?null:200 });
  return [{route:"models",method:"GET",pathname:"/v1/models",response_status:200},row("chat_tool_initial",false),
    ...(["held","completed"].includes(phase)?[row("chat_continuation",phase==="held")]:[])];
}
function pending() {
  const main = {workspace:"C:/owned",draftTarget:{workspacePath:"C:/owned",sessionId:null,ownerGeneration:"1"},
    runTarget:{sessionId:null},transcript:[],draft:"unsent",runStatus:"idle"};
  const expected = {main,jobId:JOB,profileId:PROFILE,requesterLabel:"device-a",command:"Write a receipt",decision:"approved",responseText:"REPLY"};
  const projection = {workspace_path:main.workspace,draft_target:main.draftTarget,run_target:main.runTarget,
    transcript_rows:[],busy:false,run_status_key:"idle",confirmation_visible:true,confirmation_id:CONFIRM,
    confirmation:{remote:{job_id:JOB,profile_id:PROFILE,session_id:SESSION,target_label:"temp",requester_label:"device-a"}},mcp_activity:{awaiting_approval:1}};
  const surface = {projection,prompt:"unsent",promptEnabled:false,shellInert:true,awaitingBadge:1,errors:0,
    dialog:{count:1,id:CONFIRM,visible:true,busy:false,title:"受入タスクの操作を確認",focusedAction:"deny-permission",command:"Command: Write a receipt",text:`${JOB} device-a`,buttons:[
      {action:"deny-permission",label:"許可しない",visible:true,enabled:true},
      {action:"abort-permission",label:"タスクを停止",visible:true,enabled:true},
      {action:"approve-permission",label:"この操作を許可",visible:true,enabled:true},
    ]}};
  return {expected,sample:{surface,job:{state:"awaiting_approval"},ledger:ledger("pending"),receipt:null}};
}
function terminal(decision) {
  const {sample,expected}=pending(); expected.decision=decision;
  Object.assign(sample.surface.projection,{confirmation_visible:false,confirmation_id:null,confirmation:null});
  Object.assign(sample.surface,{dialog:{count:0},shellInert:false,promptEnabled:true});
  sample.job={state:decision==="abort"?"interrupted":"completed",result:decision==="abort"?null:"REPLY"};
  sample.ledger=ledger(decision==="abort"?"abort":"completed");sample.receipt=decision==="approved"?"REMOTE_APPROVAL_FILE":null;
  return {sample,expected};
}

test("three fresh receiver decision variants reuse the existing browser/Desktop lifecycle",()=>{
  for(const [decision,id]of Object.entries({approved:"mcp.receiver-approve",denied:"mcp.receiver-deny",abort:"mcp.receiver-abort"})){
    const scenario=createMcpReceiverPermissionScenario({decision});assert.equal(scenario.id,id);assert.equal(scenario.manualGate,"pending");
    assert.equal(scenario.databaseRequired,true);assert.equal("launch"in scenario,false);
    for(const method of["prepare","execute","requestGracefulExit","quiesce","cleanup"])assert.equal(typeof scenario[method],"function");
  }
  for(const decision of[undefined,"allow","stop",true])assert.throws(()=>receiverPermissionScenarioId(decision));
  assert.throws(()=>createMcpReceiverPermissionScenario({decision:"approved",ignoreHTTPSErrors:true}));
});

test("permission plan safely quotes an owned path and tests the effect independently from command echo",()=>{
  for(const decision of["approved","denied","abort"]){
    const root=path.resolve("owned o'hare"), plan=receiverPermissionPlan(root,decision);
    assert.equal(path.dirname(plan.receiptPath),root);assert.ok(plan.call.arguments.command.includes("o''hare"));
    assert.equal(plan.call.arguments.sandbox_permissions,"require_escalated");assert.equal(plan.call.arguments.timeout_ms,5000);
    assert.equal(plan.call.name,"shell");assert.equal(plan.receiptText,"REMOTE_APPROVAL_FILE");
    assert.deepEqual(createChatToolContinuationProviderScript({call:plan.call}).call,plan.call);
    assert.equal(plan.call.outputMarker,decision==="denied"?"permission denied by receiver user":"Stdout:\nREMOTE_APPROVAL_OK");
  }
  assert.throws(()=>receiverPermissionPlan("relative","approved"));
});

test("permission provider oracle distinguishes pending/held/replied and no-continuation abort",()=>{
  for(const phase of["pending","held","completed","abort"])assert.equal(receiverPermissionLedgerReady(ledger(phase),phase),true);
  assert.equal(receiverPermissionLedgerReady(ledger("completed"),"abort"),false);
  assert.equal(receiverPermissionLedgerReady(ledger("held"),"completed"),false);
  for(const invalid of[null,{},[],[...ledger("pending"),{route:"unknown"}],[...ledger("pending"),...ledger("pending")]])assert.equal(receiverPermissionLedgerReady(invalid,"pending"),false);
  for(const mutation of[row=>row.contract.pass=false,row=>row.response_phase="rejected",row=>row.response_status=500,row=>row.contract.role="other",row=>row.query_present=true]){
    const rows=ledger("pending");mutation(rows[1]);assert.equal(receiverPermissionLedgerReady(rows,"pending"),false);
  }
});

test("pending permission requires exact job/profile/confirmation owner and three correctly labelled buttons",()=>{
  const {sample,expected}=pending();assert.deepEqual(receiverPermissionPendingFailures(sample,expected),[]);
  for(const mutate of[
    v=>v.job.state="running",v=>v.surface.projection.confirmation_id="not-an-id",
    v=>v.surface.projection.confirmation.remote.job_id=SESSION,v=>v.surface.projection.confirmation.remote.profile_id=SESSION,
    v=>v.surface.projection.confirmation.remote.requester_label="other",v=>v.surface.dialog.id=SESSION,
    v=>v.surface.dialog.visible=false,v=>v.surface.dialog.focusedAction="approve-permission",v=>v.surface.dialog.command="other command",
    v=>v.surface.dialog.buttons[0].label="実行する",v=>v.surface.dialog.buttons[0].enabled=false,
    v=>v.surface.dialog.buttons.push({...v.surface.dialog.buttons[0]}),v=>v.surface.awaitingBadge=0,
    v=>v.surface.shellInert=false,v=>v.surface.errors=1,
  ]){const changed=structuredClone(sample);mutate(changed);assert.notEqual(receiverPermissionPendingFailures(changed,expected).length,0);}
});

test("Main owner and draft remain independent of the remotely accepted task",()=>{
  const {sample,expected}=pending();assert.equal(receiverPermissionMainUnchanged(sample.surface,expected.main),true);
  for(const mutate of[v=>v.prompt="changed",v=>v.projection.run_target.sessionId=SESSION,v=>v.projection.draft_target.ownerGeneration="2",
    v=>v.projection.workspace_path="D:/other",v=>v.projection.busy=true,v=>v.projection.transcript_rows.push({row_kind:"user",body:"remote request"})]){
    const changed=structuredClone(sample.surface);mutate(changed);assert.equal(receiverPermissionMainUnchanged(changed,expected.main),false);
  }
});

test("terminal decision verifies actual receipt creation or noncreation and rejects a lingering prompt",()=>{
  for(const decision of["approved","denied","abort"]){
    const {sample,expected}=terminal(decision);assert.deepEqual(receiverPermissionTerminalFailures(sample,expected),[]);
    for(const mutate of[v=>v.receipt=decision==="approved"?null:"REMOTE_APPROVAL_FILE",v=>v.job.state="failed",
      v=>v.surface.projection.confirmation_visible=true,v=>v.surface.dialog.count=1,v=>v.surface.shellInert=true,
      v=>v.surface.promptEnabled=false,v=>v.surface.prompt="changed"]){
      const changed=structuredClone(sample);mutate(changed);assert.notEqual(receiverPermissionTerminalFailures(changed,expected).length,0);
    }
    if(decision!=="abort"){const changed=structuredClone(sample);changed.job.result="unrelated reply";assert.ok(receiverPermissionTerminalFailures(changed,expected).includes("remote-result"));}
  }
});
