import { isDeepStrictEqual } from 'node:util';
import { DesktopE2eError } from '../core/execution.mjs';
import { waitForObservation } from '../core/deadline.mjs';
import { WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion } from '../drivers/webview_input.mjs';
import { DesktopCommandProbe } from '../drivers/desktop_command_probe.mjs';
import { prepareShellBaseline, acquireInteractiveShell, requestGracefulExit, quiesceShellBaseline } from './shell_baseline.mjs';
import { captureScenarioScreenshot } from './observations.mjs';
import { trustedClickProbeEvents } from './settings_preferences.mjs';

const ID = 'navigation.modal-keyboard-controls';
const OWNER = `scenario:${ID}`;
const DRAFT = 'Modal keyboard preserves this unsent draft 日本語';
export const MODAL_KEYBOARD_PLAN = Object.freeze([
  { overlay:'workspace', action:'show-workspace-picker', heading:'workspace-dialog-title', closes:[] },
  { overlay:'command_palette', action:'show-command-palette', heading:'command-palette-dialog-title', closes:[] },
  { overlay:'shortcuts', action:'show-shortcuts', heading:'shortcuts-dialog-title', closes:['button[data-action="close-overlay"]'] },
  { overlay:'about', action:'show-about', heading:'about-dialog-title', closes:['.modal-header button[data-action="close-overlay"]','.modal-actions button[data-action="close-overlay"]'] },
  { overlay:'hub', action:'show-hub', heading:'hub-dialog-title', closes:['.hub-modal-header button[data-action="close-overlay"]','.hub-modal-footer button[data-action="close-overlay"]'] },
  { overlay:'provider', action:'show-provider', heading:'provider-dialog-title', closes:['button[data-action="close-overlay"]'] },
  { overlay:'config', action:'show-config', heading:'config-dialog-title', closes:['button[data-action="close-overlay"]'] },
]);
const GUARDED_COMMANDS = ['new_chat','show_command_palette','toggle_access_mode','submit_prompt','submit_side_chat','cancel_run','cancel_side_chat','export_transcript_markdown','set_session_search_include_archived'];
export const MODAL_BLOCKED_KEYS = Object.freeze([
  {key:'n',modifier:'Control'}, {key:'k',modifier:'Control'}, {key:'F8'},
  {key:'Enter',modifier:'Control'}, {key:'F9'}, {key:'i',modifier:'Control'},
]);
const failure=(message,evidence)=>new DesktopE2eError('product','modal-keyboard-controls',message,evidence);

export function modalDisclosureExposesTarget(target) {
  for(let ancestor=target?.parentElement;ancestor;ancestor=ancestor.parentElement){
    if(ancestor.tagName==='DETAILS'&&!ancestor.open){
      const summary=Array.from(ancestor.children).find(child=>child.tagName==='SUMMARY');
      if(!summary?.contains(target))return false;
    }
  }
  return true;
}

export async function observeModalKeyboard(cdp, plan) {
  return cdp.evaluate(`(async()=>{
    const p=await window.__TAURI_INTERNALS__.invoke('desktop_state');
    await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
    const disclosureExposesTarget=(${modalDisclosureExposesTarget.toString()});
    const shown=e=>Boolean(e?.isConnected&&e.getClientRects().length&&disclosureExposesTarget(e)&&!e.closest('[hidden],[aria-hidden="true"],[inert]')&&getComputedStyle(e).visibility!=='hidden');
    const dialogs=[...document.querySelectorAll('[role="dialog"],[role="alertdialog"]')].filter(shown);
    const d=document.querySelector('[aria-labelledby="'+${JSON.stringify(plan.heading)}+'"]');
    const controls=d?[...d.querySelectorAll('button,input,select,textarea,a[href],summary,[contenteditable]:not([contenteditable="false"]),[tabindex]')].filter(e=>shown(e)&&!e.matches(':disabled')&&e.getAttribute('aria-disabled')!=='true'&&e.tabIndex>=0):[];
    const describe=e=>e?{tag:e.tagName,id:e.id,action:e.dataset.action??null,focusKey:e.dataset.focusKey??null,configKey:e.dataset.configKey??null,href:e.getAttribute('href'),text:(e.getAttribute('aria-label')??e.textContent??'').trim().slice(0,90)}:null;
    return {p,prompt:document.querySelector('section.composer #prompt')?.value,dialogShown:shown(d),
      dialogs:dialogs.map(e=>e.getAttribute('aria-labelledby')),inDialog:Boolean(d?.contains(document.activeElement)),dialogFocused:document.activeElement===d,
      focus:describe(document.activeElement),focusIndex:controls.indexOf(document.activeElement),targets:controls.map(describe),
      hubReady:${JSON.stringify(plan.overlay)}!=='hub'||Boolean(d?.querySelector('#hub-tab-devices[aria-pressed="true"]')
        &&shown(d?.querySelector('#device-network-import:not(:disabled)'))&&shown(d?.querySelector('#device-network-refresh:not(:disabled)'))
        &&d?.querySelector('[data-settings-passive="device-network-receiver-status"]')?.textContent.trim()==='受付 OFF · 停止中'),
      closeTargets:${JSON.stringify(plan.closes)}.map(selector=>{const matches=d?[...d.querySelectorAll(selector)]:[];return {selector,count:matches.length,index:controls.indexOf(matches[0])};}),
      values:d?[...d.querySelectorAll('input,textarea,select')].map(e=>({id:e.id,key:e.dataset.configKey??null,value:e.value,checked:e.checked??null})):[],
      errors:[...document.querySelectorAll('.fatal,.ui-error-notice')].filter(shown).map(e=>e.textContent)};
  })()`);
}
export function modalContentPreserved(before, after) {
  return before.prompt===after.prompt && before.p.workspace_path===after.p.workspace_path
    && isDeepStrictEqual(before.p.draft_target,after.p.draft_target)
    && isDeepStrictEqual(before.p.transcript_rows,after.p.transcript_rows)
    && isDeepStrictEqual(before.p.run_target,after.p.run_target)
    && before.p.access_label===after.p.access_label
    && isDeepStrictEqual(before.p.access_target,after.p.access_target)
    && before.p.session_settings.access_mode===after.p.session_settings.access_mode
    && before.p.session_search_include_archived===after.p.session_search_include_archived
    && after.p.busy===false && after.p.navigation_loading===false && after.errors.length===0;
}
export function modalFocusStep(before, after, backwards=false) {
  if(!before.inDialog||!after.inDialog||before.focusIndex<0||before.targets.length===0
    ||!isDeepStrictEqual(before.targets,after.targets))return false;
  const expected=(before.focusIndex+(backwards?-1:1)+before.targets.length)%before.targets.length;
  return after.focusIndex===expected;
}
async function wait(cdp,plan,label,accept){
  try{return(await waitForObservation({label,timeoutMs:12_000,pollMs:50,retrySampleErrors:false,sample:()=>observeModalKeyboard(cdp,plan),accept})).value;}
  catch(error){if(error?.code==='observation-timeout'&&!error.evidence?.last_error)throw failure(label,error.evidence);throw error;}
}
async function click(input,locator){const start=(await input.snapshotProbe()).sequence;await input.click(locator);return assertTrustedProbeSequence(await input.snapshotProbe(start),{afterSequence:start,expected:trustedClickProbeEvents(locator)});}
async function key(input,{key,modifier}){
  const start=(await input.snapshotProbe()).sequence;
  if(modifier)await input.keyDown(modifier);
  try{await input.pressKey(key);}finally{if(modifier)await input.keyUp(modifier);}
  const expected=[...(modifier?[{type:'keydown',key:modifier}]:[]),{type:'keydown',key},{type:'keyup',key},...(modifier?[{type:'keyup',key:modifier}]:[])];
  return assertTrustedProbeSequence(await input.snapshotProbe(start),{afterSequence:start,expected});
}
async function enter(input,cdp,plan){
  await key(input,{key:'k',modifier:'Control'});
  const palette=MODAL_KEYBOARD_PLAN[1];
  await wait(cdp,palette,'Palette receives focus',s=>s.p.overlay==='command_palette'&&s.dialogShown&&s.inDialog);
  if(plan.overlay==='command_palette')return;
  const search={selector:'#local-search',identity:{tag:'INPUT',id:'local-search'}};
  await click(input,search);await key(input,{key:'a',modifier:'Control'});
  const seq=(await input.snapshotProbe()).sequence;await input.insertText(search,plan.action);
  assertTrustedTextInsertion(await input.snapshotProbe(seq),{afterSequence:seq,identity:search.identity,text:plan.action});
  await wait(cdp,palette,'Exact palette search settles before selection',s=>s.p.local_search_text===plan.action&&s.targets.some(t=>t.focusKey===`palette-action:${plan.action}`));
  await click(input,{selector:`[aria-labelledby="command-palette-dialog-title"] button[data-focus-key="palette-action:${plan.action}"]`,identity:{tag:'BUTTON',action:plan.action,focusKey:`palette-action:${plan.action}`}});
  let lastTargets=null,stableSamples=0;
  const ready=await wait(cdp,plan,'Exact requested modal is loaded with a stable enabled focus set',s=>{
    if(s.p.overlay!==plan.overlay||!s.dialogShown||!s.inDialog||(s.focusIndex<0&&!s.dialogFocused)||!s.hubReady){lastTargets=null;stableSamples=0;return false;}
    stableSamples=isDeepStrictEqual(lastTargets,s.targets)?stableSamples+1:1;lastTargets=s.targets;
    return stableSamples>=3;
  });
  if(ready.dialogFocused){
    await key(input,{key:'Tab'});
    await wait(cdp,plan,'Tab enters the first available control from the dialog fallback owner',s=>s.inDialog&&s.focusIndex===0&&isDeepStrictEqual(s.targets,ready.targets));
  }
}

export function createModalKeyboardControlsScenario(){
  let cleanup={input:'pass',resources:[]};
  return Object.freeze({id:ID,productOracle:'pass',manualGate:'pending',databaseRequired:true,
    prepare:prepareShellBaseline,requestGracefulExit,quiesce:quiesceShellBaseline,async cleanup(){return cleanup;},
    async execute({context,driver:cdp,sink}){
      await acquireInteractiveShell({context,driver:cdp,sink},{evidenceOwner:OWNER,screenshotStem:'modal-keyboard-shell'});
      const input=new WebviewInput(cdp,{probeId:ID,maxProbeEvents:16384});
      const commands=new DesktopCommandProbe(cdp,{probeId:ID,commands:GUARDED_COMMANDS});
      let primary=null;
      try{
        await input.installProbe();await commands.install();
        const prompt={selector:'section.composer #prompt',identity:{tag:'TEXTAREA',id:'prompt'}};
        await click(input,prompt);
        const textStart=(await input.snapshotProbe()).sequence;await input.insertText(prompt,DRAFT);
        assertTrustedTextInsertion(await input.snapshotProbe(textStart),{afterSequence:textStart,identity:prompt.identity,text:DRAFT});
        const baseline=await observeModalKeyboard(cdp,MODAL_KEYBOARD_PLAN[0]);
        if(baseline.prompt!==DRAFT)throw failure('Fixture local draft was not retained',baseline);
        for(const plan of MODAL_KEYBOARD_PLAN){
          await enter(input,cdp,plan);
          const opened=await observeModalKeyboard(cdp,plan);
          if(!modalContentPreserved(baseline,opened)||opened.dialogs.length!==1||opened.targets.length>160||opened.targets.length===0)
            throw failure('Modal opens with one bounded focus owner and unchanged shell content',{plan,opened});
          const cycles=[];
          for(const backwards of [false,true]){
            const start=await observeModalKeyboard(cdp,plan);let before=start;const steps=[];
            for(let n=0;n<start.targets.length;n++){
              const gesture=await key(input,{key:'Tab',...(backwards?{modifier:'Shift'}:{})});
              const after=await observeModalKeyboard(cdp,plan);
              if(!modalFocusStep(before,after,backwards)||!modalContentPreserved(baseline,after))
                throw failure('Tab must move once within the current modal focus cycle',{plan,backwards,before,after,gesture});
              steps.push({from:before.focusIndex,to:after.focusIndex,gesture});before=after;
            }
            if(before.focusIndex!==start.focusIndex)throw failure('Full focus traversal failed to wrap', {plan,backwards,start,before});
            cycles.push({backwards,targets:start.targets,steps});
          }
          const beforeKeys=await observeModalKeyboard(cdp,plan),commandStart=(await commands.snapshot()).sequence,blocked=[];
          for(const shortcut of MODAL_BLOCKED_KEYS){
            const gesture=await key(input,shortcut),after=await observeModalKeyboard(cdp,plan);
            if(after.p.overlay!==plan.overlay||!after.inDialog||after.focusIndex!==beforeKeys.focusIndex
              ||!modalContentPreserved(baseline,after)||!isDeepStrictEqual(beforeKeys.values,after.values))
              throw failure('Background shortcut escaped or mutated the current modal',{plan,shortcut,beforeKeys,after,gesture});
            blocked.push({shortcut,gesture});
          }
          const calls=(await commands.snapshot(commandStart)).calls;
          if(calls.length)throw failure('Modal shortcuts issued a forbidden background command',{plan,calls});
          await captureScenarioScreenshot({cdp,sink,name:`modal-keyboard-${plan.overlay}`,owner:OWNER});
          await key(input,{key:'Escape'});
          await wait(cdp,plan,'Escape closes the modal and preserves the draft',s=>s.p.overlay==='none'&&s.dialogs.length===0&&modalContentPreserved(baseline,s));
          const closes=[];
          for(const selector of plan.closes){
            await enter(input,cdp,plan);let current=await observeModalKeyboard(cdp,plan);
            const target=current.closeTargets.find(t=>t.selector===selector);
            if(target?.count!==1||target.index<0)throw failure('Declared explicit Close is not an enabled exact control',{plan,selector,current});
            for(let step=0;current.focusIndex!==target.index&&step<current.targets.length;step++){
              const before=current;await key(input,{key:'Tab'});current=await observeModalKeyboard(cdp,plan);
              if(!modalFocusStep(before,current))throw failure('Explicit Close cannot be reached by Tab',{plan,selector,before,current});
            }
            if(current.focusIndex!==target.index)throw failure('Explicit Close focus was not acquired',{plan,selector,current});
            const gesture=await key(input,{key:'Enter'});
            await wait(cdp,plan,'Explicit Close returns to the shell without changing its draft',s=>s.p.overlay==='none'&&s.dialogs.length===0&&modalContentPreserved(baseline,s));
            closes.push({selector,gesture});
          }
          await sink.record('modal-keyboard-verified',{plan,cycles,blocked,commands:calls,escape:true,closes},{phase:'executing',owner:OWNER});
        }
        await sink.record('modal-keyboard-scope',{dialogs:MODAL_KEYBOARD_PLAN.map(p=>p.overlay),standaloneCommands:'No separate current commands dialog; command_palette tested',noExplicitClose:['workspace','command_palette'],excluded:['F9 suppression with an export-enabled history behind the dialog (this fixture has an empty transcript)','backdrop pointer behavior','dirty discard confirmations','Initial Setup','permission decisions','Session Settings','Prompt Review','MCP History (separate actual case)','physical OS keyboard and IME'],input:'trusted WebView keyboard and pointer; no DOM mutation or synthetic DOM events'},{phase:'executing',owner:OWNER});
        return {acquisition:'pass',oracle:'pass',manual:'pending'};
      }catch(error){primary=error;try{await captureScenarioScreenshot({cdp,sink,name:'modal-keyboard-failure',owner:OWNER});}catch{}throw error;}
      finally{
        const errors=[];for(const [kind,action]of[['input',()=>input.cleanup()],['commands',()=>commands.remove()]]){try{cleanup.resources.push({kind,result:await action()});}catch(error){errors.push(String(error));}}
        if(errors.length){cleanup={input:'fail',resources:[...cleanup.resources,{kind:'failures',errors}]};if(primary===null)throw new Error(errors.join('; '));}
      }
    },
  });
}
