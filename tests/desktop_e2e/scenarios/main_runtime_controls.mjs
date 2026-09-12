import path from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import { mkdir } from 'node:fs/promises';
import { DesktopE2eError } from '../core/execution.mjs';
import { WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion } from '../drivers/webview_input.mjs';
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from '../drivers/desktop_command_probe.mjs';
import { startScriptedProvider } from '../drivers/scripted_provider.mjs';
import { prepareDesktopFixture } from './fixture.mjs';
import { providerRestartFixtureConfig, relevantProviderHistory } from './provider_restart.mjs';
import { acquireInteractiveShell, requestGracefulExit } from './shell_baseline.mjs';
import { captureScenarioScreenshot } from './observations.mjs';
import { wait, trustedClick } from './hub_browser_enrollment.mjs';
import { RAIL_FIXTURE_TURNS, observeMainControls, replaceMainPrompt, submitMainPrompt,
  exerciseHeldSteer, exerciseLoadedHistoryRail, observeGoalQuery, stoppedSteerFailures } from './main_remaining_controls.mjs';
import {activateMainActionEntry,submitMainFromEntry,exerciseShortcutRows} from './main_action_entries.mjs';

const VARIANTS=Object.freeze({steer:'main.steer-controls',rail:'history.rail-controls',goal:'main.goal-query',shortcuts:'navigation.shortcut-row-controls',palette:'main.palette-run-controls'});
export function mainControlProviderOptions(kind) {
  if(kind==='steer') return {expectedPrompt:'Keep the Main steer control audit running.',responseBehavior:'hold_until_peer_close'};
  if(kind==='rail') return {turns:RAIL_FIXTURE_TURNS,orderedConversation:true,responseBehavior:'hold_until_release'};
  if(kind==='shortcuts') return {expectedPrompt:'Complete the Shortcuts Send control audit.',responseText:'SHORTCUT_SEND_OK',responseBehavior:'hold_until_release'};
  if(kind==='palette') return {expectedPrompt:'Keep the Palette Send control audit running.',responseBehavior:'hold_until_peer_close'};
  if(kind==='goal') return {expectedPrompt:'Complete the Goal query setup turn.',responseText:'GOAL_QUERY_SETUP_OK',responseBehavior:'hold_until_release'};
  throw new TypeError('Unknown Main control audit variant');
}
export function completedMainTurnFailures(value,turns,sessionId=null) {
  const failures=[],p=value?.surface?.p,history=relevantProviderHistory(p);
  if(!p||p.busy!==false||p.post_run_refresh_pending!==false||p.run_status_key!=='completed'
    ||p.composer_submit_mode!=='new_request'||p.can_submit!==true||p.run_target?.expectedState?.kind!=='idle') failures.push('terminal-not-settled');
  if(!p?.run_target?.sessionId||(sessionId!==null&&p.run_target.sessionId!==sessionId)) failures.push('session-owner');
  if(!isDeepStrictEqual(value?.surface?.composerRunTarget,p?.run_target)) failures.push('composer-owner-not-settled');
  if(history.completed_summaries!==turns.length||JSON.stringify(history.users)!==JSON.stringify(turns.map(t=>t.prompt))
    ||JSON.stringify(history.assistants)!==JSON.stringify(turns.map(t=>t.responseText))) failures.push('canonical-history');
  const ledger=value?.ledger;
  if(!Array.isArray(ledger)||ledger.length!==turns.length||ledger.some(r=>r.method!=='POST'||r.contract?.pass!==true||r.response_phase!=='completed'||r.response_status!==200)) failures.push('provider-ledger');
  if(value?.surface?.prompt?.value!==''||value?.surface?.prompt?.disabled!==false||value?.surface?.visibleErrors?.length!==0) failures.push('composer-or-error');
  return failures;
}
export function heldMainTurnFailures(value,index) {
  const failures=[],s=value?.surface,p=s?.p;
  if(p?.run_status_key!=='running'||p.run_target?.expectedState?.kind!=='turn'||p.composer_submit_mode!=='steer'||!p.run_target.sessionId) failures.push('live-owner');
  if(!isDeepStrictEqual(s?.composerRunTarget,p?.run_target)||s?.prompt?.value!==''||s?.prompt?.disabled!==false||s?.runStrip!==true) failures.push('live-dom-not-settled');
  if(value?.ledger?.length!==index+1||value.ledger[index]?.contract?.pass!==true||value.ledger[index]?.response_phase!=='held') failures.push('provider-not-held');
  return failures;
}
export function createMainSteerControlsScenario(){return createMainRuntimeControlScenario('steer');}
export function createHistoryRailControlsScenario(){return createMainRuntimeControlScenario('rail');}
export function createMainGoalQueryScenario(){return createMainRuntimeControlScenario('goal');}
export function createShortcutRowControlsScenario(){return createMainRuntimeControlScenario('shortcuts');}
export function createPaletteRunControlsScenario(){return createMainRuntimeControlScenario('palette');}
export function createMainRuntimeControlScenario(kind) {
  const options=mainControlProviderOptions(kind),id=VARIANTS[kind],owner=`scenario:${id}`;
  const state={provider:null,input:null,commands:null,failures:[],close:null};
  const fail=(message,evidence)=>new DesktopE2eError('product','main-runtime-control',message,evidence);
  async function releaseProbes() {
    if(state.commands){try{await state.commands.remove();}catch(error){state.failures.push({resource:'command-probe',message:String(error)});}state.commands=null;}
    if(state.input){try{await state.input.cleanup();}catch(error){state.failures.push({resource:'input-probe',message:String(error)});}state.input=null;}
  }
  return Object.freeze({id,productOracle:'pass',manualGate:'pending',databaseRequired:true,
    async prepare({context,sink,phase}) {
      state.provider=await startScriptedProvider(options);
      await prepareDesktopFixture({context,sink,phase,owner,configText:providerRestartFixtureConfig(state.provider.baseUrl),
        sentinelName:'E2E_MAIN_CONTROL_AUDIT.txt',sentinelText:`Isolated ${id} GUI control audit.\n`});
      await mkdir(path.join(context.paths.workspace,'.git'));
      await sink.record('main-control-provider-started',state.provider.resourceObservation(),{phase,owner});
    },
    async execute({context,driver:cdp,sink}) {
      await acquireInteractiveShell({context,driver:cdp,sink},{evidenceOwner:owner,screenshotStem:`${kind}-control-shell`});
      const input=state.input=new WebviewInput(cdp,{probeId:`${kind}-control-input`});
      const commands=state.commands=new DesktopCommandProbe(cdp,{probeId:`${kind}-control-command`,commands:['submit_prompt','refresh_desktop','show_command_palette','set_session_search_include_archived','toggle_access_mode','new_chat','cancel_run','export_transcript_markdown']});
      await input.installProbe(); await commands.install();
      const d={context,input,commands,cdp,sink,owner,provider:state.provider,DesktopE2eError,wait,trustedClick,
        assertTrustedProbeSequence,assertTrustedTextInsertion,assertExactDesktopCommandSequence,captureScenarioScreenshot};
      if(state.provider.requestLedger.length) throw fail('Provider was contacted before the first GUI Send',state.provider.requestLedger);
      const turns=kind==='rail'?RAIL_FIXTURE_TURNS:[{prompt:options.expectedPrompt,responseText:options.responseText}];
      let sessionId=null;
      for(let index=0;index<turns.length;index++) {
        await replaceMainPrompt(d,turns[index].prompt);
        if(kind==='shortcuts'||kind==='palette')await submitMainFromEntry(d,turns[index].prompt,kind);
        else await submitMainPrompt(d,turns[index].prompt);
        const held=await wait('Actual Main turn reaches the existing held provider',async()=>({surface:await observeMainControls(d),ledger:state.provider.requestLedger}),v=>
          heldMainTurnFailures(v,index).length===0);
        const heldSession=held.surface.p.run_target.sessionId;
        if(sessionId!==null&&sessionId!==heldSession) throw fail('Follow-up Send changed session',held);
        sessionId=heldSession;
        if(kind==='shortcuts'||kind==='palette'){
          await captureScenarioScreenshot({cdp,sink,name:`main-${kind}-send-accepted-list`,owner});
          await sink.record('main-entry-accepted-overlay',{kind,overlay:held.surface.p.overlay,
            observation:'Send accepted through an action list. The current overlay remains independently owned until explicitly dismissed.'},{phase:'executing',owner});
        }
        if(kind==='steer'||kind==='palette') {
          if(kind==='steer')await exerciseHeldSteer(d);
          if(kind==='palette')await activateMainActionEntry(d,'cancel-run','palette');
          else await trustedClick(input,cdp,{selector:'section.run-strip [data-action="cancel-run"]',identity:{tag:'BUTTON',action:'cancel-run'}},sink);
          const terminal=await wait('Real Stop settles both steered turn and actual DOM',async()=>({surface:await observeMainControls(d),ledger:state.provider.requestLedger}),v=>
            stoppedSteerFailures(v,sessionId).length===0);
          if(kind==='palette'&&terminal.surface.p.overlay==='command_palette'){
            await captureScenarioScreenshot({cdp,sink,name:'main-palette-stopped-list',owner});
            await input.pressKey('Escape');
            await wait('Explicit palette dismissal exposes stopped Main',async()=>({surface:await observeMainControls(d),ledger:state.provider.requestLedger}),v=>
              v.surface.p.overlay==='none'&&stoppedSteerFailures(v,sessionId).length===0);
          }
          await captureScenarioScreenshot({cdp,sink,name:kind==='palette'?'main-palette-stopped':'main-steer-stopped',owner});
          await sink.record(kind==='palette'?'main-palette-control-terminal':'main-steer-control-terminal',terminal,{phase:'executing',owner});
          return {acquisition:'pass',oracle:'pass',manual:'pending'};
        }
        state.provider.releaseResponse(index);
        const completed=await wait('GUI turn completes with exact canonical history',async()=>({surface:await observeMainControls(d),ledger:state.provider.requestLedger}),v=>
          completedMainTurnFailures(v,turns.slice(0,index+1),sessionId).length===0);
        await sink.record('main-control-setup-turn',completed,{phase:'executing',owner});
      }
      if(kind==='rail') await exerciseLoadedHistoryRail(d);
      else if(kind==='shortcuts')await exerciseShortcutRows(d);
      else {
        const evidence=await observeGoalQuery(d);
        if(evidence.classification!=='goal-query-visible-success') throw new DesktopE2eError('product',evidence.classification,
          'The Goal query requires an observable successful Goal result and unchanged canonical history.',evidence);
      }
      return {acquisition:'pass',oracle:'pass',manual:'pending'};
    },
    async requestGracefulExit(cdp){await releaseProbes();return requestGracefulExit(cdp);},
    async quiesce(){
      if(state.close)return structuredClone(state.close);
      await releaseProbes();
      const provider=state.provider?await state.provider.close():{pass:true};
      state.close={input:provider.pass&&(provider.forced_connection_count??0)===0&&!state.failures.length?'pass':'fail',resources:[{kind:'main-control-provider',provider,failures:[...state.failures]}]};
      return structuredClone(state.close);
    },
    async cleanup(){return {input:state.close?.input==='pass'?'pass':'fail',resources:[]};},
  });
}
