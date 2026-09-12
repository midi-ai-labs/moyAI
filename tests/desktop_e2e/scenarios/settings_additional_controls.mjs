import { readFile } from 'node:fs/promises';
import { createSettingsControlScenario, observeFieldControls, openGlobal, replace, wait, focusThroughTab, exerciseSession } from './settings_field_controls.mjs';
import { captureScenarioScreenshot } from './observations.mjs';
import { WebviewInput, assertTrustedProbeSequence } from '../drivers/webview_input.mjs';
import { acquireInteractiveShell } from './shell_baseline.mjs';
import { DesktopE2eError } from '../core/execution.mjs';

const GLOBAL='[role="dialog"][aria-labelledby="config-dialog-title"]';
const INITIAL='[data-surface="initial-setup"]';
const SESSION='[data-modal="session-settings"]';
const scopeSelector={global:GLOBAL,initial:INITIAL,session:SESSION};
const issue=(message,evidence)=>new DesktopE2eError('product','settings-additional-control',message,evidence);
const byAction=(scope,action)=>({selector:`${scope} button[data-action="${action}"]`,identity:{tag:'BUTTON',action}});
const byId=(tag,id)=>({selector:`${tag.toLowerCase()}#${id}`,identity:{tag,id}});
const fieldValue=(surface,key)=>surface.projection.config_fields.find(field=>field.key===key)?.value;

async function enter(input,cdp,target){
  await focusThroughTab(input,cdp,target);
  const start=(await input.snapshotProbe()).sequence;
  await input.pressKey('Enter');
  return assertTrustedProbeSequence(await input.snapshotProbe(start),{afterSequence:start,expected:[
    {type:'keydown',key:'Enter',identity:target.identity},
    {type:'click',identity:target.identity,button:0,buttons:0},
    {type:'keyup',key:'Enter'},
  ]});
}
async function photograph(args,name){await captureScenarioScreenshot({cdp:args.cdp,sink:args.sink,name,owner:args.owner});}
async function saveReopen(args,expected){
  const {input,cdp}=args;
  await enter(input,cdp,byAction(GLOBAL,'save-global-config'));
  await wait(cdp,'global','Global changes saved',s=>!s.dirty&&[...expected].every(([key,value])=>fieldValue(s,key)===value));
  await enter(input,cdp,byAction(GLOBAL,'close-overlay'));await openGlobal(input,cdp);
  await wait(cdp,'global','Saved settings reopen',s=>[...expected].every(([key,value])=>fieldValue(s,key)===value));
}
export function disclosureRoundTrip(before,after){
  return before.length===after.length&&before.every(row=>after.some(item=>item.key===row.key&&item.value===row.value));
}
async function observeDetails(cdp,scope){
  return cdp.evaluate(`(()=>{const panel=document.querySelector(${JSON.stringify(scopeSelector[scope])});return [...panel.querySelectorAll('details[data-details-key]')]
    .filter(node=>!node.closest('[hidden],[aria-hidden="true"]'))
    .map(node=>({key:node.dataset.detailsKey,open:node.open,parents:[...(function*(){for(let p=node.parentElement;p&&p!==panel;p=p.parentElement)if(p.tagName==='DETAILS')yield p.dataset.detailsKey;})()].reverse(),
      text:node.textContent.trim(),technical:node.classList.contains('settings-field-technical')}));})()`);
}
async function setDisclosure(input,cdp,scope,key,open){
  let item=(await observeDetails(cdp,scope)).find(row=>row.key===key);
  if(!item)throw issue('The declared disclosure is absent',{scope,key});
  for(const parent of item.parents)await setDisclosure(input,cdp,scope,parent,true);
  if(item.open===open)return;
  const target={selector:`${scopeSelector[scope]} details[data-details-key="${key}"] > summary`,identity:{tag:'DETAILS',detailsKey:key}};
  await focusThroughTab(input,cdp,target);
  const start=(await input.snapshotProbe()).sequence;await input.pressKey(' ');
  const probe=await input.snapshotProbe(start);
  if(probe.events.some(event=>!event.isTrusted))throw issue('Disclosure input was not trusted',{key,probe});
  await wait(cdp,scope,`${key} becomes ${open?'open':'closed'}`,asyncSurface=>asyncSurface.panel);
  item=(await observeDetails(cdp,scope)).find(row=>row.key===key);
  if(item?.open!==open)throw issue('Disclosure did not change state',{key,open,item,probe});
}
async function allDetails(args,scope){
  const baseline=await observeFieldControls(args.cdp,scope),rows=await observeDetails(args.cdp,scope);
  const values=baseline.controls.map(row=>({key:row.id||row.key,value:row.value}));
  for(const row of rows){
    await setDisclosure(args.input,args.cdp,scope,row.key,true);
    if(!(await observeDetails(args.cdp,scope)).find(item=>item.key===row.key)?.text)throw issue('Expanded disclosure is empty',row);
    await setDisclosure(args.input,args.cdp,scope,row.key,false);
    const after=await observeFieldControls(args.cdp,scope);
    if(!disclosureRoundTrip(values,after.controls.map(item=>({key:item.id||item.key,value:item.value}))))throw issue('Disclosure changed a draft value',{row,after});
    await args.sink.record('settings-disclosure-roundtrip',{scope,key:row.key,technical:row.technical,opened:true,closed:true,draft_unchanged:true},{phase:'executing',owner:args.owner});
  }
  for(const row of [...rows].reverse())await setDisclosure(args.input,args.cdp,scope,row.key,row.open);
  await photograph(args,`settings-additional-${scope}${scope==='initial'?`-${baseline.step}`:''}-disclosures`);
}
async function selectCatalog(args,scope,id,key){
  const target=byId('SELECT',id),{input,cdp}=args;
  const ready=await wait(cdp,scope,`${id} has a real current catalog`,s=>s.controls.some(row=>row.id===id&&!row.disabled&&row.options.includes(args.provider.modelId)));
  const options=ready.controls.find(row=>row.id===id).options.filter(Boolean);
  const current=ready.controls.find(row=>row.key===key&&row.tag==='INPUT')?.value;
  const labels=await cdp.evaluate(`(()=>[...document.querySelector(${JSON.stringify(target.selector)}).options].map(o=>({value:o.value,label:o.textContent})))()`);
  const currentSuffix=id==='side-chat-model'?'（現在の設定）':id==='initial-setup-model-select'?'（現在の入力）':null;
  const allowedCurrent=currentSuffix!==null&&labels.some(row=>row.value===current&&row.label===`${current}${currentSuffix}`);
  if(options.some(value=>value!==args.provider.modelId&&!(allowedCurrent&&value===current)))throw issue('Catalog contains an unadvertised model without an explicit current-setting label',{options,labels,current,model:args.provider.modelId});
  await focusThroughTab(input,cdp,target);
  await input.pressKey('End');await input.pressKey('Tab');
  const chosen=await wait(cdp,scope,`${id} selection updates the model draft`,s=>s.controls.some(row=>row.key===key&&row.tag==='INPUT'&&row.value===args.provider.modelId));
  await args.sink.record('settings-catalog-select',{scope,id,key,options,labels,selected:args.provider.modelId,owner:chosen.projection.config_target},{phase:'executing',owner:args.owner});
}

async function globalAdditional(args){
  const {input,cdp,context,sink,owner,provider}=args;
  await openGlobal(input,cdp);await allDetails(args,'global');
  await replace(input,cdp,'global','model.model','manual-before-catalog');
  await saveReopen(args,new Map([['model.model','manual-before-catalog']]));
  await enter(input,cdp,byAction(GLOBAL,'load-provider-models'));
  await selectCatalog(args,'global','main-provider-model','model.model');
  await saveReopen(args,new Map([['model.model',provider.modelId]]));
  await replace(input,cdp,'global','side_chat.base_url',provider.baseUrl);
  await replace(input,cdp,'global','side_chat.model','side-manual-before-catalog');
  await saveReopen(args,new Map([['side_chat.model','side-manual-before-catalog']]));
  await enter(input,cdp,byAction(GLOBAL,'load-side-chat-models'));
  await selectCatalog(args,'global','side-chat-model','side_chat.model');
  await saveReopen(args,new Map([['model.model',provider.modelId],['side_chat.model',provider.modelId]]));
  await photograph(args,'settings-additional-models');
  const range=byId('INPUT','opacity-input');await focusThroughTab(input,cdp,range);
  for(const [key,value] of [['Home',50],['End',100],['ArrowLeft',99],['ArrowLeft',98],['ArrowRight',99]]){
    await input.pressKey(key);
    const observed=await wait(cdp,'global','Global opacity changes and keeps focus',s=>s.projection.window_opacity_percent===value);
    const focused=await cdp.evaluate(`document.activeElement?.id==='opacity-input'`);
    if(!focused)throw issue('Global opacity lost continuous keyboard focus',{value,observed});
    await sink.record('settings-global-opacity',{value,focused},{phase:'executing',owner});
  }
  await input.pressKey('Tab');await photograph(args,'settings-additional-opacity');
  await enter(input,cdp,byAction(GLOBAL,'close-overlay'));await openGlobal(input,cdp);
  await wait(cdp,'global','Global opacity survives reopen',s=>s.projection.window_opacity_percent===99);
  await enter(input,cdp,byAction(GLOBAL,'close-overlay'));
}

export function temporaryConfigViewMatches(surface,original,effective){
  return !surface.dirty&&[...original].every(([key,value])=>fieldValue(surface,key)===value)
    &&surface.projection.model_label===effective.get('model.model')
    &&surface.projection.provider_effective_context_window===effective.get('model.context_window');
}
async function temporaryApply(args){
  const {input,cdp,context,sink,owner}=args;
  await openGlobal(input,cdp);
  const range=byId('INPUT','opacity-input');await focusThroughTab(input,cdp,range);
  await input.pressKey('End');await input.pressKey('ArrowLeft');await input.pressKey('Tab');
  await wait(cdp,'global','Separate Desktop opacity is committed before restart',s=>s.projection.window_opacity_percent===99);
  const fileBefore=await readFile(context.paths.config_file);
  const before=await observeFieldControls(cdp,'global');
  const original=new Map([['model.context_window',fieldValue(before,'model.context_window')],['model.model',fieldValue(before,'model.model')]]);
  const expected=new Map([['model.context_window','32768'],['model.model','temporary-settings-projection']]);
  for(const [key,value] of expected)await replace(input,cdp,'global',key,value);
  await enter(input,cdp,byAction(GLOBAL,'apply-session-config'));
  await wait(cdp,'global','Temporary effective config applies while Global fields retain saved defaults',s=>temporaryConfigViewMatches(s,original,expected));
  if(!(await readFile(context.paths.config_file)).equals(fileBefore))throw issue('Temporary Apply changed global config bytes',{});
  await enter(input,cdp,byAction(GLOBAL,'close-overlay'));await openGlobal(input,cdp);
  const reopened=await wait(cdp,'global','Temporary effective values survive dialog reopen',s=>temporaryConfigViewMatches(s,original,expected));
  await photograph(args,'settings-additional-temporary-apply');
  await sink.record('settings-temporary-applied',{values:[...expected],original:[...original],file_unchanged:true,config_target:reopened.projection.config_target},{phase:'executing',owner});
  await enter(input,cdp,byAction(GLOBAL,'close-overlay'));
  // Exact restart uses the common host; dispose the old-generation probes first.
  await input.cleanup();await args.commands.remove();
  const restarted=await args.host.restart({context,scenario:args.scenario,sink,driver:cdp,phase:'executing'});
  await acquireInteractiveShell({context,driver:restarted.driver,sink},{evidenceOwner:owner,screenshotStem:'settings-additional-restarted'});
  const nextInput=new WebviewInput(restarted.driver,{probeId:'settings-additional-restarted'});await nextInput.installProbe();
  try{
    await openGlobal(nextInput,restarted.driver);
    const restored=await wait(restarted.driver,'global','Restart restores saved effective defaults and separate opacity',s=>s.projection.window_opacity_percent===99&&temporaryConfigViewMatches(s,original,original));
    if(!(await readFile(context.paths.config_file)).equals(fileBefore))throw issue('Restart mutated saved config bytes',{});
    await captureScenarioScreenshot({cdp:restarted.driver,sink,name:'settings-additional-restored',owner});
    await sink.record('settings-temporary-restored',{original:[...original],file_unchanged:true,opacity:restored.projection.window_opacity_percent,restart:restarted.restart},{phase:'executing',owner});
    await enter(nextInput,restarted.driver,byAction(GLOBAL,'close-overlay'));
  }finally{await nextInput.cleanup();}
}

async function initialAdditional(args){
  const {input,cdp,provider,sink,owner}=args;
  await wait(cdp,'initial','Initial Start ready',s=>s.step==='start');
  await enter(input,cdp,byAction(INITIAL,'initial-setup-next'));
  await replace(input,cdp,'initial','model.base_url',provider.baseUrl);
  await enter(input,cdp,byAction(INITIAL,'initial-setup-next'));
  await replace(input,cdp,'initial','model.model','initial-manual-before-catalog');
  await enter(input,cdp,byAction(INITIAL,'load-provider-models'));
  await selectCatalog(args,'initial','initial-setup-model-select','model.model');
  for(const step of ['model','permissions','tools','finish']){
    await wait(cdp,'initial',`Initial ${step} ready`,s=>s.step===step);
    await allDetails(args,'initial');
    if(step==='finish')break;
    await enter(input,cdp,byAction(INITIAL,'initial-setup-next'));
  }
  await enter(input,cdp,byAction(INITIAL,'finish-initial-setup'));
  await wait(cdp,'initial','Initial setup finishes',s=>!s.panel&&!s.projection.startup.initial_setup_required);
  await openGlobal(input,cdp);
  const saved=await wait(cdp,'global','Initial model candidate was persisted',s=>fieldValue(s,'model.model')===provider.modelId);
  await photograph(args,'settings-additional-initial-catalog-saved');
  await sink.record('settings-initial-catalog-persisted',{model:provider.modelId,config_target:saved.projection.config_target},{phase:'executing',owner});
  await enter(input,cdp,byAction(GLOBAL,'close-overlay'));
}
async function sessionAdditional(args){
  await exerciseSession(args);
  const {input,cdp,sink,owner,context}=args;
  const show={selector:'button[data-action="show-session-settings"][data-session-settings-trigger="model"]',identity:{tag:'BUTTON',action:'show-session-settings',sessionSettingsTrigger:'model'}};
  await enter(input,cdp,show);
  const baseline=await wait(cdp,'session','Session settings ready for dirty close',s=>s.panel);
  const file=await readFile(context.paths.config_file),original=baseline.controls.find(row=>row.key==='context-window').value;
  await replace(input,cdp,'session','context-window','12345');
  await enter(input,cdp,byAction(SESSION,'close-overlay'));
  await enter(input,cdp,byAction('[role="alertdialog"]','cancel-local-confirm'));
  await wait(cdp,'session','Cancel preserves dirty session draft',s=>s.controls.find(row=>row.key==='context-window')?.value==='12345');
  await enter(input,cdp,byAction(SESSION,'close-overlay'));
  await photograph(args,'settings-additional-session-dirty-confirm');
  await enter(input,cdp,byAction('[role="alertdialog"]','confirm-session-settings-discard-close'));
  await wait(cdp,'session','Discard confirmation closes Session Settings',s=>!s.panel&&s.projection.overlay==='none');
  await enter(input,cdp,show);
  await wait(cdp,'session','Discarded session draft is gone on reopen',s=>s.controls.find(row=>row.key==='context-window')?.value===original);
  if(!(await readFile(context.paths.config_file)).equals(file))throw issue('Session discard wrote global config',{});
  await photograph(args,'settings-additional-session-restored');
  await sink.record('settings-session-discard-close',{cancel_preserved:true,discard_closed:true,reopen_value:original,file_unchanged:true},{phase:'executing',owner});
  await enter(input,cdp,byAction(SESSION,'close-overlay'));
}
export function createGlobalAdditionalControlsScenario(){return createSettingsControlScenario('global',{id:'settings.additional-controls',catalogRequests:true,exercise:globalAdditional});}
export function createInitialAdditionalControlsScenario(){return createSettingsControlScenario('initial',{id:'settings.initial-additional-controls',catalogRequests:true,exercise:initialAdditional});}
export function createSessionDiscardCloseScenario(){return createSettingsControlScenario('session',{id:'settings.session-discard-close',exercise:sessionAdditional});}
export function createTemporaryApplyControlsScenario(){return createSettingsControlScenario('global',{id:'settings.temporary-apply-controls',exercise:temporaryApply});}
