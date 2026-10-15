import assert from 'node:assert/strict';
import test from 'node:test';
import {createExternalRejoinScenario,createExternalSidebarStopScenario,externalNavigationProviderOptions,externalNavigationHeldLedger,externalNavigationMainPreserved,externalNavigationActiveReady,externalSessionId,externalNavigationRowStopped} from '../scenarios/external_navigation_controls.mjs';
const ROOT='01ARZ3NDEKTSV4RRFFQ69G5FAV',EXTERNAL='01ARZ3NDEKTSV4RRFFQ69G5FAX';
const main={workspace:'C:\\owned',sessionId:ROOT,history:[{kind:'user',identity:'user:1',body:'original'}]};
function ledger(){return ['completed','held'].map(response_phase=>({route:'responses',contract:{pass:true},response_phase}));}
function surface(){return {errors:0,composerRunTarget:{sessionId:ROOT},prompt:'Keep this unsent draft in the original Main conversation',promptEnabled:true,p:{workspace_path:main.workspace,run_target:{sessionId:ROOT},draft_target:{sessionId:ROOT},run_status_key:'completed',busy:false,overlay:'none',navigation_admission_open:true,transcript_rows:[{row_kind:'user',stable_history_identity:'user:1',body:'original'}],session_rows:[{session_id:EXTERNAL,loaded_status:'active',active_turn_id:'turn',interrupt_target:{sessionId:EXTERNAL}}]},actions:['rejoin-session','interrupt-session'].map(action=>({action,index:0,disabled:false,aria:'false'}))};}
test('external root scenarios retain one lifecycle and exactly two finite ordinary model responses',()=>{
  assert.equal(createExternalRejoinScenario().id,'navigation.external-rejoin');assert.equal(createExternalSidebarStopScenario().id,'navigation.external-sidebar-stop');
  assert.equal(createExternalRejoinScenario().manualGate,'pending');assert.throws(()=>createExternalRejoinScenario({injectSession:true}));
  const options=externalNavigationProviderOptions();assert.equal(options.responseBehavior,'hold_until_release');assert.equal(options.turns.length,2);assert.notEqual(options.turns[0].prompt,options.turns[1].prompt);
});
test('CLI identity is read only from one complete session_started JSON event',()=>{
  const line=JSON.stringify({kind:'session_started',session_id:EXTERNAL,title:'external'});assert.equal(externalSessionId(line),null);assert.equal(externalSessionId(line+'\n'),EXTERNAL);
  assert.equal(externalSessionId(JSON.stringify({kind:'turn_started',session_id:EXTERNAL})+'\n'),null);assert.equal(externalSessionId('{"kind":"session_started","session_id":"wrong"}\n'),null);
  assert.throws(()=>externalSessionId(line+'\n'+line+'\n'),/multiple/);assert.throws(()=>externalSessionId('non-json\n'));
});
test('Main conservation rejects draft, owner, canonical or error changes',()=>{
  assert.equal(externalNavigationMainPreserved(surface(),main),true);
  for(const mutate of [v=>v.prompt='lost',v=>v.composerRunTarget.sessionId=EXTERNAL,v=>v.p.run_target.sessionId=EXTERNAL,v=>v.p.draft_target.sessionId=EXTERNAL,v=>v.p.transcript_rows[0].body='changed',v=>v.p.transcript_rows[0].stable_history_identity='other',v=>v.errors=1,v=>v.p.busy=true,v=>v.promptEnabled=false]){const v=surface();mutate(v);assert.equal(externalNavigationMainPreserved(v,main),false);}
});
test('positive navigation requires exact active ordinary root and both enabled row actions while provider remains held',()=>{
  const sample=()=>({surface:surface(),sessionId:EXTERNAL,ledger:ledger()});assert.equal(externalNavigationActiveReady(sample(),main),true);
  for(const mutate of [v=>v.sessionId=ROOT,v=>v.surface.p.navigation_admission_open=false,v=>v.surface.p.session_rows[0].loaded_status='idle',v=>v.surface.p.session_rows[0].active_turn_id=null,v=>v.surface.p.session_rows[0].interrupt_target=null,v=>v.surface.actions[0].disabled=true,v=>v.surface.actions[0].aria='true',v=>v.surface.actions[0].index=1,v=>v.surface.actions.push({...v.surface.actions[0]}),v=>v.ledger[1].response_phase='completed',v=>v.ledger[1].contract.pass=false]){const v=sample();mutate(v);assert.equal(externalNavigationActiveReady(v,main),false);}
  assert.equal(externalNavigationHeldLedger([...ledger(),...ledger()],2),false);
});

test('stopped external row requires canonical terminal and the same DOM row with no activity or stale Stop action',()=>{
  const value=()=>({p:{session_rows:[{session_id:EXTERNAL,status:'cancelled',loaded_status:'idle',active_turn_id:null,interrupt_target:null}],pending_async_operations:[],async_polling_required:false},sidebarRows:[{focusKey:`session:${EXTERNAL}:select`,activity:null,stop:0,rejoin:0}]});
  assert.equal(externalNavigationRowStopped(value(),EXTERNAL),true);
  for(const mutate of [v=>v.p.session_rows[0].session_id=ROOT,v=>v.p.session_rows[0].status='running',v=>v.p.session_rows[0].active_turn_id='old',v=>v.p.session_rows[0].interrupt_target={},v=>v.p.pending_async_operations=['session_maintenance'],v=>v.p.async_polling_required=true,v=>v.sidebarRows[0].activity='running',v=>v.sidebarRows[0].stop=1,v=>v.sidebarRows[0].rejoin=1,v=>v.sidebarRows[0].focusKey=`session:${ROOT}:select`,v=>v.sidebarRows=[]]){const v=value();mutate(v);assert.equal(externalNavigationRowStopped(v,EXTERNAL),false);}
});
