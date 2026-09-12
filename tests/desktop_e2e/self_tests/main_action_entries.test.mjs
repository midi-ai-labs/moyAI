import test from 'node:test';
import assert from 'node:assert/strict';
import {SHORTCUT_ENTRY_STEPS,shortcutResultFailures,activateMainActionEntry,entryDomFailures} from '../scenarios/main_action_entries.mjs';
const ledger=[{method:'POST',contract:{pass:true},response_phase:'completed'}];
const base=()=>({p:{busy:false,post_run_refresh_pending:false,background_mutation_pending:false,pending_async_operations:[],
  run_target:{sessionId:'s',expectedState:{kind:'idle'}},draft_target:{sessionId:'s'},transcript_rows:[{row_kind:'assistant',body:'ONE_OK'}],
  overlay:'shortcuts',session_search_include_archived:false,access_label:'default',selected_session_index:0,
  selected_session_title:'Completed fixture',status_message:'ready'},prompt:{value:''},visibleErrors:[],
  composerRunTarget:{sessionId:'s',expectedState:{kind:'idle'}},status:{text:'ready'},
  entryShell:{title:'Completed fixture',access:[{label:'承認を求める',disabled:false}],archive:[{selected:false,disabled:false}]}});
test('Shortcuts explicit row plan includes archive both values and all three access modes',()=>{
  assert.deepEqual(SHORTCUT_ENTRY_STEPS.filter(s=>s.archived!==undefined).map(s=>s.archived),[true,false]);
  assert.deepEqual(SHORTCUT_ENTRY_STEPS.filter(s=>s.access!==undefined).map(s=>s.access),['auto_review','full_access','default']);
  assert.equal(SHORTCUT_ENTRY_STEPS[0].action,'show-command-palette');
});
test('Shortcut row oracle preserves the same conversation, canonical history, and model request ledger',()=>{
  const step={action:'toggle-session-archived-search',archived:true};
  const fixture=()=>{const value=base();value.p.session_search_include_archived=true;value.entryShell.archive[0].selected=true;return value;};
  assert.deepEqual(shortcutResultFailures(fixture(),base(),step,ledger,structuredClone(ledger)),[]);
  for(const change of [v=>{v.p.busy=true;},v=>{v.p.pending_async_operations=['state'];},v=>{v.p.run_target.sessionId='other';},
    v=>{v.p.transcript_rows[0].body='other';},v=>{v.prompt.value='lost draft';},v=>{v.p.session_search_include_archived=false;},v=>{v.visibleErrors=['error'];}]){
    const value=fixture();change(value);assert.notEqual(shortcutResultFailures(value,base(),step,ledger,ledger).length,0);
  }
  assert.ok(shortcutResultFailures(fixture(),base(),step,ledger,[...ledger,...ledger]).includes('provider'));
});
test('Access and destination success require their exact intended values',()=>{
  assert.ok(shortcutResultFailures(base(),base(),{access:'auto_review'},ledger,ledger).includes('access'));
  assert.ok(shortcutResultFailures(base(),base(),{overlay:'command_palette'},ledger,ledger).includes('overlay'));
});

test('completed backend mutation cannot qualify a previous visible access label or saving state',()=>{
  const ready=base();ready.entryShell.access[0].label='代理で承認';
  assert.deepEqual(entryDomFailures(ready,{access:'auto_review'}),[]);
  for(const change of [v=>v.entryShell.access[0].label='承認を求める',v=>v.entryShell.access[0].disabled=true,
    v=>v.entryShell.access.push({...v.entryShell.access[0]}),v=>v.status.text='saving access mode',v=>v.entryShell.title='old']){
    const value=structuredClone(ready);change(value);assert.notEqual(entryDomFailures(value,{access:'auto_review'}).length,0);
  }
});

test('archive row requires both values to reach its actual sidebar selection',()=>{
  const value=base();assert.deepEqual(entryDomFailures(value,{archived:false}),[]);
  assert.ok(entryDomFailures(value,{archived:true}).includes('archive-dom'));
  value.entryShell.archive[0].selected=true;assert.deepEqual(entryDomFailures(value,{archived:true}),[]);
});

test('Quick Chat must expose its independent workspace and matching composer DOM owner',()=>{
  const value=base();value.p.selected_session_index=-1;value.p.workspace_path='quick-chat-workspace';value.p.run_target.sessionId=null;
  value.composerRunTarget=structuredClone(value.p.run_target);value.entryShell.title='新しいチャット';
  value.entryShell.project=[{label:'プロジェクトなし',path:value.p.workspace_path}];value.entryShell.emptyHeading='何に取り組みますか？';
  assert.deepEqual(entryDomFailures(value,{quick:true}),[]);
  for(const change of [v=>v.composerRunTarget.sessionId='old',v=>v.entryShell.project[0].path='old-workspace',
    v=>v.entryShell.project[0].label='workspace',v=>v.entryShell.emptyHeading=null]){
    const changed=structuredClone(value);change(changed);assert.notEqual(entryDomFailures(changed,{quick:true}).length,0);
  }
});

function fixture(overlay){
  const log=[],state={overlay,query:'send',selected:false};
  const d={
    cdp:{evaluate:async expr=>expr.includes("invoke('desktop_state')")?{p:{overlay:state.overlay,local_search_text:state.query},prompt:{value:'raw'},visibleErrors:[]}
      :expr.includes('return {search:')?{search:state.query,count:state.query==='cancel-run'?1:0}:1},
    input:{keyDown:async key=>log.push(['down',key]),keyUp:async key=>log.push(['up',key]),pressKey:async key=>{
      log.push(['press',key]);if(key==='a')state.selected=true;if(key==='Backspace'&&state.selected){state.query='';state.selected=false;}},
      insertText:async(_target,text)=>{state.query+=text;log.push(['text',text]);},snapshotProbe:async()=>({sequence:0})},
    commands:{snapshot:async()=>({sequence:0})},sink:{record:async()=>{}},owner:'test',
    trustedClick:async(_input,_cdp,target)=>log.push(['click',target.selector]),assertTrustedTextInsertion:()=>({pass:true}),
    wait:async(label,sample,accept)=>{const value=await sample();assert.equal(accept(value),true,label);return value;},
  };
  return {d,log,state};
}
test('already-open Shortcuts dialog is reused without attempting a blocked background opener',async()=>{
  const {d,log}=fixture('shortcuts');await activateMainActionEntry(d,'send','shortcuts');
  assert.equal(log.filter(row=>row[0]==='click').length,1);
  assert.ok(log[0][1].includes('shortcuts-dialog-title'));
});
test('existing Palette query is replaced through trusted keyboard input before choosing Stop',async()=>{
  const {d,log,state}=fixture('command_palette');await activateMainActionEntry(d,'cancel-run','palette');
  assert.equal(state.query,'cancel-run');
  assert.ok(log.some(row=>row[0]==='press'&&row[1]==='a'));
  assert.ok(log.some(row=>row[0]==='press'&&row[1]==='Backspace'));
  assert.ok(log.at(-1)[1].includes('palette-action:cancel-run'));
});
