import { DesktopE2eError } from '../core/execution.mjs';
import { assertTrustedProbeSequence } from '../drivers/webview_input.mjs';

const REVIEW = '[role="dialog"][aria-labelledby="prompt-review-dialog-title"]';
const RAW = `${REVIEW} .review-grid > pre`;
const PROBE = 'moyai.desktop_e2e.prompt_review.raw_pointer.v1';
const error = (owner, code, message, evidence) => new DesktopE2eError(owner, `review-raw-${code}`, message, evidence);

// These observers never focus, select, edit or dispatch an application action.
// Range is used only to read the first/last glyph geometry for real mouse input.
function geometryExpression(text) {
  return `(() => {
    const nodes=[...document.querySelectorAll(${JSON.stringify(RAW)})];
    const raw=nodes.length===1?nodes[0]:null, node=raw?.firstChild;
    if(!raw||raw.textContent!==${JSON.stringify(text)}||raw.childNodes.length!==1||node?.nodeType!==Node.TEXT_NODE||node.length<2) return {valid:false,reason:'raw-text-shape'};
    const glyph=(index)=>{const r=document.createRange();r.setStart(node,index);r.setEnd(node,index+1);return r.getBoundingClientRect();};
    const first=glyph(0),last=glyph(node.length-1),rect=raw.getBoundingClientRect(),style=getComputedStyle(raw);
    const start={x:first.left+first.width*0.15,y:first.top+first.height/2};
    const end={x:last.right-last.width*0.15,y:last.top+last.height/2};
    const hit=p=>document.elementFromPoint(p.x,p.y)===raw;
    const pathClear=Array.from({length:9},(_,i)=>({x:start.x+(end.x-start.x)*i/8,y:start.y+(end.y-start.y)*i/8})).every(hit);
    const visible=style.display!=='none'&&style.visibility!=='hidden'&&style.userSelect!=='none'&&!raw.closest('[inert]');
    return {valid:visible&&first.width>0&&last.width>0&&Math.abs(start.y-end.y)<1&&start.x<end.x&&pathClear
      &&start.x>=0&&end.x<innerWidth&&start.y>=0&&end.y<innerHeight,
      start,end,text:raw.textContent,raw_rect:{left:rect.left,top:rect.top,right:rect.right,bottom:rect.bottom},viewport:{width:innerWidth,height:innerHeight}};
  })()`;
}

function installExpression() {
  return `(() => {
    const key=Symbol.for(${JSON.stringify(PROBE)});
    if(globalThis[key])return {installed:false};
    const controller=new AbortController(),state={controller,events:[]};
    const record=event=>{
      const raw=document.querySelector(${JSON.stringify(RAW)}),selection=window.getSelection();
      state.events.push({type:event.type,isTrusted:event.isTrusted,defaultPrevented:event.defaultPrevented,
        targetTag:event.target instanceof Element?event.target.tagName:null,
        targetIsRaw:event.target===raw,selected:selection?.toString()??'',
        selectionInsideRaw:!!raw&&!!selection?.anchorNode&&!!selection?.focusNode&&raw.contains(selection.anchorNode)&&raw.contains(selection.focusNode),
        key:event instanceof KeyboardEvent?event.key:null,ctrlKey:event instanceof KeyboardEvent?event.ctrlKey:null,
        button:event instanceof MouseEvent?event.button:null,buttons:event instanceof MouseEvent?event.buttons:null,
        clientX:event instanceof MouseEvent?event.clientX:null,clientY:event instanceof MouseEvent?event.clientY:null});
      if(state.events.length>100)state.events.shift();
    };
    for(const type of ['pointerdown','pointermove','pointerup','click','keydown','copy'])window.addEventListener(type,record,{signal:controller.signal});
    globalThis[key]=state;return {installed:true};
  })()`;
}

const observationExpression = `(() => {
  const raw=document.querySelector(${JSON.stringify(RAW)}),selection=window.getSelection();
  return {events:globalThis[Symbol.for(${JSON.stringify(PROBE)})]?.events??[],selected:selection?.toString()??'',
    selectionInsideRaw:!!raw&&!!selection?.anchorNode&&!!selection?.focusNode&&raw.contains(selection.anchorNode)&&raw.contains(selection.focusNode)};
})()`;

export function rawPointerFailures(events, { drag = false } = {}) {
  if (!Array.isArray(events)) return ['missing-pointer-events'];
  const expected = drag ? ['pointerdown','pointerup'] : ['pointerdown','pointerup','click'];
  const failures = [];
  let position = -1;
  for (const type of expected) {
    const index = events.findIndex((event,i) => i > position && event.type === type);
    const event = events[index];
    if (!event || !event.isTrusted || !event.targetIsRaw || event.targetTag !== 'PRE'
      || event.button !== 0 || event.buttons !== (type === 'pointerdown' ? 1 : 0)) failures.push(`raw-${type}`);
    position = index;
  }
  return failures;
}

export function rawCopyFailures(observation, text) {
  const failures = [];
  if (observation?.selected !== text || observation?.selectionInsideRaw !== true) failures.push('raw-selection');
  const keys = observation?.events?.filter(e => e.type === 'keydown' && e.key?.toLowerCase() === 'c') ?? [];
  if (keys.length !== 1 || !keys[0].isTrusted || !keys[0].ctrlKey || keys[0].defaultPrevented
    || keys[0].selected !== text || !keys[0].selectionInsideRaw) failures.push('native-copy-default');
  const copies = observation?.events?.filter(e => e.type === 'copy') ?? [];
  if (copies.length !== 1 || !copies[0].isTrusted || copies[0].defaultPrevented
    || copies[0].selected !== text || !copies[0].selectionInsideRaw) failures.push('native-copy-event');
  return failures;
}

export function assertRawDragProbe(snapshot, afterSequence) {
  return assertTrustedProbeSequence(snapshot,{afterSequence,expected:[
    {type:'pointermove',buttons:0},
    {type:'pointerdown',button:0,buttons:1},
    ...Array.from({length:8},()=>({type:'pointermove',buttons:1})),
    {type:'pointerup',button:0,buttons:0},
  ]});
}

// The current shared input owner supports only semantic-target center clicks.
// PRE has no stable input identity; keep this glyph-bounded delivery local to
// this one scenario, with no retry, native helper or weakened shared guard.
export async function rawMouse(cdp, start, end = start) {
  let pressed = false, releaseIssued = false, last = start;
  const send = (type,point,buttons) => cdp.call('Input.dispatchMouseEvent', {
    type,x:point.x,y:point.y,button:type==='mouseMoved'&&buttons===0?'none':'left',buttons,modifiers:0,
    ...(type==='mouseMoved'?{}:{clickCount:1}),
  });
  try {
    await send('mouseMoved',start,0);
    pressed = true;
    await send('mousePressed',start,1);
    if (end !== start) {
      for (let step=1;step<=8;step++) {
        last={x:start.x+(end.x-start.x)*step/8,y:start.y+(end.y-start.y)*step/8};
        await send('mouseMoved',last,1);
      }
    }
    releaseIssued=true;
    await send('mouseReleased',last,0);
  } finally {
    // One release also covers an uncertain press/move; an uncertain release is
    // never resent. The outer existing lifecycle still owns window teardown.
    if(pressed&&!releaseIssued)await send('mouseReleased',last,0);
  }
}

export async function exerciseRawReviewInteraction({ cdp, input, text, assertUnchanged, record, screenshot }) {
  const installed=await cdp.evaluate(installExpression());
  if(!installed.installed)throw error('harness','probe','Raw review observer is already installed',installed);
  let primary=null;
  try {
    const initial=await cdp.evaluate(geometryExpression(text));
    if(!initial.valid)throw error('harness','geometry','Raw glyphs are not an exact visible one-line pointer target',initial);
    const before=(await input.snapshotProbe()).sequence;
    await rawMouse(cdp,initial.start);
    await assertUnchanged('Raw text click preserves the same review dialog and both drafts');
    const clicked=await cdp.evaluate(observationExpression);
    if(rawPointerFailures(clicked.events).length)throw error('product','click','Raw text click did not remain within PRE',clicked);
    const clickProof=assertTrustedProbeSequence(await input.snapshotProbe(before),{afterSequence:before,expected:[
      {type:'pointerdown',button:0,buttons:1},{type:'pointerup',button:0,buttons:0},{type:'click',button:0,buttons:0},
    ]});
    await record('review-raw-click',{geometry:initial,observation:clicked,proof:clickProof});
    await screenshot('raw-click');

    const next=await cdp.evaluate(geometryExpression(text));
    if(!next.valid)throw error('harness','geometry','Raw glyphs moved outside the exact pointer target',next);
    const eventStart=clicked.events.length, dragStart=(await input.snapshotProbe()).sequence;
    await rawMouse(cdp,next.start,next.end);
    await assertUnchanged('Mouse selection preserves the same review dialog and both drafts');
    const selected=await cdp.evaluate(observationExpression);
    await record('review-raw-drag-delivery',{geometry:next,observation:selected,probe:await input.snapshotProbe(dragStart)});
    if(rawPointerFailures(selected.events.slice(eventStart),{drag:true}).length
      ||selected.selected!==text||!selected.selectionInsideRaw)throw error('product','selection','Real mouse drag did not select the exact raw text within PRE',selected);
    const dragProof=assertRawDragProbe(await input.snapshotProbe(dragStart),dragStart);
    await record('review-raw-selection',{geometry:next,observation:selected,proof:dragProof});
    await screenshot('raw-selected');

    const keyStart=(await input.snapshotProbe()).sequence;
    await input.keyDown('Control');try{await input.pressKey('c');}finally{await input.keyUp('Control');}
    await assertUnchanged('Native selection Copy preserves the same review dialog and both drafts');
    const copied=await cdp.evaluate(observationExpression),failures=rawCopyFailures(copied,text);
    if(failures.length)throw error('product','copy','Raw selection Copy default was suppressed or selection changed',{failures,copied});
    const keyProof=assertTrustedProbeSequence(await input.snapshotProbe(keyStart),{afterSequence:keyStart,expected:[
      {type:'keydown',key:'Control'},{type:'keydown',key:'c'},{type:'keyup',key:'c'},{type:'keyup',key:'Control'},
    ]});
    await record('review-raw-copy',{observation:copied,proof:keyProof,os_clipboard_content_verified:false});
  }catch(caught){primary=caught;throw caught;}
  finally{
    try{await cdp.evaluate(`(()=>{const key=Symbol.for(${JSON.stringify(PROBE)}),state=globalThis[key];state?.controller.abort();delete globalThis[key];return true;})()`);}
    catch(cleanup){if(!primary)throw cleanup;}
  }
}
