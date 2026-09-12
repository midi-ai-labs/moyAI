import { isDeepStrictEqual } from 'node:util';
import path from 'node:path';
import { DesktopE2eError } from '../core/execution.mjs';
import { waitForObservation } from '../core/deadline.mjs';
import { WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion } from '../drivers/webview_input.mjs';
import { DesktopCommandProbe } from '../drivers/desktop_command_probe.mjs';
import { prepareShellBaseline, acquireInteractiveShell, requestGracefulExit, quiesceShellBaseline } from './shell_baseline.mjs';
import { captureScenarioScreenshot } from './observations.mjs';
import { trustedClickProbeEvents } from './settings_preferences.mjs';

export const SHELL_MENU_ENTRY_PLAN = Object.freeze([
  {action:'new-chat',menu:'file',command:'new_chat',overlay:'none'},
  {action:'show-command-palette',menu:'edit',command:'show_command_palette',overlay:'command_palette',heading:'command-palette-dialog-title'},
  {action:'refresh',menu:'view',command:'refresh_desktop'},
  {action:'show-provider',menu:'view',command:'show_provider_editor',overlay:'provider',heading:'provider-dialog-title'},
  {action:'show-config',menu:'view',command:'show_config_editor',overlay:'config',heading:'config-dialog-title'},
  {action:'show-hub',menu:'view',overlay:'hub',heading:'hub-dialog-title'},
  {action:'show-mcp-history',menu:'view',overlay:'mcp_history',heading:'mcp-history-title'},
  {action:'show-shortcuts',menu:'help',command:'show_shortcuts',overlay:'shortcuts',heading:'shortcuts-dialog-title'},
  {action:'show-about',menu:'help',command:'show_about',overlay:'about',heading:'about-dialog-title'},
]);
export const SHELL_PALETTE_ENTRY_PLAN = Object.freeze([
  ...SHELL_MENU_ENTRY_PLAN,
  {action:'show-workspace-picker',command:'show_workspace_picker',overlay:'workspace',heading:'workspace-dialog-title'},
  {action:'toggle-artifact-pane',collapsed:true},
  {action:'toggle-artifact-pane',collapsed:false},
  {action:'toggle-session-archived-search',command:'set_session_search_include_archived',archived:true},
  {action:'toggle-session-archived-search',command:'set_session_search_include_archived',archived:false},
]);

export async function observeShellEntryControls(cdp) {
  return cdp.evaluate(`(async()=>{
    const projection=await window.__TAURI_INTERNALS__.invoke('desktop_state');
    await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
    const visible=el=>{const r=el?.getBoundingClientRect();return Boolean(el?.isConnected&&r.width>0&&r.height>0&&getComputedStyle(el).visibility!=='hidden');};
    const palette=document.querySelector('[aria-labelledby="command-palette-dialog-title"]');
    return {projection,prompt:document.querySelector('section.composer #prompt')?.value,
      collapsed:document.querySelector('aside.artifact-pane')?.classList.contains('collapsed'),
      headings:[...document.querySelectorAll('[role="dialog"] h2')].filter(visible).map(el=>el.id),
      visible_dialogs:[...document.querySelectorAll('[role="dialog"]')].filter(visible).map(el=>({label:el.getAttribute('aria-label'),labelledby:el.getAttribute('aria-labelledby'),modal:el.dataset.modal})),
      palette:visible(palette),search:palette?.querySelector('#local-search')?.value??null,
      palette_actions:[...(palette?.querySelectorAll('button[data-focus-key^="palette-action:"]')??[])].map(el=>({id:el.dataset.action,disabled:el.disabled})),
      menus:[...document.querySelectorAll('[data-titlebar-menu]')].filter(visible).map(el=>el.dataset.titlebarMenu),
      errors:[...document.querySelectorAll('.fatal,.ui-error-notice:not([hidden])')].filter(visible).map(el=>el.textContent)
    };
  })()`);
}
export function entryDestinationMatches(surface,plan,{newChatWorkspace=null}={}) {
  const p=surface.projection;
  const settled=p.navigation_loading===false && p.busy===false && p.background_mutation_pending===false
    && p.async_polling_required===false && p.post_run_refresh_pending===false && p.pending_async_operations?.length===0;
  const quickChat=plan.action!=='new-chat' || (typeof newChatWorkspace==='string'
    && path.normalize(p.workspace_path)===path.normalize(newChatWorkspace)
    && path.normalize(p.draft_target.workspacePath)===path.normalize(newChatWorkspace)
    && p.draft_target.sessionId===null && p.thread_empty===true && surface.prompt==='');
  return settled && quickChat && surface.errors.length===0 && (plan.overlay===undefined||surface.projection.overlay===plan.overlay)
    && (plan.heading===undefined||surface.headings.includes(plan.heading))
    && (plan.collapsed===undefined||surface.collapsed===plan.collapsed)
    && (plan.archived===undefined||surface.projection.session_search_include_archived===plan.archived);
}
export function emptyShellContentPreserved(before,after,{newChat=false}={}) {
  const transcriptPreserved=newChat
    ? [before,after].every(surface=>surface.projection.thread_empty===true
      && surface.projection.transcript_rows.every(row=>row.row_kind==='empty_placeholder'))
    : isDeepStrictEqual(before.projection.transcript_rows,after.projection.transcript_rows);
  return before.prompt===after.prompt && (newChat||before.projection.workspace_path===after.projection.workspace_path)
    && transcriptPreserved
    && after.projection.busy===false
    && (newChat||isDeepStrictEqual(before.projection.draft_target,after.projection.draft_target));
}
const failure=(message,evidence)=>new DesktopE2eError('product','shell-entry-control',message,evidence);
async function wait(cdp,label,accept){
  try{return(await waitForObservation({label,timeoutMs:15_000,pollMs:50,retrySampleErrors:false,sample:()=>observeShellEntryControls(cdp),accept})).value;}
  catch(error){if(error?.code==='observation-timeout'&&!error.evidence?.last_error)throw failure(label,error.evidence);throw error;}
}
async function click(input,locator){const start=(await input.snapshotProbe()).sequence;await input.click(locator);return assertTrustedProbeSequence(await input.snapshotProbe(start),{afterSequence:start,expected:trustedClickProbeEvents(locator)});}
async function chord(input,key){await input.keyDown('Control');try{await input.pressKey(key);}finally{await input.keyUp('Control');}}

function createEntryScenario(kind){
  const id=`navigation.${kind}-entry-controls`,owner=`scenario:${id}`,plan=kind==='menu'?SHELL_MENU_ENTRY_PLAN:SHELL_PALETTE_ENTRY_PLAN;
  let cleanup={input:'pass',resources:[]};
  return Object.freeze({id,productOracle:'pass',manualGate:'not_required',databaseRequired:true,prepare:prepareShellBaseline,requestGracefulExit,quiesce:quiesceShellBaseline,
    async cleanup(){return cleanup;},
    async execute({context,driver:cdp,sink}){
      await acquireInteractiveShell({context,driver:cdp,sink},{evidenceOwner:owner,screenshotStem:`${kind}-entry-shell`});
      const input=new WebviewInput(cdp,{probeId:id,maxProbeEvents:8192});
      const commands=new DesktopCommandProbe(cdp,{probeId:id,commands:[...new Set([...plan.map(row=>row.command).filter(Boolean),'submit_prompt','submit_side_chat','cancel_run','cancel_side_chat'])]});
      let primary=null;
      try{
        await input.installProbe();await commands.install();
        const initial=await observeShellEntryControls(cdp);
        if(initial.collapsed||initial.projection.session_search_include_archived)throw failure('Fixture must begin with expanded output and ordinary session search',initial);
        for(let index=0;index<plan.length;index++){
          const entry=plan[index],before=await observeShellEntryControls(cdp);
          let target;
          if(kind==='menu'){
            await click(input,{selector:`button[data-action="show-${entry.menu}-menu"]`,identity:{tag:'BUTTON',action:`show-${entry.menu}-menu`}});
            await wait(cdp,'Exact titlebar menu opens',state=>state.projection.overlay===`${entry.menu}_menu`&&state.menus.includes(entry.menu));
            target={selector:`[data-titlebar-menu="${entry.menu}"] button[data-action="${entry.action}"]`,identity:{tag:'BUTTON',action:entry.action}};
          }else{
            await chord(input,'k');
            await wait(cdp,'Palette opens',state=>state.projection.overlay==='command_palette'&&state.palette);
            const search={selector:'#local-search',identity:{tag:'INPUT',id:'local-search'}};
            await click(input,search);await chord(input,'a');
            const start=(await input.snapshotProbe()).sequence;await input.insertText(search,entry.action);
            assertTrustedTextInsertion(await input.snapshotProbe(start),{afterSequence:start,identity:search.identity,text:entry.action});
            await wait(cdp,'Palette contains exact requested action',state=>state.search===entry.action&&state.projection.local_search_text===entry.action&&state.palette_actions.some(row=>row.id===entry.action&&!row.disabled));
            target={selector:`[aria-labelledby="command-palette-dialog-title"] button[data-focus-key="palette-action:${entry.action}"]`,identity:{tag:'BUTTON',action:entry.action,focusKey:`palette-action:${entry.action}`}};
          }
          const commandStart=(await commands.snapshot()).sequence;
          const gesture=await click(input,target);
          const arrived=await wait(cdp,`${kind}:${entry.action} reaches its exact destination`,state=>entryDestinationMatches(state,entry,{newChatWorkspace:path.join(context.paths.data,'quick-chat-workspace')}));
          const calls=(await commands.snapshot(commandStart)).calls;
          if(entry.command&&calls.filter(call=>call.command===entry.command).length!==1)throw failure('One entry activation must issue exactly one associated command',{entry,calls});
          if(calls.some(call=>['submit_prompt','submit_side_chat','cancel_run','cancel_side_chat'].includes(call.command)))throw failure('Navigation entry must not start or stop a task',calls);
          if(!emptyShellContentPreserved(before,arrived,{newChat:entry.action==='new-chat'}))throw failure('Entry changed unrelated shell content',{entry,before,arrived});
          await captureScenarioScreenshot({cdp,sink,name:`${kind}-entry-${String(index+1).padStart(2,'0')}-${entry.action}`,owner});
          if(arrived.projection.overlay!=='none'){
            await input.pressKey('Escape');
            await wait(cdp,'Entry overlay closes back to shell',state=>state.projection.overlay==='none'&&!state.palette&&state.menus.length===0&&state.visible_dialogs.length===0&&state.errors.length===0);
          }
          await sink.record('shell-entry-verified',{kind,entry,gesture,commands:calls,destination:{overlay:arrived.projection.overlay,headings:arrived.headings,dialogs:arrived.visible_dialogs,collapsed:arrived.collapsed,archived:arrived.projection.session_search_include_archived,workspace:arrived.projection.workspace_path,draft_target:arrived.projection.draft_target,navigation_loading:arrived.projection.navigation_loading,pending_async_operations:arrived.projection.pending_async_operations},closed:true},{phase:'executing',owner});
        }
        await sink.record('shell-entry-limits',{kind,verified:plan.map(row=>row.action),not_run:['session-required Side Chat/session settings/archive/rollback/fork/rejoin entries','runtime-sensitive send/stop/steer/review entries','OS file picker/folder/exit entries','Provider model retrieval/apply/save palette entries'],scope:'Only individual activated entries in the declared idle empty-shell fixture, not every menu/palette action.'},{phase:'executing',owner});
        return {acquisition:'pass',oracle:'pass',manual:'not_required'};
      }catch(error){primary=error;try{await captureScenarioScreenshot({cdp,sink,name:`${kind}-entry-failure`,owner});}catch{}throw error;}
      finally{
        const errors=[];for(const [name,fn]of[['input',()=>input.cleanup()],['commands',()=>commands.remove()]]){try{cleanup.resources.push({kind:name,result:await fn()});}catch(error){errors.push(String(error));}}
        if(errors.length){cleanup={input:'fail',resources:[...cleanup.resources,{kind:'failures',errors}]};if(primary===null)throw new Error(errors.join('; '));}
      }
    },
  });
}
export function createMenuEntryControlsScenario(){return createEntryScenario('menu');}
export function createPaletteEntryControlsScenario(){return createEntryScenario('palette');}
