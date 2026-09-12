import {isDeepStrictEqual} from 'node:util';
import {DesktopE2eError} from '../core/execution.mjs';
import {DesktopCommandProbe,assertExactDesktopCommandSequence} from '../drivers/desktop_command_probe.mjs';
import {byId,action,wait,trustedClick} from './hub_browser_enrollment.mjs';
import {captureScenarioScreenshot,invokeDesktopCommand} from './observations.mjs';

const fail=(message,evidence)=>new DesktopE2eError('product','receiver-connection-controls-mismatch',message,evidence);
const expectedDiagnosticStages={
  hub:[['ローカルIPv4','pass'],['HubへのTCP接続','pass'],['HubのTLS・端末認可','pass'],['Hub証明書の自動更新','pass'],['Gateway証明書の自動更新','pass'],['モデルGatewayへのTLS接続','pass']],
  receiver:[['ローカルIPv4','pass'],['待受IPv4と証明書','pass'],['この端末の待受','pass'],['別端末からの到達','skipped']],
};
export function receiverDiagnosticFailures(surface,scope,before){
  const failures=[];
  const expected=expectedDiagnosticStages[scope];
  if(!expected || surface?.count!==1 || surface.open!==true || surface.buttonEnabled!==true || surface.warnings!==0 || !surface.text?.includes('診断日時:'))return ['diagnostic-surface'];
  const current=surface.projection;
  if(current?.revision!==before.revision || current?.generation!==before.generation || current?.device_id!==before.device_id
    || current?.hub_url!==before.hub_url || !isDeepStrictEqual(current?.receiver,before.receiver))failures.push('diagnosis-mutated-publication');
  if(!isDeepStrictEqual(surface.stages?.map(row=>[row.label,row.status]),expected)
    || surface.stages.some(row=>!row.detail?.trim()))failures.push('diagnostic-stage-result');
  if(scope==='receiver' && (!surface.stages?.[2]?.detail.includes(before.receiver.endpoint)
    || !surface.stages[2].detail.includes('receiving') || !surface.stages?.[3]?.detail.includes('確定できません')))failures.push('listener-or-physical-limit');
  return failures;
}
export function receiverRecommendationReady(surface,before){
  const expected=before?.recommended_main_selection;
  const sorted=value=>Array.isArray(value)?[...value].sort():null;
  return Boolean(expected && surface?.fatal===0 && surface.selected?.length===new Set(surface.selected).size
    && isDeepStrictEqual(sorted(surface.selected),sorted(expected.allowed_model_ids))
    && surface.preferred===expected.preferred_model_id && surface.wait===expected.wait_policy
    && surface.affinity===String(expected.affinity_turns)
    && isDeepStrictEqual(surface.capabilities?.split(/[,\s]+/u).filter(Boolean),expected.required_capabilities)
    && surface.saveEnabled===true && surface.hub.settings_revision===before.settings_revision
    && surface.hub.main_mode===before.main_mode && surface.hub.side_chat_mode===before.side_chat_mode
    && isDeepStrictEqual(surface.hub.main_review,before.main_review) && isDeepStrictEqual(surface.hub.side_chat_review,before.side_chat_review));
}
export async function exerciseReceiverRecommendation({cdp,input,sink,owner}){
  const before=await invokeDesktopCommand(cdp,'hub_projection');
  if(!before.recommended_main_selection)throw fail('The real Hub has no model recommendation',before);
  await trustedClick(input,cdp,action('hub-main-recommendation'),sink);
  const observation=await wait('Hub recommendation fills only the visible Main draft',async()=>({
    hub:await invokeDesktopCommand(cdp,'hub_projection'),
    ...await cdp.evaluate(`(()=>({selected:[...document.querySelectorAll('input[data-hub-field^="main:model:"]:checked')].map(n=>n.dataset.hubField.slice('main:model:'.length)),preferred:document.querySelector('#hub-main-preferred')?.value,wait:document.querySelector('#hub-main-wait')?.value,affinity:document.querySelector('#hub-main-affinity')?.value,capabilities:document.querySelector('#hub-main-capabilities')?.value,saveEnabled:document.querySelector('[data-action="hub-save-main"]')?.disabled===false,fatal:document.querySelectorAll('.fatal').length}))()`),
  }),value=>receiverRecommendationReady(value,before));
  await sink.record('receiver-hub-recommendation-draft',{before,observation},{phase:'executing',owner});
  await captureScenarioScreenshot({cdp,sink,name:'receiver-hub-recommendation-draft',owner});
}
export async function exerciseReceiverLocalDiagnostics({state,cdp,input,sink,owner}){
  for(const scope of ['hub','receiver']){
    const key='device-network-diagnostic-'+JSON.stringify([scope,'']);
    const detailsKey=scope==='hub'?'device-network-hub-diagnostic':'device-network-bind-details';
    const detailsSelector='details[data-details-key="'+detailsKey+'"]';
    const summary={selector:detailsSelector+' > summary',identity:{tag:'DETAILS',detailsKey}};
    if(!await cdp.evaluate(`document.querySelector(${JSON.stringify(detailsSelector)})?.open`))await trustedClick(input,cdp,summary,sink);
    const before=await invokeDesktopCommand(cdp,'device_network_projection');
    const commands=state.commands=new DesktopCommandProbe(cdp,{probeId:'receiver-diagnostic-'+scope,commands:['device_network_diagnose']});
    await commands.install();
    await trustedClick(input,cdp,byId('device-network-diagnose-'+scope),sink);
    const observation=await wait('Actual '+scope+' diagnostic completes without changing reception',async()=>({
      projection:await invokeDesktopCommand(cdp,'device_network_projection'),
      ...await cdp.evaluate(`(()=>{const nodes=[...document.querySelectorAll('[data-settings-passive]')].filter(n=>n.dataset.settingsPassive===${JSON.stringify(key)}),n=nodes[0];return {count:nodes.length,open:document.querySelector(${JSON.stringify(detailsSelector)})?.open,buttonEnabled:document.getElementById(${JSON.stringify('device-network-diagnose-'+scope)})?.disabled===false,text:n?.textContent,warnings:n?.querySelectorAll('.warning').length,stages:[...(n?.querySelectorAll('ol > li')??[])].map(row=>({label:row.querySelector('strong')?.firstChild?.textContent?.trim(),status:row.dataset.status,detail:row.querySelector('p')?.textContent}))};})()`),
    }),value=>receiverDiagnosticFailures(value,scope,before).length===0,20000);
    const proof=assertExactDesktopCommandSequence(await commands.snapshot(),{expected:[{command:'device_network_diagnose',args:{scope,expectedRevision:before.revision,expectedGeneration:before.generation}}]});
    await sink.record('receiver-saved-connection-diagnosed',{scope,before,observation,proof,physical_limit:'Receiver firewall skipped is correct; loopback cannot prove physical peer reachability.'},{phase:'executing',owner});
    await captureScenarioScreenshot({cdp,sink,name:'receiver-'+scope+'-diagnostic',owner});
    await commands.remove();state.commands=null;
    await trustedClick(input,cdp,summary,sink);
    await wait('Diagnostic disclosure closes',()=>cdp.evaluate(`document.querySelector(${JSON.stringify(detailsSelector)})?.open`),value=>value===false);
  }
}
