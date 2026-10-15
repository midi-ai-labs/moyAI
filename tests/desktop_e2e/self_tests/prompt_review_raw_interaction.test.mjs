import test from 'node:test';
import assert from 'node:assert/strict';
import { rawPointerFailures, rawCopyFailures, rawMouse, assertRawDragProbe } from '../scenarios/prompt_review_raw_interaction.mjs';
import { createPromptReviewRawInteractionScenario, createPromptReviewSubmitScenario } from '../scenarios/prompt_review_submit.mjs';

const text='review original request marker';
const pointer=()=>['pointerdown','pointerup','click'].map(type=>({type,isTrusted:true,targetIsRaw:true,targetTag:'PRE',button:0,buttons:type==='pointerdown'?1:0}));
const copied=()=>({selected:text,selectionInsideRaw:true,events:[
  {type:'keydown',key:'c',ctrlKey:true,isTrusted:true,defaultPrevented:false,selected:text,selectionInsideRaw:true},
  {type:'copy',isTrusted:true,defaultPrevented:false,selected:text,selectionInsideRaw:true},
]});

test('raw text drag carries the held left button through every mouse move',async()=>{
  const calls=[],cdp={async call(method,params){calls.push({method,params});}};
  await rawMouse(cdp,{x:10,y:20},{x:210,y:20});
  assert.equal(calls.filter(c=>c.params.type==='mousePressed').length,1);
  assert.equal(calls.filter(c=>c.params.type==='mouseReleased').length,1);
  const drag=calls.filter(c=>c.params.type==='mouseMoved'&&c.params.buttons===1);
  assert.equal(drag.length,8);
  assert.ok(drag.every(c=>c.params.button==='left'));
  assert.deepEqual(calls.at(-1).params,{type:'mouseReleased',x:210,y:20,button:'left',buttons:0,modifiers:0,clickCount:1});
});

test('drag proof accounts for initial hover plus all eight held moves without accepting missing or untrusted movement',()=>{
  const events=[{type:'pointermove',buttons:0},
    {type:'pointerdown',button:0,buttons:1},
    ...Array.from({length:8},()=>({type:'pointermove',buttons:1})),
    {type:'pointerup',button:0,buttons:0}].map((event,i)=>({sequence:i+1,isTrusted:true,...event}));
  const snapshot={found:true,sequence:events.length,dropped_through:0,events};
  assert.equal(assertRawDragProbe(snapshot,0).events.length,11);
  assert.throws(()=>assertRawDragProbe({...snapshot,events:events.filter((_,i)=>i!==4)},0));
  const falseMove=structuredClone(snapshot);falseMove.events[4].isTrusted=false;
  assert.throws(()=>assertRawDragProbe(falseMove,0));
});

test('raw click rejects pointer capture retargeting mouseup and click to the backdrop',()=>{
  assert.deepEqual(rawPointerFailures(pointer()),[]);
  const captured=pointer();for(const event of captured.slice(1))Object.assign(event,{targetIsRaw:false,targetTag:'DIV'});
  assert.deepEqual(rawPointerFailures(captured),['raw-pointerup','raw-click']);
  const synthetic=pointer();synthetic[0].isTrusted=false;
  assert.ok(rawPointerFailures(synthetic).includes('raw-pointerdown'));
});

test('raw drag requires acquired trusted down/up inside PRE and the correct button state',()=>{
  assert.deepEqual(rawPointerFailures(pointer().slice(0,2),{drag:true}),[]);
  assert.ok(rawPointerFailures([],{drag:true}).length>0);
  const broken=pointer();broken[1].buttons=1;
  assert.ok(rawPointerFailures(broken,{drag:true}).includes('raw-pointerup'));
});

test('Copy proof rejects browser-default suppression and copy-event absence independently',()=>{
  assert.deepEqual(rawCopyFailures(copied(),text),[]);
  const keyBlocked=copied();keyBlocked.events[0].defaultPrevented=true;keyBlocked.events.pop();
  assert.deepEqual(rawCopyFailures(keyBlocked,text),['native-copy-default','native-copy-event']);
  const copyBlocked=copied();copyBlocked.events[1].defaultPrevented=true;
  assert.deepEqual(rawCopyFailures(copyBlocked,text),['native-copy-event']);
  const duplicate=copied();duplicate.events.push({...duplicate.events[1]});
  assert.deepEqual(rawCopyFailures(duplicate,text),['native-copy-event']);
});

test('raw selection and Copy must refer to the exact text wholly within PRE',()=>{
  for(const change of [value=>value.selected='other',value=>value.selectionInsideRaw=false,
    value=>value.events[0].selectionInsideRaw=false,value=>value.events[1].isTrusted=false,
    value=>value.events[1].selected='other']){
    const value=copied();change(value);assert.notDeepEqual(rawCopyFailures(value,text),[]);
  }
});

test('bounded raw interaction reuses lifecycle with an automatic contract gate and leaves old visual gates intact',()=>{
  const raw=createPromptReviewRawInteractionScenario();
  assert.equal(raw.id,'prompt-review.raw-interaction');assert.equal(raw.manualGate,'not_required');assert.equal(raw.databaseRequired,true);
  for(const name of ['prepare','execute','requestGracefulExit','quiesce','cleanup'])assert.equal(typeof raw[name],'function');
  assert.equal(createPromptReviewSubmitScenario({choice:'raw'}).manualGate,'pending');
  assert.equal(createPromptReviewSubmitScenario({choice:'enhanced',enhanceEntries:'menu-palette'}).manualGate,'pending');
  assert.throws(()=>createPromptReviewSubmitScenario({choice:'enhanced',rawInteractionOnly:true}),TypeError);
});
