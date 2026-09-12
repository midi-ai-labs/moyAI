import {isDeepStrictEqual as same} from 'node:util';
import {assertTrustedProbeSequence,assertTrustedTextInsertion} from '../drivers/webview_input.mjs';
import {invokeDesktopCommand,captureScenarioScreenshot} from './observations.mjs';
import {byId,trustedClick,wait} from './hub_browser_enrollment.mjs';
import {waitForSemanticTargetSettlement} from '../core/semantic_target_settlement.mjs';
const TARGET=byId('device-network-search','INPUT'),OWNER='scenario:hub.outgoing-controls';
const peers=p=>p.peers.map(row=>({key:JSON.stringify([row.device_id,row.profile_id]),selected:row.selected}));
export function outgoingPeerSearchMatches(value,{query,visibleKeys,baseline}){return value.query===query&&value.focused&&same(value.visibleKeys,visibleKeys)
  &&same(peers(value.network),baseline)&&value.errors===0&&(visibleKeys.length>0||value.emptyText.includes('検索条件'));}
async function observe(cdp){return {network:await invokeDesktopCommand(cdp,'device_network_projection'),...await cdp.evaluate(`(()=>{
  const field=document.getElementById('device-network-search'),list=document.querySelector('.device-network-peers');return {query:field?.value,focused:document.activeElement===field,
    visibleKeys:Array.from(list?.querySelectorAll('[data-action="device-network-select"]')??[]).map(n=>n.dataset.value),emptyText:list?.querySelector('.hub-empty')?.textContent??'',errors:document.querySelectorAll('.fatal,.ui-error-notice').length};})()`)};}
export async function exerciseOutgoingPeerSearch({cdp,input,sink,deviceId,profileId}){
  await waitForSemanticTargetSettlement({input,locator:TARGET,label:'Peer search input is mounted after receiver discovery'});
  await trustedClick(input,cdp,TARGET,sink);
  const initial=await wait('Owned outgoing receiver is visible before filtering',()=>observe(cdp),v=>v.query===''&&v.focused&&v.visibleKeys.length===1&&v.visibleKeys[0]===JSON.stringify([deviceId,profileId]));
  const baseline=peers(initial.network),key=baseline[0].key,results=[];
  const match=initial.network.peers.find(row=>row.device_id===deviceId&&row.profile_id===profileId).display_name;
  for(const query of [match,'unmatched-device-search-control','']){
    if((await observe(cdp)).query!==''){
      await input.keyDown('Control');try{await input.pressKey('a');}finally{await input.keyUp('Control');}
      const start=(await input.snapshotProbe()).sequence;await input.pressKey('Backspace');
      assertTrustedProbeSequence(await input.snapshotProbe(start),{afterSequence:start,expected:[{type:'keydown',identity:TARGET.identity,key:'Backspace'},{type:'input',identity:TARGET.identity,inputType:'deleteContentBackward',data:null},{type:'keyup',identity:TARGET.identity,key:'Backspace'}]});
    }
    if(query){const start=(await input.snapshotProbe()).sequence;await input.insertText(TARGET,query);assertTrustedTextInsertion(await input.snapshotProbe(start),{afterSequence:start,identity:TARGET.identity,text:query});}
    const expected={query,visibleKeys:query==='unmatched-device-search-control'?[]:[key],baseline};
    const result=await wait('Search filters only visible peers and preserves allowed selections',()=>observe(cdp),v=>outgoingPeerSearchMatches(v,expected));
    await captureScenarioScreenshot({cdp,sink,name:`outgoing-peer-search-${results.length}`,owner:OWNER});results.push(result);
  }
  await sink.record('outgoing-peer-search-result',{baseline,results,scope:'One allowed protocol peer: display-name match, no match, clear; selection unchanged. No multi-peer/Japanese/case matrix.'},{phase:'executing',owner:OWNER});
}
