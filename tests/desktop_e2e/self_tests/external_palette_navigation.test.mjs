import assert from 'node:assert/strict';
import test from 'node:test';
import {createExternalPaletteRejoinScenario,externalNavigationProviderOptions,externalPaletteActiveReady,externalPaletteTerminalPreserved,externalSessionRowArguments} from '../scenarios/external_navigation_controls.mjs';
const ROOT='01ARZ3NDEKTSV4RRFFQ69G5FAV',OTHER='01ARZ3NDEKTSV4RRFFQ69G5FAX';
const MAIN={workspace:'C:/owned',sessionId:ROOT,commitGeneration:2,history:[{kind:'user',identity:'u1',body:'first'},{kind:'assistant',identity:'a1',body:'answer'}]};
const ledger=()=>['completed','held'].map(response_phase=>({route:'responses',contract:{pass:true},response_phase}));
const surface=()=>({errors:0,composerRunTarget:{sessionId:ROOT},prompt:'Keep this unsent draft in the original Main conversation',promptEnabled:true,p:{workspace_path:MAIN.workspace,run_target:{sessionId:ROOT},draft_target:{sessionId:ROOT},run_status_key:'completed',busy:false,overlay:'none',navigation_admission_open:true,composer_commit_generation:2,post_run_refresh_pending:false,transcript_rows:MAIN.history.map(row=>({row_kind:row.kind,stable_history_identity:row.identity,body:row.body})),selected_session_index:0,session_rows:[{session_id:ROOT,loaded_status:'active',active_turn_id:'turn2',interrupt_target:{sessionId:ROOT}}]}});
test('palette Rejoin uses existing owner and exact finite continued conversation',()=>{
 assert.equal(createExternalPaletteRejoinScenario().id,'navigation.external-palette-rejoin');assert.equal(createExternalPaletteRejoinScenario().manualGate,'pending');
 const opts=externalNavigationProviderOptions(true);assert.equal(opts.orderedConversation,true);assert.equal(opts.turns.length,2);assert.equal(opts.responseBehavior,'hold_until_release');assert.equal(externalNavigationProviderOptions().orderedConversation,undefined);
});
test('positive palette condition requires same selected session admission and preserved idle Main',()=>{
 const sample=()=>({surface:surface(),sessionId:ROOT,ledger:ledger()});assert.equal(externalPaletteActiveReady(sample(),MAIN),true);const noStop=sample();delete noStop.surface.p.session_rows[0].interrupt_target;assert.equal(externalPaletteActiveReady(noStop,MAIN),true);
 for(const mutate of [v=>v.sessionId=OTHER,v=>v.surface.p.selected_session_index=1,v=>v.surface.p.session_rows[0].session_id=OTHER,v=>v.surface.p.busy=true,v=>v.surface.prompt='lost',v=>v.surface.p.navigation_admission_open=false,v=>v.surface.p.session_rows[0].active_turn_id=null,v=>v.ledger[1].response_phase='completed',v=>v.ledger[1].contract.pass=false]){const v=sample();mutate(v);assert.equal(externalPaletteActiveReady(v,MAIN),false);}
});
test('continued session stop preserves every first-turn row and unsent draft with exact cancelled row',()=>{
 const value=()=>{const v=surface();Object.assign(v.p,{run_status_key:'cancelled',pending_async_operations:[],async_polling_required:false});Object.assign(v.p.session_rows[0],{status:'cancelled',loaded_status:'idle',active_turn_id:null,interrupt_target:null});v.p.transcript_rows.push({row_kind:'user',body:'Keep this external root active for sidebar navigation'});v.sidebarRows=[{focusKey:'session:'+ROOT+':select',activity:null,stop:0,rejoin:0}];return v;};
 assert.equal(externalPaletteTerminalPreserved(value(),MAIN),true);
 for(const mutate of [v=>v.p.transcript_rows[0].body='changed',v=>v.p.transcript_rows[1].stable_history_identity='wrong',v=>v.p.transcript_rows.reverse(),v=>v.p.transcript_rows.pop(),v=>v.p.transcript_rows.push({...v.p.transcript_rows.at(-1)}),v=>v.p.composer_commit_generation=3,v=>v.p.run_target.sessionId=OTHER,v=>v.p.draft_target.sessionId=OTHER,v=>v.p.run_status_key='running',v=>v.p.post_run_refresh_pending=true,v=>v.p.pending_async_operations=['stop'],v=>v.sidebarRows[0].activity='running',v=>v.sidebarRows[0].stop=1,v=>v.prompt='lost']){const v=value();mutate(v);assert.equal(externalPaletteTerminalPreserved(v,MAIN),false);}
});

test('palette command identity comes from one typed selected row, independently of sidebar DOM presence',()=>{
 const v=surface();v.p.project_rows=[{project_id:'project'}];v.p.selected_project_index=0;v.actions=[];
 assert.deepEqual(externalSessionRowArguments(v,ROOT).args,{index:0,expectedTarget:{workspacePath:MAIN.workspace,ownerProjectId:'project',ownerSessionId:ROOT,rowId:ROOT}});
 assert.throws(()=>externalSessionRowArguments(v,OTHER),/Exact typed/);v.p.session_rows.push({...v.p.session_rows[0]});assert.throws(()=>externalSessionRowArguments(v,ROOT),/Exact typed/);
});
