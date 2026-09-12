import path from 'node:path';
import process from 'node:process';
import {mkdir,readFile} from 'node:fs/promises';
import {isDeepStrictEqual as same} from 'node:util';
import {DesktopE2eError} from '../core/execution.mjs';
import {waitForSemanticTargetSettlement} from '../core/semantic_target_settlement.mjs';
import {DesktopCommandProbe,assertExactDesktopCommandSequence} from '../drivers/desktop_command_probe.mjs';
import {WebviewInput,assertTrustedProbeSequence,assertTrustedTextInsertion} from '../drivers/webview_input.mjs';
import {runWindowsExternalProcess} from '../drivers/windows_external_process.mjs';
import {startScriptedProvider} from '../drivers/scripted_provider.mjs';
import {prepareDesktopFixture} from './fixture.mjs';
import {providerRestartFixtureConfig} from './provider_restart.mjs';
import {acquireInteractiveShell,requestGracefulExit} from './shell_baseline.mjs';
import {captureScenarioScreenshot,invokeDesktopCommand} from './observations.mjs';
import {action,byId,wait,trustedClick} from './hub_browser_enrollment.mjs';
import {trustedInsert} from './side_chat_quote.mjs';
import {activateMainActionEntry} from './main_action_entries.mjs';

const MAIN='Create original GUI navigation conversation',REPLY='ORIGINAL_NAVIGATION_READY',EXTERNAL='Keep this external root active for sidebar navigation';
const DRAFT='Keep this unsent draft in the original Main conversation';
const PROMPT=byId('prompt','TEXTAREA');
const fail=(message,evidence={})=>new DesktopE2eError('product','external-navigation-mismatch',message,evidence);
const responses=ledger=>ledger.filter(row=>row.route==='responses');
const core=p=>p.transcript_rows.map(row=>({kind:row.row_kind,identity:row.stable_history_identity??null,body:row.body}));
export function externalNavigationProviderOptions(sameSession=false){return {...(sameSession?{orderedConversation:true}:{}),responseBehavior:'hold_until_release',turns:[{prompt:MAIN,responseText:REPLY},{prompt:EXTERNAL,responseText:'UNUSED_EXTERNAL_REPLY'}]};}
export function externalNavigationHeldLedger(ledger,count){const rows=responses(ledger);return rows.length===count&&rows.every(row=>row.contract?.pass===true)&&rows[0].response_phase===(count===1?'held':'completed')&&(count===1||rows[1].response_phase==='held');}
export function externalNavigationMainPreserved(v,main){return v?.errors===0&&same(v.composerRunTarget,v.p?.run_target)&&v.p?.workspace_path===main.workspace&&v.p.run_target?.sessionId===main.sessionId&&v.p.draft_target?.sessionId===main.sessionId
  &&v.p.run_status_key==='completed'&&!v.p.busy&&v.prompt===DRAFT&&v.promptEnabled&&same(core(v.p),main.history);}
export function externalNavigationRowStopped(surface,sessionId){
  const row=surface?.p?.session_rows?.find(row=>row.session_id===sessionId),dom=surface?.sidebarRows?.filter(row=>row.focusKey==='session:'+sessionId+':select')??[];
  return row?.status==='cancelled'&&row.loaded_status==='idle'&&row.active_turn_id==null&&row.interrupt_target==null
    &&surface.p.pending_async_operations.length===0&&!surface.p.async_polling_required
    &&dom.length===1&&dom[0].activity===null&&dom[0].stop===0&&dom[0].rejoin===0;
}
export function externalNavigationActiveReady(sample,main){const {surface,sessionId,ledger}=sample??{},p=surface?.p,row=p?.session_rows?.find(row=>row.session_id===sessionId);
  return Boolean(externalNavigationMainPreserved(surface,main)&&p.navigation_admission_open&&p.overlay==='none'&&sessionId!==main.sessionId
    &&row?.loaded_status==='active'&&row.active_turn_id&&row.interrupt_target&&externalNavigationHeldLedger(ledger,2)
    &&['rejoin-session','interrupt-session'].every(name=>{const matches=surface.actions.filter(item=>item.action===name&&item.index===p.session_rows.indexOf(row));return matches.length===1&&!matches[0].disabled&&matches[0].aria!=='true';}));}
export function externalPaletteActiveReady(sample,main){
  const {surface,sessionId,ledger}=sample??{},p=surface?.p,row=p?.session_rows?.[p.selected_session_index];
  return Boolean(externalNavigationMainPreserved(surface,main)&&p.navigation_admission_open&&p.overlay==='none'&&sessionId===main.sessionId
    &&row?.session_id===sessionId&&row.loaded_status==='active'&&row.active_turn_id&&externalNavigationHeldLedger(ledger,2));
}
export function externalPaletteTerminalPreserved(v,main){
  const rows=core(v.p);return v?.errors===0&&same(v.composerRunTarget,v.p?.run_target)&&v.p?.workspace_path===main.workspace
    &&v.p.run_target?.sessionId===main.sessionId&&v.p.draft_target?.sessionId===main.sessionId&&v.p.composer_commit_generation===main.commitGeneration
    &&v.p.run_status_key==='cancelled'&&!v.p.busy&&v.prompt===DRAFT&&v.promptEnabled&&v.p.post_run_refresh_pending===false
    &&main.history.every((old,index)=>same(rows[index],old))&&rows.filter(row=>row.kind==='user'&&row.body===EXTERNAL).length===1
    &&externalNavigationRowStopped(v,main.sessionId);
}
export function externalSessionId(stdout){
  const lines=stdout.split('\n');lines.pop();
  const rows=lines.filter(line=>line.trim()).map(line=>JSON.parse(line));
  const starts=rows.filter(row=>row.kind==='session_started');
  if(starts.length>1)throw fail('CLI created multiple sessions',starts);
  const id=starts[0]?.session_id;
  return /^[0-9A-Z]{26}$/.test(id??'')?id:null;
}
async function observe(cdp){const p=await invokeDesktopCommand(cdp,'desktop_state');return {p,...await cdp.evaluate(`(()=>{
  const prompt=document.getElementById('prompt'),composer=document.querySelector('section.composer');return {composerRunTarget:composer?.dataset.runTarget?JSON.parse(composer.dataset.runTarget):null,prompt:prompt?.value,promptEnabled:!!prompt&&!prompt.disabled&&!prompt.closest('[inert]'),
    busyStrip:document.querySelectorAll('section.run-strip [data-action="cancel-run"]').length,errors:document.querySelectorAll('.fatal,.ui-error-notice').length,
    sidebarRows:Array.from(document.querySelectorAll('aside.sidebar button[data-action="session"]')).map(n=>{const row=n.closest('.nav-row-wrap');return {focusKey:n.dataset.focusKey,activity:row?.dataset.taskActivityRow??null,stop:row?.querySelectorAll('[data-action="interrupt-session"]').length??-1,rejoin:row?.querySelectorAll('[data-action="rejoin-session"]').length??-1};}),
    actions:Array.from(document.querySelectorAll('aside.sidebar button[data-action][data-index]')).map(n=>({action:n.dataset.action,index:Number(n.dataset.index),focusKey:n.dataset.focusKey,disabled:n.disabled,aria:n.getAttribute('aria-disabled')}))};})()`)};}
export function externalSessionRowArguments(surface,sessionId){
  const p=surface.p,indices=p.session_rows.flatMap((row,index)=>row.session_id===sessionId?[index]:[]);
  if(indices.length!==1)throw fail('Exact typed session row unavailable',{sessionId,indices});
  const index=indices[0];return {row:p.session_rows[index],args:{index,expectedTarget:{workspacePath:p.workspace_path,ownerProjectId:p.project_rows[p.selected_project_index]?.project_id??null,ownerSessionId:p.session_rows[p.selected_session_index]?.session_id??null,rowId:sessionId}}};
}
function rowTarget(surface,sessionId,name){const p=surface.p,index=p.session_rows.findIndex(row=>row.session_id===sessionId),matches=surface.actions.filter(item=>item.action===name&&item.index===index);
  if(index<0||matches.length!==1||!matches[0].focusKey)throw fail('Exact session control unavailable',{sessionId,name,index,matches});
  return {locator:{selector:`button[data-focus-key=${JSON.stringify(matches[0].focusKey)}]`,identity:{tag:'BUTTON',action:name,focusKey:matches[0].focusKey}},row:p.session_rows[index],
    args:{index,expectedTarget:{workspacePath:p.workspace_path,ownerProjectId:p.project_rows[p.selected_project_index]?.project_id??null,ownerSessionId:p.session_rows[p.selected_session_index]?.session_id??null,rowId:sessionId}}};}
async function type(input,cdp,text,sink,owner){const ready=await waitForSemanticTargetSettlement({input,locator:PROMPT,label:'External navigation composer'});if(ready.value.classified.decision!=='pass')throw fail('Composer not settled',ready.value);
  const typed=await trustedInsert(input,PROMPT,text);await wait('Trusted text appears on the current Main',()=>observe(cdp),v=>v.prompt===text&&v.promptEnabled&&same(v.composerRunTarget,v.p.run_target));await sink.record('external-navigation-typed',{typed},{phase:'executing',owner});}
async function clickRow(input,cdp,surface,sessionId,target,sink,owner){
  const row=rowTarget(surface,sessionId,'session').locator;
  const ready=await waitForSemanticTargetSettlement({input,locator:row,label:'Exact external session row is mounted'});
  if(ready.value.classified.decision!=='pass')throw fail('External row not settled',ready.value);
  const start=(await input.snapshotProbe()).sequence;await input.hover(row);
  const hover=assertTrustedProbeSequence(await input.snapshotProbe(start),{afterSequence:start,expected:[{type:'pointermove',identity:row.identity,buttons:0}]});
  const revealed=await waitForSemanticTargetSettlement({input,locator:target,label:'Hovered external row action is visible and enabled'});
  if(revealed.value.classified.decision!=='pass')throw fail('External row action not settled',revealed.value);
  await sink.record('external-row-action-revealed',{row,target,hover,revealed:revealed.value},{phase:'executing',owner});
  await trustedClick(input,cdp,target,sink);
}
export function createExternalRejoinScenario(options={}){return createExternalNavigationScenario(options,'rejoin');}
export function createExternalSidebarStopScenario(options={}){return createExternalNavigationScenario(options,'sidebar-stop');}
export function createExternalPaletteRejoinScenario(options={}){return createExternalNavigationScenario(options,'rejoin','palette');}
function createExternalNavigationScenario(options,variant,entry='row'){
  if(Object.keys(options).some(key=>key!=='cliBinary'))throw new TypeError('External navigation accepts only cliBinary');
  const sameSession=entry==='palette',id=sameSession?`navigation.external-palette-${variant==='rejoin'?'rejoin':'stop'}`:`navigation.external-${variant}`,owner=`scenario:${id}`,stem=sameSession?`external-palette-${variant}`:`external-${variant}`;
  const state={provider:null,input:null,commands:null,external:null,externalResult:null,externalError:null,ownerReceipt:null,released:new Set(),failures:[],close:null};
  const releaseHeld=()=>{for(const [index,row] of responses(state.provider?.requestLedger??[]).entries()){if(row.response_phase==='held'&&!state.released.has(index)){state.provider.releaseResponse(index);state.released.add(index);}}};
  const settleExternal=async()=>{if(state.external){await state.external;await state.ownerReceipt;if(state.externalError)state.failures.push('external-process-cleanup:'+state.externalError.message);state.external=null;}};
  return Object.freeze({id,productOracle:'pass',manualGate:'pending',databaseRequired:true,
    async prepare({context,sink,phase}){
      state.provider=await startScriptedProvider(externalNavigationProviderOptions(sameSession));
      await prepareDesktopFixture({context,sink,phase,owner,configText:providerRestartFixtureConfig(state.provider.baseUrl),sentinelName:'E2E_EXTERNAL_NAV.txt',sentinelText:'Keep this owned workspace file.\n'});
      await mkdir(path.join(context.paths.workspace,'.git'));
    },
    async execute({context,driver:cdp,sink}){
      await acquireInteractiveShell({context,driver:cdp,sink},{evidenceOwner:owner,screenshotStem:'external-navigation-shell'});
      const input=state.input=new WebviewInput(cdp,{probeId:id});await input.installProbe();
      await type(input,cdp,MAIN,sink,owner);await trustedClick(input,cdp,action('send','section.composer'),sink);
      await wait('One original GUI request is held',()=>state.provider.requestLedger,v=>externalNavigationHeldLedger(v,1));
      state.provider.releaseResponse(0);state.released.add(0);
      const initial=await wait('Original GUI conversation settles canonically',()=>observe(cdp),v=>same(v.composerRunTarget,v.p.run_target)&&!v.p.busy&&v.p.run_status_key==='completed'&&v.promptEnabled&&v.p.can_submit
        &&v.p.composer_submit_mode==='new_request'&&v.p.post_run_refresh_pending===false&&v.p.run_target?.expectedState?.kind==='idle'&&v.p.pending_async_operations?.length===0
        &&v.p.transcript_rows.some(row=>row.row_kind==='assistant'&&row.body===REPLY));
      const main={workspace:initial.p.workspace_path,sessionId:initial.p.run_target.sessionId,history:core(initial.p),commitGeneration:initial.p.composer_commit_generation};
      await type(input,cdp,DRAFT,sink,owner);
      const stdoutPath=path.join(context.paths.logs,'external-root.stdout.jsonl'),stderrPath=path.join(context.paths.logs,'external-root.stderr.log');
      const executable=path.resolve(options.cliBinary??path.join(path.dirname(context.binary),'moyai.exe'));
      state.external=runWindowsExternalProcess({executionRoot:context.root,executable,args:['run','--dir',context.paths.workspace,...(sameSession?['--session',main.sessionId]:['--title',`external-${variant}`]),'--format','json',EXTERNAL],
        cwd:context.paths.workspace,env:{...process.env,MOYAI_CONFIG_PATH:context.paths.config_file,MOYAI_DATA_DIR:context.paths.data,RUST_BACKTRACE:'1'},
        stdoutPath,stderrPath,timeoutMs:60000,maxOutputBytes:2*1024*1024,label:`external-${variant}`,
        onOwner:externalOwner=>{state.ownerReceipt=sink.record('external-root-process-owner',externalOwner,{phase:'executing',owner}).catch(error=>{state.failures.push('external-owner-evidence:'+error.message);});}
      }).then(value=>{state.externalResult=value;return value;},error=>{state.externalError=error;return null;});
      await wait('The real CLI root reaches its independent model request',()=>state.provider.requestLedger,v=>externalNavigationHeldLedger(v,2));
      const sessionId=await wait('CLI JSON identifies one exact ordinary session',async()=>{if(state.externalError)throw state.externalError;try{return externalSessionId(await readFile(stdoutPath,'utf8'));}catch(error){if(error.code==='ENOENT')return null;throw error;}},v=>typeof v==='string');
      await trustedClick(input,cdp,action('refresh','aside.sidebar'),sink);
      const active=await wait('The exact external root row is actionable with original Main idle',async()=>({surface:await observe(cdp),sessionId,ledger:state.provider.requestLedger}),v=>sameSession?externalPaletteActiveReady(v,main):externalNavigationActiveReady(v,main));
      await captureScenarioScreenshot({cdp,sink,name:`${stem}-active-row`,owner});
      const name=variant==='rejoin'?'rejoin-session':'interrupt-session',command=variant==='rejoin'?'rejoin_session':'interrupt_session',target=sameSession?externalSessionRowArguments(active.surface,sessionId):rowTarget(active.surface,sessionId,name);
      if(variant==='sidebar-stop')target.args.expectedStopTarget=target.row.interrupt_target;
      state.commands=new DesktopCommandProbe(cdp,{probeId:id,commands:sameSession?[command,'interrupt_session']:[command]});await state.commands.install();
      if(sameSession){
        await input.keyDown('Control');try{await input.pressKey('k');}finally{await input.keyUp('Control');}
        await wait('Palette opens while selected external admission is active and Main is idle',()=>observe(cdp),v=>v.p.overlay==='command_palette');
        const search=byId('local-search','INPUT');
        const ready=await waitForSemanticTargetSettlement({input,locator:search,label:'Palette search visible before unavailable Stop query'});
        if(ready.value.classified.decision!=='pass')throw fail('Palette search not settled',ready.value);
        const typed=await trustedInsert(input,search,'interrupt-session');
        const absent=await wait('Unowned selected Stop is absent, without issuing a Stop command',async()=>({surface:await observe(cdp),commands:await state.commands.snapshot(),dom:await cdp.evaluate(`({query:document.querySelector('#local-search')?.value,count:document.querySelectorAll('[aria-labelledby="command-palette-dialog-title"] [data-action="interrupt-session"]').length})`)}),v=>
          v.surface.p.local_search_text==='interrupt-session'&&v.dom.query==='interrupt-session'&&v.dom.count===0&&v.commands.calls.length===0
          &&v.surface.p.session_rows[v.surface.p.selected_session_index]?.interrupt_target==null&&!v.surface.p.busy&&v.surface.prompt===DRAFT&&same(core(v.surface.p),main.history));
        await captureScenarioScreenshot({cdp,sink,name:'external-palette-unavailable-stop',owner});
        await sink.record('external-palette-stop-unavailable',{typed,absent},{phase:'executing',owner});
        const activated=await activateMainActionEntry({input,cdp,sink,owner,commands:state.commands,wait,trustedClick,assertTrustedTextInsertion,DesktopE2eError},name,'palette');
        await wait('One exact palette navigation command is issued',()=>state.commands.snapshot(),v=>v.calls.length===1);
        await sink.record('external-palette-entry',{activated},{phase:'executing',owner});
        if((await observe(cdp)).p.overlay==='command_palette'){await input.pressKey('Escape');await wait('Explicit palette dismissal exposes Main',()=>observe(cdp),v=>v.p.overlay==='none');}
      }else await clickRow(input,cdp,active.surface,sessionId,target.locator,sink,owner);
      let operated;
      if(variant==='rejoin'){
        operated=await wait('Rejoin binds the external running owner, without source draft leakage',()=>observe(cdp),v=>same(v.composerRunTarget,v.p.run_target)&&v.p.run_target?.sessionId===sessionId&&v.p.draft_target?.sessionId===sessionId
          &&v.p.busy&&v.p.run_status_key==='running'&&(sameSession?v.prompt===DRAFT&&v.p.composer_commit_generation===main.commitGeneration:v.prompt!==DRAFT)&&v.p.transcript_rows.some(row=>row.row_kind==='user'&&row.body===EXTERNAL)&&v.busyStrip===1&&v.errors===0);
        await captureScenarioScreenshot({cdp,sink,name:sameSession?'external-palette-rejoin-live-owner':'external-rejoin-live-owner',owner});
        await trustedClick(input,cdp,action('cancel-run','section.run-strip'),sink);
      }
      const terminal=await wait('User stop settles the real CLI before fixture response release',async()=>{if(state.externalError)throw state.externalError;return {surface:await observe(cdp),result:state.externalResult,ledger:state.provider.requestLedger};},v=>v.result?.outcome.root_exit_code===130
        &&v.result.job.descendant_zero===true&&externalNavigationHeldLedger(v.ledger,2)
        &&(variant==='rejoin'?v.surface.p.run_target?.sessionId===sessionId&&!v.surface.p.busy&&v.surface.p.run_status_key==='cancelled'&&v.surface.promptEnabled:sameSession?externalPaletteTerminalPreserved(v.surface,main):externalNavigationMainPreserved(v.surface,main)),30000);
      const proof=assertExactDesktopCommandSequence(await state.commands.snapshot(),{expected:[{command,args:target.args}]});await state.commands.remove();state.commands=null;
      await settleExternal();releaseHeld();
      await wait('Owned fixture has no active request',()=>state.provider.resourceObservation(),v=>v.active_request_count===0);
      if(variant==='rejoin'&&!sameSession)await trustedClick(input,cdp,rowTarget(await observe(cdp),main.sessionId,'session').locator,sink);
      const restored=await wait('Original conversation and draft return unchanged; exact external row is terminal',()=>observe(cdp),v=>sameSession?externalPaletteTerminalPreserved(v,main):externalNavigationMainPreserved(v,main)&&externalNavigationRowStopped(v,sessionId),30000);
      if(responses(state.provider.requestLedger).length!==2)throw fail('Navigation issued an extra model request',state.provider.requestLedger);
      await captureScenarioScreenshot({cdp,sink,name:`${stem}-source-restored`,owner});
      await sink.record('external-navigation-result',{variant,entry,sessionId,main,active,operated,terminal,restored,proof,process:state.externalResult},{phase:'executing',owner});
      return {acquisition:'pass',oracle:'pass',manual:'pending'};
    },
    async requestGracefulExit(cdp){
      if(state.commands){try{await state.commands.remove();}catch{state.failures.push('command-probe');}state.commands=null;}
      releaseHeld();await settleExternal();
      if(state.input){try{await state.input.cleanup();}catch{state.failures.push('input');}state.input=null;}
      return requestGracefulExit(cdp);
    },
    async quiesce(){releaseHeld();await settleExternal();if(state.input){try{await state.input.cleanup();}catch{state.failures.push('input');}state.input=null;}
      const provider=state.provider?await state.provider.close():{pass:true};state.close={pass:provider.pass&&!state.failures.length,provider,external:state.externalResult,failures:[...state.failures]};return {input:state.close.pass?'pass':'fail',resources:[{kind:'external-navigation',...state.close}]};},
    async cleanup(){return {input:state.close?.pass?'pass':'fail',resources:[]};}
  });
}
