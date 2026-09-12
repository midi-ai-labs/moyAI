// Caller supplies the existing scenario dependencies.
// No launcher, injected state, private mutation, or cleanup owner is defined here.
import { isDeepStrictEqual } from 'node:util';
const PROMPT = { selector: 'section.composer #prompt', identity: { tag: 'TEXTAREA', id: 'prompt' } };
const SEND = { selector: 'section.composer button[data-action="send"]', identity: { tag: 'BUTTON', action: 'send' } };
const same = isDeepStrictEqual;
const posts = provider => provider.requestLedger.filter(row => row.method === 'POST');
const fail = (d, message, evidence) => new d.DesktopE2eError('product', 'main-controls-audit', message, evidence);
const record = (d, name, value) => d.sink.record(name, value, { phase: 'executing', owner: d.owner });

export async function observeMainControls(d) {
  return d.cdp.evaluate(`(async () => {
    const p=await window.__TAURI_INTERNALS__.invoke('desktop_state');
    const prompt=document.querySelector('section.composer #prompt');
    const send=document.querySelector('section.composer [data-action="send"]');
    const hint=document.querySelector('#goal-command-hint');
    const composer=document.querySelector('section.composer');
    const details=Array.from(document.querySelectorAll('details[data-details-key="status-detail"]'));
    return { p, composerRunTarget:composer?.dataset.runTarget?JSON.parse(composer.dataset.runTarget):null,
      prompt: { value:prompt?.value, selection:[prompt?.selectionStart,prompt?.selectionEnd], disabled:prompt?.disabled },
      send: { disabled:send?.disabled, ariaDisabled:send?.getAttribute('aria-disabled'), title:send?.title, label:send?.getAttribute('aria-label') },
      hint: { visible:Boolean(hint&&!hint.hidden),text:hint?.textContent?.trim() },
      status: { text:document.querySelector('.status-line > span')?.textContent?.trim(), detailCount:details.length,
        detailText:details[0]?.querySelector('pre')?.textContent, open:details[0]?.open,
        focused:document.activeElement===details[0]?.querySelector('summary') },
      runStrip: Array.from(document.querySelectorAll('section.run-strip')).some(e=>e.getClientRects().length>0),
      pending:Array.from(document.querySelectorAll('[data-pending-input-id]')).map(e=>({id:e.dataset.pendingInputId,turn:e.dataset.turnId,text:e.textContent.trim()})),
      visibleErrors:Array.from(document.querySelectorAll('.fatal,.ui-error-notice')).filter(e=>e.getClientRects().length).map(e=>e.textContent.trim()) };
  })()`);
}
const observe = observeMainControls;
export async function replaceMainPrompt(d, text) {
  await d.trustedClick(d.input,d.cdp,PROMPT,d.sink);
  await d.input.keyDown('Control');
  try { await d.input.pressKey('a'); } finally { await d.input.keyUp('Control'); }
  await d.input.pressKey('Backspace');
  const afterSequence=(await d.input.snapshotProbe()).sequence;
  if (text) {
    await d.input.insertText(PROMPT,text);
    const proof=d.assertTrustedTextInsertion(await d.input.snapshotProbe(afterSequence),{afterSequence,identity:PROMPT.identity,text});
    await record(d,'main-controls-text',{text,proof});
  }
  return d.wait('Actual composer contains exact draft',()=>observe(d),v=>v.prompt.value===text&&!v.prompt.disabled);
}
const replacePrompt = replaceMainPrompt;
export async function submitMainPrompt(d,text,entry='pointer') {
  const before=await observe(d), afterSequence=(await d.commands.snapshot()).sequence;
  if (before.prompt.value!==text) throw fail(d,'Submit draft changed before activation',before);
  if(entry==='pointer') await d.trustedClick(d.input,d.cdp,SEND,d.sink);
  else {
    const start=(await d.input.snapshotProbe()).sequence;
    await d.input.keyDown('Control');
    try { await d.input.pressKey('Enter'); } finally { await d.input.keyUp('Control'); }
    const proof=d.assertTrustedProbeSequence(await d.input.snapshotProbe(start),{afterSequence:start,expected:[
      {type:'keydown',identity:PROMPT.identity,key:'Control',code:'ControlLeft'},
      {type:'keydown',identity:PROMPT.identity,key:'Enter',code:'Enter'},
      {type:'keyup',identity:PROMPT.identity,key:'Enter',code:'Enter'},
      {type:'keyup',identity:PROMPT.identity,key:'Control',code:'ControlLeft'},
    ]});
    await record(d,'main-controls-keyboard-submit',{proof});
  }
  const commands=await d.wait('Submitted GUI command is captured',()=>d.commands.snapshot(afterSequence),v=>v.calls?.length>=1);
  d.assertExactDesktopCommandSequence(commands,{afterSequence,expected:[{command:'submit_prompt',args:{
    text,expectedTarget:before.p.draft_target,expectedRunTarget:before.p.run_target,
  }}]});
  await record(d,'main-controls-submitted',{entry,before,commands});
  return before;
}
const submit = submitMainPrompt;

export function steerQueueFailures(accepted,{base,texts,ledger,currentLedger}) {
  const failures=[],rows=accepted?.p?.pending_turn_inputs;
  if(!Array.isArray(rows)||rows.length!==texts.length||rows.some((row,i)=>row.text!==texts[i]
    ||row.turn_id!==base.p.run_target.expectedState.turnId)||new Set(rows?.map(row=>row.id)).size!==rows?.length) failures.push('pending-identities-or-text');
  if(!Array.isArray(accepted?.pending)||accepted.pending.length!==texts.length
    ||rows?.some(row=>!accepted.pending.some(dom=>dom.id===row.id&&dom.turn===row.turn_id&&dom.text===row.text))) failures.push('visible-queue');
  if(!same(accepted?.p?.run_target,base.p.run_target)||accepted?.p?.composer_submit_mode!=='steer') failures.push('run-owner');
  if(accepted?.prompt?.value!=='') failures.push('draft-not-cleared');
  if(!same(currentLedger,ledger)) failures.push('additional-provider-request');
  if(accepted?.visibleErrors?.length!==0) failures.push('visible-error');
  return failures;
}
export function classifyGoalQuery(evidence) {
  const {before,result,providerBefore,providerAfter}=evidence;
  if(!same(providerBefore,providerAfter)) return 'goal-query-contacted-provider';
  if(result?.p?.run_target?.sessionId!==before?.p?.run_target?.sessionId
    ||!same(result?.p?.run_target?.expectedState,before?.p?.run_target?.expectedState)) return 'goal-query-changed-turn-owner';
  if(/run command completed without a terminal turn summary/.test(result?.p?.status_message??'')) return 'goal-control-reported-as-run-failure';
  if(result?.p?.status_code!=='goal_control'||result?.p?.status_detail!==GOAL_EMPTY_TEXT
    ||result?.status?.text!==GOAL_STATUS_TEXT||result?.status?.detailCount!==1||result?.status?.detailText!==GOAL_EMPTY_TEXT) return 'goal-result-needs-visible-feedback-oracle';
  if(!same(result.p.transcript_rows,before.p.transcript_rows)||result.p.run_status_key!==before.p.run_status_key) return 'goal-query-changed-history';
  if(!/^\d+$/.test(result.p.composer_commit_generation??'')||!/^\d+$/.test(before.p.composer_commit_generation??'')
    ||BigInt(result.p.composer_commit_generation)!==BigInt(before.p.composer_commit_generation)+1n
    ||result.prompt?.value!==''||result.prompt?.disabled!==false||!same(result.composerRunTarget,result.p.run_target)) return 'goal-composer-not-settled';
  if(result.runStrip!==false||result.visibleErrors?.length!==0) return 'goal-visible-error-or-running';
  return 'goal-query-visible-success';
}
export const GOAL_EMPTY_TEXT='このセッションには Goal が設定されていません。';
export const GOAL_STATUS_TEXT='Goal の状態を確認しました。';
export function stoppedSteerFailures(value,sessionId) {
  const failures=[],s=value?.surface,p=s?.p;
  if(p?.run_status_key!=='cancelled'||p.busy!==false||p.post_run_refresh_pending!==false||p.can_submit!==true
    ||p.run_target?.sessionId!==sessionId||p.run_target?.expectedState?.kind!=='idle') failures.push('terminal-owner');
  if(!same(s?.composerRunTarget,p?.run_target)||s?.runStrip!==false||s?.prompt?.disabled!==false) failures.push('terminal-dom');
  if(s?.visibleErrors?.length!==0) failures.push('visible-error');
  if(value?.ledger?.length!==1||value.ledger[0]?.response_phase!=='peer_closed') failures.push('provider-still-open');
  return failures;
}

// Preconditions: a real, non-Goal first turn is held; command/input probes installed.
// Caller uses existing actual Stop after this helper, then existing cleanup.
export async function exerciseHeldSteer(d) {
  const base=await observe(d), ledger=structuredClone(posts(d.provider));
  if(base.p.composer_submit_mode!=='steer'||base.p.run_target.expectedState.kind!=='turn'
    ||ledger.length!==1||ledger[0].contract?.pass!==true||ledger[0].response_phase!=='held') {
    throw fail(d,'Steer requires one actual held turn', {base,ledger});
  }
  const texts=['Additional direction from pointer.','Additional direction from keyboard.'];
  const queued=[];
  for(let index=0;index<texts.length;index++) {
    const blank=await replacePrompt(d,'');
    if(blank.send.title!=='依頼文を入力してください') throw fail(d,'Empty live steer is not explained',blank);
    // Current can_submit is admission availability, not draft non-emptiness.
    // If enabled, actually activate blank Send and prove it creates no pending input.
    if(!blank.send.disabled) {
      await submit(d,'');
      const unchanged=await d.wait('Blank Send leaves the same running turn available',()=>observe(d),v=>
        v.p.composer_submit_mode==='steer'&&same(v.p.run_target,base.p.run_target));
      if(unchanged.p.pending_turn_inputs.length!==index||!same(posts(d.provider),ledger)) throw fail(d,'Blank Send created work',unchanged);
      await record(d,'main-steer-blank-send',{blank,unchanged});
      if(unchanged.visibleErrors.length) {
        const dismiss={selector:'.ui-error-notice button[data-action="dismiss-ui-error"]',identity:{tag:'BUTTON',action:'dismiss-ui-error'}};
        await d.trustedClick(d.input,d.cdp,dismiss,d.sink);
        await d.wait('Rejected blank input notice is explicitly dismissed',()=>observe(d),v=>v.visibleErrors.length===0);
      }
    }
    const ready=await replacePrompt(d,texts[index]);
    if(ready.send.title!=='実行中のタスクへ追加指示を送信'||ready.send.label!==ready.send.title||ready.send.disabled
      ||!same(ready.p.run_target,base.p.run_target)) throw fail(d,'Live direction meaning or owner differs',ready);
    await submit(d,texts[index],index?'keyboard':'pointer');
    const accepted=await d.wait('Direction persists once on same held turn',()=>observe(d),v=>
      v.p.composer_submit_mode==='steer'&&v.prompt.value===''&&same(v.p.run_target,base.p.run_target)
      &&v.p.pending_turn_inputs.length===index+1&&v.pending.length===index+1);
    const failures=steerQueueFailures(accepted,{base,texts:texts.slice(0,index+1),ledger,currentLedger:posts(d.provider)});
    if(failures.length) throw fail(d,'Steer was duplicated, delivered elsewhere, or caused another model request',{failures,accepted,ledger:posts(d.provider)});
    queued.push(accepted);
    await d.captureScenarioScreenshot({cdp:d.cdp,sink:d.sink,name:`main-steer-${index?'keyboard':'pointer'}-queued`,owner:d.owner});
  }
  await record(d,'main-steer-two-input-paths',{base,queued,providerLedger:posts(d.provider),notRun:['provider consumption','terminal-before-click race','Goal slash during Running']});
}

// Preconditions: an existing idle session with completed GUI turns; no running provider requests.
// Preserve canonical history and require both typed state and the rendered Goal result.
export async function observeGoalQuery(d) {
  const before=await observe(d), ledger=structuredClone(posts(d.provider));
  if(before.p.busy||before.p.run_target?.expectedState?.kind!=='idle'||!before.p.run_target.sessionId) throw fail(d,'Goal query requires settled existing session',before);
  const typed=await replacePrompt(d,'/goal');
  await d.captureScenarioScreenshot({cdp:d.cdp,sink:d.sink,name:'main-goal-query-before-send',owner:d.owner});
  if(!typed.hint.visible||!typed.hint.text.includes('現在のgoalを表示')) throw fail(d,'Goal query hint does not explain current operation',typed);
  await submit(d,'/goal');
  const result=await d.wait('Goal control finishes or exposes failure',()=>observe(d),v=>
    !v.p.busy&&!v.p.post_run_refresh_pending&&!v.p.background_mutation_pending&&v.p.can_submit
    &&v.p.status_message!==before.p.status_message&&same(v.composerRunTarget,v.p.run_target)
    &&(v.p.status_code!=='goal_control'||v.prompt.value===''&&v.status.detailText===v.p.status_detail));
  await d.captureScenarioScreenshot({cdp:d.cdp,sink:d.sink,name:'main-goal-query-after-send',owner:d.owner});
  const evidence={before,typed,result,providerBefore:ledger,providerAfter:posts(d.provider),
    noProviderRequest:same(ledger,posts(d.provider)),
    successfulControlMisclassified:/run command completed without a terminal turn summary/.test(result.p.status_message),
    verdict:'Typed control result, visible result, consumed input and unchanged canonical history are independently required.'};
  evidence.classification=classifyGoalQuery(evidence);
  await record(d,'main-goal-query-observation',evidence);
  if(evidence.classification!=='goal-query-visible-success') return evidence;
  const detail={selector:'details[data-details-key="status-detail"] > summary',identity:{tag:'DETAILS',detailsKey:'status-detail'}};
  const checkDetail=async(open,label)=>{
    const value=await d.wait(label,()=>observe(d),v=>v.status.detailCount===1&&v.status.open===open&&v.status.detailText===GOAL_EMPTY_TEXT);
    if(value.p.status_code!=='goal_control'||!same(value.p.transcript_rows,result.p.transcript_rows)||!same(posts(d.provider),ledger)
      ||value.prompt.value!==''||value.visibleErrors.length) throw fail(d,'Goal disclosure changed result, history, or input',value);
    return value;
  };
  await d.trustedClick(d.input,d.cdp,detail,d.sink);
  const opened=await checkDetail(true,'Exact Goal detail opens with complete result');
  await d.captureScenarioScreenshot({cdp:d.cdp,sink:d.sink,name:'main-goal-detail-open',owner:d.owner});
  const refreshSequence=(await d.commands.snapshot()).sequence;
  await d.trustedClick(d.input,d.cdp,{selector:'.sidebar [data-action="refresh"]',identity:{tag:'BUTTON',action:'refresh'}},d.sink);
  const refreshCommands=await d.wait('Explicit refresh command is observed',()=>d.commands.snapshot(refreshSequence),v=>v.calls?.length>0);
  d.assertExactDesktopCommandSequence(refreshCommands,{afterSequence:refreshSequence,expected:[{command:'refresh_desktop',args:{}}]});
  await d.wait('Actual refresh settles without changing the Goal result',()=>observe(d),v=>
    !v.p.busy&&!v.p.post_run_refresh_pending&&!v.p.background_mutation_pending&&v.p.pending_async_operations.length===0);
  const refreshed=await checkDetail(true,'Open Goal detail survives actual Refresh');
  await d.captureScenarioScreenshot({cdp:d.cdp,sink:d.sink,name:'main-goal-detail-refreshed',owner:d.owner});
  await d.trustedClick(d.input,d.cdp,detail,d.sink);
  const closed=await checkDetail(false,'Exact Goal detail closes');
  const sequence=(await d.input.snapshotProbe()).sequence;
  await d.input.pressKey('Enter');
  const proof=d.assertTrustedProbeSequence(await d.input.snapshotProbe(sequence),{afterSequence:sequence,expected:[
    {type:'keydown',identity:detail.identity,key:'Enter',code:'Enter'},
    {type:'keyup',identity:detail.identity,key:'Enter',code:'Enter'},
  ]});
  const keyboardOpened=await checkDetail(true,'Keyboard reopens exact Goal detail');
  await d.trustedClick(d.input,d.cdp,detail,d.sink);
  await checkDetail(false,'Goal detail ends closed');
  await record(d,'main-goal-detail-controls',{opened,refreshed,closed,keyboardOpened,proof,refreshCommands,
    notRun:['natural periodic refresh: idle disconnected shell has no polling subscription','Goal set/clear via GUI','long Goal body scroll']});
  return evidence;
}

export const RAIL_FIXTURE_TURNS=Object.freeze([1,2].map(i=>({prompt:`Complete rail fixture turn ${i}.`,
  responseText:Array.from({length:35},(_,n)=>`Turn ${i} evidence paragraph ${n+1}. This line belongs to this response only.`).join('\n\n')})));

async function railSurface(d) {
  return d.cdp.evaluate(`(async()=>{
    const p=await window.__TAURI_INTERNALS__.invoke('desktop_state'),thread=document.querySelector('#thread'),prompt=document.querySelector('#prompt');
    const clip=thread.getBoundingClientRect();
    const rows=Array.from(thread.querySelectorAll('.history-rail-marker')).map(e=>{
      const anchor=e.dataset.historyTarget,targets=Array.from(thread.querySelectorAll('[data-history-anchor]')).filter(t=>t.dataset.historyAnchor===anchor),target=targets[0];
      const summary=target?.querySelector('.message-body > details > summary'),focus=summary??target,r=focus?.getBoundingClientRect();
      return {anchor,focusKey:e.dataset.focusKey,label:e.getAttribute('aria-label'),targetCount:targets.length,identity:target?.dataset.historyIdentity,
        focused:document.activeElement===focus,open:summary?summary.parentElement.open:null,
        destinationVisible:!!r&&r.top>=clip.top&&r.top<clip.bottom&&r.right>clip.left&&r.left<clip.right};
    });
    return {p,rows,scroll:thread.scrollTop,overflow:thread.scrollHeight-thread.clientHeight,
      prompt:prompt.value,selection:[prompt.selectionStart,prompt.selectionEnd],
      errors:Array.from(document.querySelectorAll('.fatal,.ui-error-notice')).filter(e=>e.getClientRects().length).length};
  })()`);
}
export function railNavigationFailures(reached,base,wanted) {
  const failures=[];
  if(!same(reached?.p?.run_target,base.p.run_target)||!same(reached?.p?.draft_target,base.p.draft_target)
    ||reached?.p?.composer_commit_generation!==base.p.composer_commit_generation
    ||reached?.p?.turn_page_offset!==base.p.turn_page_offset||reached?.p?.selected_session_index!==base.p.selected_session_index) failures.push('owner-or-page');
  if(!same(reached?.rows?.map(row=>[row.anchor,row.identity]),base.rows.map(row=>[row.anchor,row.identity]))) failures.push('canonical-identity');
  const destination=reached?.rows?.filter(row=>row.anchor===wanted.anchor);
  if(destination?.length!==1||destination[0].targetCount!==1||!destination[0].focused||!destination[0].destinationVisible
    ||destination[0].open===false) failures.push('destination');
  if(reached?.prompt!==base.prompt||!same(reached?.selection,base.selection)) failures.push('draft-or-selection');
  if(reached?.errors!==0) failures.push('visible-error');
  return failures;
}
// Preconditions: two actual completed GUI turns with RAIL_FIXTURE_TURNS; probes installed.
export async function exerciseLoadedHistoryRail(d) {
  await replacePrompt(d,'Preserve this unsent rail audit draft.');
  const base=await railSurface(d);
  if(base.p.busy||base.rows.length!==6||base.overflow<100||base.rows.some(row=>row.targetCount!==1||!row.identity)) throw fail(d,'Rail fixture does not expose six canonical entries and real overflow',base);
  const visited=[],indices=[0,5,1,4,2,3];
  for(const index of indices) {
    const wanted=base.rows[index],target={selector:`#thread .history-rail-marker[data-focus-key=${JSON.stringify(wanted.focusKey)}]`,identity:{tag:'BUTTON',action:'jump-history-anchor',focusKey:wanted.focusKey}};
    await d.trustedClick(d.input,d.cdp,target,d.sink);
    const reached=await d.wait('Exact rail destination focused and visible',()=>railSurface(d),v=>v.rows.some(row=>row.anchor===wanted.anchor&&row.focused&&row.destinationVisible&&(row.open===null||row.open)));
    const failures=railNavigationFailures(reached,base,wanted);
    if(failures.length) throw fail(d,'Rail changed owner, transcript, or unsent editor',{failures,wanted,base,reached});
    visited.push(reached);
    await d.captureScenarioScreenshot({cdp:d.cdp,sink:d.sink,name:`history-rail-entry-${index+1}`,owner:d.owner});
  }
  if(Math.max(...visited.map(v=>v.scroll))-Math.min(...visited.map(v=>v.scroll))<50) throw fail(d,'Rail clicks did not demonstrate actual scroll travel',visited);
  await record(d,'history-rail-six-loaded-entries',{base,visited,notRun:['unloaded page','bounded sampling over the current rail limit','child history']});
}
