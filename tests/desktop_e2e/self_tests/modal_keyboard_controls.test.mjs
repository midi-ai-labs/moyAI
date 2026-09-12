import assert from 'node:assert/strict';
import test from 'node:test';
import { MODAL_KEYBOARD_PLAN, MODAL_BLOCKED_KEYS, createModalKeyboardControlsScenario, modalFocusStep, modalContentPreserved, modalDisclosureExposesTarget } from '../scenarios/modal_keyboard_controls.mjs';

const surface=()=>({prompt:'draft',p:{workspace_path:'fixture',draft_target:{sessionId:null,ownerGeneration:'1'},transcript_rows:[{row_kind:'empty_placeholder'}],run_target:{sessionId:null},access_label:'Default',access_target:{expectedAccessMode:'default'},session_settings:{access_mode:'default'},session_search_include_archived:false,busy:false,navigation_loading:false},errors:[]});
test('modal plan separates actual dialogs and declared Close buttons',()=>{
  assert.deepEqual(MODAL_KEYBOARD_PLAN.map(p=>p.overlay),['workspace','command_palette','shortcuts','about','hub','provider','config']);
  assert.deepEqual(MODAL_KEYBOARD_PLAN.map(p=>p.closes.length),[0,0,1,2,2,1,1]);
  assert.equal(new Set(MODAL_KEYBOARD_PLAN.map(p=>p.heading)).size,7);
  assert.deepEqual(MODAL_BLOCKED_KEYS.map(k=>`${k.modifier??''}:${k.key}`),['Control:n','Control:k',':F8','Control:Enter',':F9','Control:i']);
  const scenario=createModalKeyboardControlsScenario();
  assert.equal(scenario.id,'navigation.modal-keyboard-controls');assert.equal(scenario.manualGate,'pending');
  for(const name of ['prepare','requestGracefulExit','quiesce','cleanup','execute'])assert.equal(typeof scenario[name],'function');
});
test('focus oracle verifies both wraps and rejects off-dialog or stalled movement',()=>{
  const first={inDialog:true,focusIndex:0,targets:[{id:'a'},{id:'b'},{id:'c'}]};
  const middle={...first,focusIndex:1},last={...first,focusIndex:2};
  assert.equal(modalFocusStep(first,middle),true);assert.equal(modalFocusStep(last,first),true);
  assert.equal(modalFocusStep(first,last,true),true);assert.equal(modalFocusStep(middle,first,true),true);
  assert.equal(modalFocusStep(first,first),false);assert.equal(modalFocusStep(first,{...middle,inDialog:false}),false);
  assert.equal(modalFocusStep(first,{...middle,targets:[{id:'a'},{id:'d'},{id:'c'}]}),false);
});
test('modal observation requires draft, owner, transcript and guarded settings to remain unchanged',()=>{
  const before=surface();assert.equal(modalContentPreserved(before,structuredClone(before)),true);
  for(const patch of [{prompt:'changed'},{p:{...before.p,workspace_path:'other'}},{p:{...before.p,draft_target:{sessionId:'new'}}},
    {p:{...before.p,transcript_rows:[]}},{p:{...before.p,session_settings:{access_mode:'full_access'}}},{p:{...before.p,session_search_include_archived:true}},
    {p:{...before.p,busy:true}},{p:{...before.p,navigation_loading:true}},{errors:['error']}])assert.equal(modalContentPreserved(before,{...before,...patch}),false);
});

test('closed disclosure omits its body controls while retaining only its exposed summary',()=>{
  const node=(tagName,parentElement=null,open=false)=>{const value={tagName,parentElement,open,children:[],contains(other){for(let current=other;current;current=current.parentElement)if(current===this)return true;return false;}};parentElement?.children.push(value);return value;};
  const outer=node('DETAILS'),summary=node('SUMMARY',outer),summaryText=node('SPAN',summary),input=node('INPUT',outer);
  assert.equal(modalDisclosureExposesTarget(summary),true);
  assert.equal(modalDisclosureExposesTarget(summaryText),true);
  assert.equal(modalDisclosureExposesTarget(input),false,'retained layout boxes do not make closed body controls tabbable');
  const inner=node('DETAILS',outer),innerSummary=node('SUMMARY',inner);
  assert.equal(modalDisclosureExposesTarget(innerSummary),false,'nested summary remains behind the closed outer disclosure');
  outer.open=true;
  assert.equal(modalDisclosureExposesTarget(input),true);
  assert.equal(modalDisclosureExposesTarget(innerSummary),true);
});
