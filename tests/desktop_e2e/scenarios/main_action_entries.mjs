import path from 'node:path';
import {readdir,readFile} from 'node:fs/promises';
import {isDeepStrictEqual as same} from 'node:util';
import {observeMainControls,replaceMainPrompt} from './main_remaining_controls.mjs';

const SHORTCUT='[aria-labelledby="shortcuts-dialog-title"]';
const PALETTE='[aria-labelledby="command-palette-dialog-title"]';
const act=(id,scope)=>({selector:`${scope} button[data-action=${JSON.stringify(id)}]`,identity:{tag:'BUTTON',action:id}});
const fail=(d,message,evidence)=>new d.DesktopE2eError('product','main-action-entry',message,evidence);
async function observeEntryControls(d) {
  const value=await observeMainControls(d);
  value.entryShell=await d.cdp.evaluate(`(()=>{
    const disabled=e=>e.disabled||e.getAttribute('aria-disabled')==='true';
    return {title:document.querySelector('.topbar h1')?.textContent.trim(),
      access:Array.from(document.querySelectorAll('.topbar [data-session-settings-trigger="access"],.topbar button[data-action="toggle-access"]')).map(e=>({label:e.textContent.trim(),disabled:disabled(e)})),
      archive:Array.from(document.querySelectorAll('aside.sidebar button[data-action="toggle-session-archived-search"]')).map(e=>({selected:e.classList.contains('selected'),disabled:disabled(e)})),
      project:Array.from(document.querySelectorAll('.topbar .chips > button[data-action="open-workspace-folder"],.topbar .chips > button[data-action="create-project-from-picker"]')).map(e=>({label:e.textContent.trim(),path:e.title})),
      emptyHeading:document.querySelector('.empty-thread h2')?.textContent.trim()??null};
  })()`);
  return value;
}
export function entryDomFailures(value, step={}) {
  const failures=[],p=value?.p,shell=value?.entryShell;
  if(!same(value?.composerRunTarget,p?.run_target))failures.push('composer-dom-owner');
  const title=p?.selected_session_index<0?'新しいチャット':p?.selected_session_title;
  if(!shell||shell.title!==title||value?.status?.text!==p?.status_message)failures.push('title-status-dom');
  if(step.access!==undefined){
    const label={default:'承認を求める',auto_review:'代理で承認',full_access:'フルアクセス'}[step.access];
    if(!label||shell?.access?.length!==1||shell.access[0].label!==label||shell.access[0].disabled)failures.push('access-dom');
  }
  if(step.archived!==undefined&&(shell?.archive?.length!==1||shell.archive[0].selected!==step.archived||shell.archive[0].disabled))failures.push('archive-dom');
  if(step.quick===true&&(shell?.project?.length!==1||shell.project[0].label!=='プロジェクトなし'
    ||shell.project[0].path!==p?.workspace_path||shell.emptyHeading!=='何に取り組みますか？'))failures.push('quick-chat-dom');
  return failures;
}
export const SHORTCUT_ENTRY_STEPS=Object.freeze([
  {action:'show-command-palette',overlay:'command_palette',command:'show_command_palette'},
  {action:'toggle-session-archived-search',archived:true,command:'set_session_search_include_archived'},
  {action:'toggle-session-archived-search',archived:false,command:'set_session_search_include_archived'},
  {action:'toggle-access',access:'auto_review',command:'toggle_access_mode'},
  {action:'toggle-access',access:'full_access',command:'toggle_access_mode'},
  {action:'toggle-access',access:'default',command:'toggle_access_mode'},
]);
export async function activateMainActionEntry(d,id,entry) {
  if(!['shortcuts','palette'].includes(entry))throw new TypeError('Unknown Main action entry');
  let target;
  if(entry==='shortcuts'){
    const current=await observeMainControls(d);
    if(current.p.overlay!=='shortcuts')await d.trustedClick(d.input,d.cdp,act('show-shortcuts','aside.sidebar'),d.sink);
    await d.wait('Shortcuts row dialog is actually open',async()=>({p:(await observeMainControls(d)).p,count:await d.cdp.evaluate(`document.querySelectorAll('${SHORTCUT}').length`)}),v=>v.p.overlay==='shortcuts'&&v.count===1);
    target=act(id,SHORTCUT);
    if(id==='new-chat')target.identity.focusKey='shortcut-action:new-chat';
  }else{
    const current=await observeMainControls(d);
    if(current.p.overlay!=='command_palette'){
      await d.input.keyDown('Control');try{await d.input.pressKey('k');}finally{await d.input.keyUp('Control');}
    }
    await d.wait('Palette opens for exact action',()=>observeMainControls(d),v=>v.p.overlay==='command_palette');
    const search={selector:'#local-search',identity:{tag:'INPUT',id:'local-search'}};
    await d.trustedClick(d.input,d.cdp,search,d.sink);
    await d.input.keyDown('Control');try{await d.input.pressKey('a');}finally{await d.input.keyUp('Control');}
    await d.input.pressKey('Backspace');
    const start=(await d.input.snapshotProbe()).sequence;
    await d.input.insertText(search,id);
    const proof=d.assertTrustedTextInsertion(await d.input.snapshotProbe(start),{afterSequence:start,identity:search.identity,text:id});
    await d.sink.record('main-entry-palette-search',{id,proof},{phase:'executing',owner:d.owner});
    await d.wait('Palette search and visible unique row settle',async()=>({p:(await observeMainControls(d)).p,dom:await d.cdp.evaluate(`(()=>{const n=document.querySelector('#local-search');return {search:n?.value,count:document.querySelectorAll('${PALETTE} button[data-focus-key="palette-action:${id}"]').length};})()`)}),v=>v.p.local_search_text===id&&v.dom.search===id&&v.dom.count===1);
    target={selector:`${PALETTE} button[data-focus-key="palette-action:${id}"]`,identity:{tag:'BUTTON',action:id,focusKey:`palette-action:${id}`}};
  }
  const before=await observeMainControls(d),sequence=(await d.commands.snapshot()).sequence;
  await d.trustedClick(d.input,d.cdp,target,d.sink);
  return {before,sequence,target};
}
export async function submitMainFromEntry(d,text,entry){
  const activated=await activateMainActionEntry(d,'send',entry);
  if(activated.before.prompt.value!==text)throw fail(d,'Main draft changed when opening the action entry',activated);
  const calls=await d.wait('Action entry sends one actual Main command',()=>d.commands.snapshot(activated.sequence),v=>v.calls?.length>0);
  d.assertExactDesktopCommandSequence(calls,{afterSequence:activated.sequence,expected:[{command:'submit_prompt',args:{text,
    expectedTarget:activated.before.p.draft_target,expectedRunTarget:activated.before.p.run_target}}]});
  await d.sink.record('main-entry-send',{entry,text,activated,calls},{phase:'executing',owner:d.owner});
}
export function shortcutResultFailures(value,before,step,ledger,currentLedger){
  const failures=entryDomFailures(value,step),p=value?.p;
  if(!p||p.busy!==false||p.post_run_refresh_pending!==false||p.background_mutation_pending!==false||p.pending_async_operations?.length!==0)failures.push('pending');
  if(!same(p?.run_target,before.p.run_target)||!same(p?.draft_target,before.p.draft_target)||!same(p?.transcript_rows,before.p.transcript_rows))failures.push('owner-history');
  if(value?.prompt?.value!==before.prompt.value||value?.visibleErrors?.length!==0)failures.push('draft-error');
  if(step.overlay!==undefined&&p?.overlay!==step.overlay)failures.push('overlay');
  if(step.archived!==undefined&&p?.session_search_include_archived!==step.archived)failures.push('archive');
  if(step.access!==undefined&&p?.access_label!==step.access)failures.push('access');
  if(!same(currentLedger,ledger))failures.push('provider');
  return failures;
}
export async function exerciseShortcutRows(d){
  const initial=await observeEntryControls(d),ledger=structuredClone(d.provider.requestLedger);
  if(initial.p.access_label!=='default'||initial.p.session_search_include_archived)throw fail(d,'Shortcuts fixture must begin with default access and ordinary search',initial);
  for(const step of SHORTCUT_ENTRY_STEPS){
    const before=await observeEntryControls(d),activated=await activateMainActionEntry(d,step.action,'shortcuts');
    const result=await d.wait('Shortcut row changes exact intended state and visible DOM',()=>observeEntryControls(d),v=>shortcutResultFailures(v,before,step,ledger,d.provider.requestLedger).length===0);
    const calls=(await d.commands.snapshot(activated.sequence)).calls;
    if(calls.length!==1||calls[0].command!==step.command)throw fail(d,'Shortcut row did not issue one associated command',{step,calls});
    await d.captureScenarioScreenshot({cdp:d.cdp,sink:d.sink,name:`shortcut-row-${step.action}-${step.archived??step.access??'opened'}`,owner:d.owner});
    await d.input.pressKey('Escape');
    await d.wait('Shortcut destination dismisses to the same shell',()=>observeMainControls(d),v=>v.p.overlay==='none'&&same(v.p.run_target,before.p.run_target)&&v.visibleErrors.length===0);
    await d.sink.record('shortcut-row-result',{step,activated,result,calls},{phase:'executing',owner:d.owner});
  }
  const unsent='This unsent Shortcuts export draft must not be exported.';
  await replaceMainPrompt(d,unsent);
  const exportBefore=await observeMainControls(d),directory=path.join(d.context.paths.workspace,'.moyai','transcript-exports');
  const filesBefore=await readdir(directory).catch(error=>{if(error.code==='ENOENT')return [];throw error;});
  if(filesBefore.length)throw fail(d,'Isolated transcript export directory must initially be empty',filesBefore);
  const exported=await activateMainActionEntry(d,'export-transcript','shortcuts');
  const exportAfter=await d.wait('F9 row directly saves current transcript',()=>observeMainControls(d),v=>
    v.p.status_message.startsWith('saved transcript markdown to ')&&v.prompt.value===unsent&&!v.p.busy&&v.visibleErrors.length===0);
  const files=await readdir(directory),file=files.length===1?files[0]:null;
  if(!file||!file.endsWith('.md'))throw fail(d,'F9 row must create one Markdown file',files);
  const text=await readFile(path.join(directory,file),'utf8');
  const expectedRows=exportBefore.p.transcript_rows.filter(row=>['user','assistant'].includes(row.row_kind)).map(row=>row.body);
  if(expectedRows.length!==2||expectedRows.some(body=>!text.includes(body))||text.includes(unsent)
    ||!same(exportAfter.p.transcript_rows,exportBefore.p.transcript_rows)||!same(exportAfter.p.run_target,exportBefore.p.run_target))throw fail(d,'F9 export changed owner or exported the unsent draft',{exportBefore,exportAfter,text});
  const exportCalls=(await d.commands.snapshot(exported.sequence)).calls;
  if(exportCalls.length!==1||exportCalls[0].command!=='export_transcript_markdown')throw fail(d,'F9 row did not issue one direct transcript export',exportCalls);
  await d.captureScenarioScreenshot({cdp:d.cdp,sink:d.sink,name:'shortcut-row-export-menu-stays-open',owner:d.owner});
  await d.input.pressKey('Escape');
  await d.wait('F9 row result becomes visible after dismissing the still-open list',()=>observeMainControls(d),v=>v.p.overlay==='none'&&v.prompt.value===unsent);
  await d.captureScenarioScreenshot({cdp:d.cdp,sink:d.sink,name:'shortcut-row-export-saved',owner:d.owner});
  await d.sink.record('shortcut-row-export',{file:path.join(directory,file),bytes:Buffer.byteLength(text),expectedRows,unsent,exported,exportAfter,exportCalls,
    scope:'Actual Shortcuts F9 row writes current canonical transcript directly; no native picker. Unsent draft excluded.'},{phase:'executing',owner:d.owner});
  await replaceMainPrompt(d,'');
  const newChat=await activateMainActionEntry(d,'new-chat','shortcuts');
  const quick=await d.wait('Ctrl+N row opens an independent empty Quick Chat with matching DOM',()=>observeEntryControls(d),v=>
    v.p.overlay==='none'&&!v.p.busy&&!v.p.navigation_loading&&v.p.draft_target.sessionId===null&&v.p.thread_empty&&v.prompt.value===''
    &&path.normalize(v.p.workspace_path)===path.normalize(path.join(d.context.paths.data,'quick-chat-workspace'))&&v.visibleErrors.length===0
    &&entryDomFailures(v,{quick:true}).length===0);
  const calls=(await d.commands.snapshot(newChat.sequence)).calls;
  if(calls.length!==1||calls[0].command!=='new_chat')throw fail(d,'Ctrl+N row issued an unexpected command',calls);
  await d.captureScenarioScreenshot({cdp:d.cdp,sink:d.sink,name:'shortcut-row-new-chat',owner:d.owner});
  const closed=await activateMainActionEntry(d,'close-overlay','shortcuts');
  await d.wait('Esc row closes Shortcuts dialog',()=>observeMainControls(d),v=>v.p.overlay==='none'&&v.visibleErrors.length===0);
  if(!same(d.provider.requestLedger,ledger))throw fail(d,'Shortcut navigation unexpectedly contacted the model',d.provider.requestLedger);
  await d.sink.record('shortcut-row-final',{newChat,quick,calls,closed,notRun:['actual keyboard shortcuts other than palette opening','runtime-sensitive disabled rows']},{phase:'executing',owner:d.owner});
}
