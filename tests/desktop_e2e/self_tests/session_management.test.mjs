import assert from "node:assert/strict";
import test from "node:test";
import {
  createSessionManagementScenario,
  managementConfirmationReady,
  managementFocusedTarget,
  managementRowCommand,
  managementRowLocator,
  managementRowsMatch,
  managementSearchReady,
  managementSnapshot,
} from "../scenarios/session_management.mjs";

const PROJECT = "01M28000000000000000000001";
const ALPHA = "01M28000000000000000000002";
const BETA = "01M28000000000000000000003";
const QUICK = "01M28000000000000000000004";
function surface() {
  return {
    errors:0,dialog_count:0,prompt:"未送信のメモ",
    projection:{
      workspace_path:"C:/task/日本語 workspace",draft_target:{sessionId:BETA,ownerGeneration:"1"},
      selected_project_index:0,selected_session_index:1,
      project_rows:[{project_id:PROJECT,label:"Test project",path:"C:/task/日本語 workspace"}],
      session_rows:[
        {session_id:ALPHA,title:"alpha",label:"alpha [000002]",archived:false},
        {session_id:BETA,title:"beta",label:"beta [000003]",archived:false},
      ],
      chat_session_rows:[{session_id:QUICK,title:"quick",label:"quick [000004]",archived:false}],
      transcript_rows:[{row_kind:"user",stable_history_identity:"user-b",body:"beta prompt"}],
    },
    rows:[ALPHA,BETA].map((id,index)=>({action:"session",focus_key:`session:${id}:select`,
      title:index===0?"alpha [000002]":"beta [000003]",visible:true,enabled:true})),
  };
}

test("management locators bind stable row identity and the current public action",()=>{
  assert.deepEqual(managementRowLocator("session",ALPHA,"archive-session"),{
    selector:`button[data-action="archive-session"][data-focus-key="session:${ALPHA}:archive-session"]`,
    identity:{tag:"BUTTON",action:"archive-session",focusKey:`session:${ALPHA}:archive-session`},
  });
  assert.equal(managementRowLocator("project",PROJECT,"delete-project").identity.focusKey,`project:${PROJECT}:delete`);
  assert.equal(managementRowLocator("project",PROJECT,"new-project-session").identity.focusKey,`project:${PROJECT}:new-session`);
  assert.equal(managementRowLocator("chat-session",QUICK,"delete-chat-session").identity.focusKey,`chat-session:${QUICK}:delete-chat-session`);
  for(const id of ["",`${ALPHA}\"]`,"other"]) assert.throws(()=>managementRowLocator("session",id));
  assert.throws(()=>managementRowLocator("other",ALPHA));
});

test("keyboard operations require the exact acquired search or confirmation focus owner",()=>{
  const search={tag:"INPUT",id:"session-search"};
  assert.equal(managementFocusedTarget({active:{...search,action:null}},search),true);
  for(const active of [null,{tag:"INPUT",id:"local-search"},{tag:"TEXTAREA",id:"session-search"}]) {
    assert.equal(managementFocusedTarget({active},search),false);
  }
  const cancel={tag:"BUTTON",action:"cancel-local-confirm"};
  assert.equal(managementFocusedTarget({active:cancel},cancel),true);
  assert.equal(managementFocusedTarget({active:{tag:"BUTTON",action:"confirm-local-delete"}},cancel),false);
});

test("management command captures current index and owner before confirmation",()=>{
  const p=surface().projection;
  assert.deepEqual(managementRowCommand(p,"session",ALPHA,"archive_session"),{
    command:"archive_session",args:{index:0,expectedTarget:{workspacePath:p.workspace_path,ownerProjectId:PROJECT,ownerSessionId:BETA,rowId:ALPHA}},
  });
  p.session_rows.reverse();p.selected_session_index=0;
  assert.equal(managementRowCommand(p,"session",ALPHA,"delete_session").args.index,1);
  p.session_rows.push({...p.session_rows[1]});
  assert.throws(()=>managementRowCommand(p,"session",ALPHA,"delete_session"),/one current row/);
  assert.throws(()=>managementRowCommand(p,"session",QUICK,"delete_session"),/one current row/);
});

test("filtered rows require exact distinct Rust and DOM owners, labels and archive state",()=>{
  const ready=surface();
  assert.equal(managementRowsMatch(ready,"session",[BETA,ALPHA],{[ALPHA]:false}),true);
  for(const change of [
    value=>value.projection.session_rows.push({...value.projection.session_rows[0]}),
    value=>value.projection.session_rows.splice(0,1),
    value=>value.rows.push({...value.rows[0]}),
    value=>value.rows[0].focus_key=`session:${QUICK}:select`,
    value=>value.rows[0].title="wrong label",
    value=>value.rows[0].visible=false,
    value=>value.rows[0].enabled=false,
    value=>value.projection.session_rows[0].archived=true,
  ]) {
    const invalid=structuredClone(ready);change(invalid);
    assert.equal(managementRowsMatch(invalid,"session",[ALPHA,BETA],{[ALPHA]:false}),false);
  }
  assert.equal(managementRowsMatch({...ready,projection:{...ready.projection,session_rows:[]},rows:[]},"session",[]),true);
});

test("confirmation requires the actual named target, consequence, focus and enabled exact decisions",()=>{
  const expected={title:"alpha [000002]",detail:ALPHA,verb:"アーカイブ",confirmationAction:"confirm-local-archive-state"};
  const ready={errors:0,dialog_count:1,confirmation:{visible:true,aria_modal:"true",focus_inside:true,
    title:"チャットをアーカイブしますか？",summary:expected.title,target:ALPHA,
    consequence:"履歴、実行証跡、ワークスペース内の実ファイルは削除しません。",
    actions:["cancel-local-confirm","confirm-local-archive-state"].map(action=>({action,enabled:true,visible:true})),
  }};
  assert.equal(managementConfirmationReady(ready,expected),true);
  for(const change of [
    value=>value.errors=1,
    value=>value.dialog_count=2,
    value=>value.confirmation.target=BETA,
    value=>value.confirmation.summary="beta",
    value=>value.confirmation.title="チャットを削除しますか？",
    value=>value.confirmation.consequence="履歴を削除します。",
    value=>value.confirmation.focus_inside=false,
    value=>value.confirmation.aria_modal="false",
    value=>value.confirmation.actions[1].enabled=false,
    value=>value.confirmation.actions[1].visible=false,
    value=>value.confirmation.actions[1].action="confirm-local-delete",
  ]) {
    const invalid=structuredClone(ready);change(invalid);
    assert.equal(managementConfirmationReady(invalid,expected),false);
  }
  const restored=structuredClone(ready);
  restored.confirmation.title="チャットを復元しますか？";
  restored.confirmation.consequence="履歴、実行証跡、ワークスペース内の実ファイルは変更しません。";
  assert.equal(managementConfirmationReady(restored,{...expected,verb:"復元"}),true);
});

test("search distinguishes matching results from the one retained open session",()=>{
  const before=managementSnapshot(surface());
  const retained=surface();
  retained.search="no-match";retained.projection.session_search_text="no-match";
  retained.projection.session_rows=retained.projection.session_rows.filter(row=>row.session_id===BETA);
  retained.rows=retained.rows.filter(row=>row.focus_key===`session:${BETA}:select`);
  const expected={query:"no-match",matchedIds:[],before};
  assert.equal(managementSearchReady(retained,expected),true);
  for(const change of [
    value=>value.projection.session_rows.push(surface().projection.session_rows[0]),
    value=>value.projection.draft_target.sessionId=ALPHA,
    value=>value.projection.transcript_rows[0].body="another session",
    value=>value.prompt="lost",
    value=>value.projection.session_search_text="old query",
    value=>value.search="old query",
  ]) {
    const invalid=structuredClone(retained);change(invalid);
    assert.equal(managementSearchReady(invalid,expected),false);
  }
  const matched=surface();matched.search="alpha";matched.projection.session_search_text="alpha";
  assert.equal(managementSearchReady(matched,{query:"alpha",matchedIds:[ALPHA],before}),true);
});

test("cancel snapshot detects unsent draft, history, project and other session changes",()=>{
  const ready=surface(),baseline=managementSnapshot(ready);
  for(const change of [
    value=>value.prompt="lost",
    value=>value.projection.draft_target.sessionId=ALPHA,
    value=>value.projection.transcript_rows[0].body="changed",
    value=>value.projection.project_rows=[],
    value=>value.projection.session_rows[1].archived=true,
    value=>value.projection.chat_session_rows=[],
  ]) {
    const invalid=structuredClone(ready);change(invalid);
    assert.notDeepEqual(managementSnapshot(invalid),baseline);
  }
  const later=structuredClone(ready);later.projection.projection_revision="100";
  assert.deepEqual(managementSnapshot(later),baseline,"ordinary observation revision is not a mutation");
});

test("management scenario keeps visual review pending and uses common lifecycle",()=>{
  const scenario=createSessionManagementScenario();
  assert.equal(scenario.id,"navigation.session-management");
  assert.equal(scenario.manualGate,"pending");
  assert.equal(scenario.databaseRequired,true);
  for(const name of ["prepare","execute","requestGracefulExit","quiesce","cleanup"]) assert.equal(typeof scenario[name],"function");
});
