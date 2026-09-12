import path from 'node:path';
import { readFile } from 'node:fs/promises';
import { DesktopE2eError } from '../core/execution.mjs';
import { DesktopCommandProbe } from '../drivers/desktop_command_probe.mjs';
import { WebviewInput } from '../drivers/webview_input.mjs';
import { normalizeHubBrowserOptions, startHubBrowserResource } from '../drivers/hub_browser_resource.mjs';
import { acquireInteractiveShell, prepareShellBaseline } from './shell_baseline.mjs';
import { captureScenarioScreenshot, invokeDesktopCommand } from './observations.mjs';
import { trustedClick, byId, wait, requestHubEnrollmentExit } from './hub_browser_enrollment.mjs';
import { importDesktopHubParticipationFile } from './hub_browser_enrollment.mjs';

const ID='hub.join-retry-controls', OWNER=`scenario:${ID}`;
const product=(message,evidence)=>new DesktopE2eError('product','hub-join-retry-result',message,evidence);
const timing=evidence=>new DesktopE2eError('harness','hub-join-retry-timing-limit',
  'Automatic enrollment won before an explicit Retry command was observed; this run cannot prove the Retry control',evidence);
const watched=['device_network_request_join','device_network_refresh','submit_prompt','submit_side_chat','device_network_receiver'];

export function failedJoinReady(surface,url){
  const p=surface.network;
  return p.enrollment==='error'&&p.hub_url===url&&p.device_id===null&&p.request_id===null&&p.can_join===true
    &&surface.join.count===1&&surface.join.visible&&surface.join.enabled&&surface.errorVisible;
}
export function explicitRetryObserved(calls){
  return calls.length===1&&calls[0].command==='device_network_request_join';
}
export function acceptedRetryIdentity(network,snapshot,deviceId=null){
  return network.enrollment==='active'&&typeof network.device_id==='string'&&network.device_id.length>0
    &&(deviceId===null||network.device_id===deviceId)&&network.can_join===false
    &&snapshot.devices.length===1&&snapshot.devices[0].device_id===network.device_id
    &&snapshot.join_requests.length===0;
}
export async function observeJoinRetry(cdp){
  return cdp.evaluate(`(async()=>{
    const network=await window.__TAURI_INTERNALS__.invoke('device_network_projection');
    await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
    const visible=e=>Boolean(e?.isConnected&&e.getClientRects().length&&!e.closest('[hidden],[aria-hidden="true"],[inert]'));
    const nodes=[...document.querySelectorAll('#device-network-join')],button=nodes[0];
    const feedback=document.querySelector('#device-network-feedback');
    return {network,join:{count:nodes.length,visible:visible(button),enabled:Boolean(button&&!button.disabled),focused:document.activeElement===button},
      errorVisible:visible(feedback)&&feedback.dataset.error==='true'&&Boolean(feedback.textContent.trim()),
      enrollmentText:document.querySelector('[data-settings-passive="device-network-enrollment"]')?.textContent.trim()??'',
      feedback:visible(feedback)?feedback.textContent.trim():null};
  })()`);
}

export function createHubJoinRetryControlsScenario(options={}){
  const settings=normalizeHubBrowserOptions(options);
  const state={resource:null,input:null,probe:null,nativeOwner:null,nativeCandidate:null,nativeBefore:null,importDispatched:false,inputFailures:[],close:null};
  return Object.freeze({id:ID,productOracle:'pass',manualGate:'pending',databaseRequired:true,
    async prepare(args){await prepareShellBaseline(args);state.resource=await startHubBrowserResource({...args,options:settings});},
    requestGracefulExit:cdp=>requestHubEnrollmentExit(cdp,state),
    async execute({context,runtime,driver:cdp,sink}){
      const resource=state.resource,{page,hub}=resource;
      await acquireInteractiveShell({context,driver:cdp,sink},{evidenceOwner:OWNER,screenshotStem:'join-retry-shell'});
      const input=state.input=new WebviewInput(cdp,{probeId:ID});
      const probe=state.probe=new DesktopCommandProbe(cdp,{probeId:ID,commands:watched});
      let primary=null;
      try{
        await input.installProbe();await probe.install();
        await page.goto(hub.url);await page.locator('#management-status').filter({hasText:'Hub本体に接続中'}).waitFor();
        await page.locator('nav a[href="#device-network"]').click();
        await page.locator('#network-ip').fill('127.0.0.1');await page.locator('#network-port').fill(String(hub.networkPort));
        await page.locator('#network-start').click();await page.locator('#network-stop').waitFor();
        const original=await wait('Real Hub network is running',()=>hub.observeNetwork(),s=>s.server.running===true);
        const downloadPromise=page.waitForEvent('download');
        await page.getByRole('button',{name:'設定ファイルを保存',exact:true}).click();
        const download=await downloadPromise;
        if(download.suggestedFilename()!=='hub-participation.toml')throw product('Unexpected public config filename',{});
        const importPath=path.join(context.paths.workspace,'hub-participation.toml');await download.saveAs(importPath);
        const config=await readFile(importPath,'utf8'),url=`https://127.0.0.1:${hub.networkPort}`;
        if(!config.includes(url)||!config.includes('BEGIN CERTIFICATE')||config.includes('PRIVATE KEY'))throw product('Browser must download only this Hub endpoint and public CA',{});
        await page.locator('#network-stop').click();await page.locator('#network-start').waitFor();
        await wait('Real network has stopped while browser management remains available',()=>hub.observeNetwork(),s=>s.server.running===false);
        await page.locator('#network-ip').fill('127.0.0.1');await page.locator('#network-port').fill(String(hub.networkPort));
        await importDesktopHubParticipationFile({context,runtime,cdp,input,sink,nativeState:state,importPath});
        const failed=await wait('Failed import preserves configured endpoint and enables explicit participation retry',()=>observeJoinRetry(cdp),s=>failedJoinReady(s,url));
        await captureScenarioScreenshot({cdp,sink,name:'join-retry-offline-import',owner:OWNER});
        await sink.record('join-retry-unregistered',{url,network:failed.network,join:failed.join,error:failed.feedback},{phase:'executing',owner:OWNER});
        // Acquire the ordinary control before server restart; do not alter the background heartbeat.
        for(let steps=0;steps<80;steps++){
          const observed=await observeJoinRetry(cdp);if(observed.join.focused)break;
          if(!failedJoinReady(observed,url))throw product('Retry precondition changed while Hub was still stopped',observed);
          await input.pressKey('Tab');
        }
        if(!(await observeJoinRetry(cdp)).join.focused)throw new DesktopE2eError('harness','hub-join-retry-focus','Retry button was not reached by ordinary Tab',{});
        await page.locator('#network-start').click();await page.locator('#network-stop').waitFor();
        const beforeClick=await observeJoinRetry(cdp);
        if(['pending','active'].includes(beforeClick.network.enrollment))throw timing({stage:'before-click',observation:beforeClick});
        if(!failedJoinReady(beforeClick,url))throw product('Retry is not available after server restart',beforeClick);
        const commandStart=(await probe.snapshot()).sequence;
        try{await trustedClick(input,cdp,byId('device-network-join'),sink);}
        catch(error){
          const current=await observeJoinRetry(cdp),calls=(await probe.snapshot(commandStart)).calls;
          if(!explicitRetryObserved(calls)&&['pending','active'].includes(current.network.enrollment))throw timing({stage:'click',observation:current,calls});
          throw error;
        }
        const delivered=await wait('Dedicated Retry emits one participation command',async()=>({calls:(await probe.snapshot(commandStart)).calls,observed:await observeJoinRetry(cdp)}),v=>{
          if(v.calls.length===0&&['pending','active'].includes(v.observed.network.enrollment))throw timing({stage:'command',...v});
          return v.calls.length>0;
        });
        if(!explicitRetryObserved(delivered.calls))throw product('Retry must call only its dedicated join command once',delivered);
        const expected={expectedRevision:beforeClick.network.revision,expectedGeneration:beforeClick.network.generation};
        for(const [key,value]of Object.entries(expected))if(delivered.calls[0].args[key]!==value)throw product('Retry command target drifted',{expected,call:delivered.calls[0]});
        const pending=await wait('Retry creates one pending request and disables repeat submission',async()=>({surface:await observeJoinRetry(cdp),snapshot:await hub.observeNetwork()}),v=>v.surface.network.enrollment==='pending'&&!v.surface.network.can_join&&!v.surface.join.enabled
          &&typeof v.surface.network.request_id==='string'&&v.snapshot.devices.length===0&&v.snapshot.join_requests.length===1&&v.snapshot.join_requests[0].request_id===v.surface.network.request_id);
        if(pending.snapshot.hub_id!==original.hub_id||pending.snapshot.ca_sha256!==original.ca_sha256)throw product('Network restart changed this Hub identity',{});
        await captureScenarioScreenshot({cdp,sink,name:'join-retry-pending',owner:OWNER});
        await page.locator('nav a[href="#clients"]').click();
        const row=page.locator(`[data-id="request:${pending.surface.network.request_id}"]`);await row.waitFor();
        await resource.screenshot('join-retry-hub-pending');await row.locator('button[data-network-action]').click();
        const active=await wait('Approval adopts exactly one registered device',async()=>({surface:await observeJoinRetry(cdp),snapshot:await hub.observeNetwork()}),v=>acceptedRetryIdentity(v.surface.network,v.snapshot)&&!v.surface.join.enabled&&v.surface.enrollmentText.includes('参加済み'),45_000);
        const deviceId=active.surface.network.device_id,stableStart=performance.now();
        do{
          const current=await observeJoinRetry(cdp),snapshot=await hub.observeNetwork();
          if(!acceptedRetryIdentity(current.network,snapshot,deviceId))throw product('Approval created a duplicate or changed the registered device identity',{network:current.network,snapshot});
          if(performance.now()-stableStart>=1200)break;
          await new Promise(resolve=>setTimeout(resolve,100));
        }while(true);
        const allCalls=(await probe.snapshot()).calls;
        if(!explicitRetryObserved(allCalls))throw product('Retry flow must not use refresh, task execution or receiver mutation',allCalls);
        if(resource.pageErrors().length)throw product('Hub page reported errors',resource.pageErrors());
        await captureScenarioScreenshot({cdp,sink,name:'join-retry-desktop-active',owner:OWNER});await resource.screenshot('join-retry-hub-approved');
        await sink.record('join-retry-verified',{command:delivered.calls[0],request_id:pending.surface.network.request_id,device_id:deviceId,devices:active.snapshot.devices.length,same_hub_ca:true,id_stable_ms:1200,identity_before_registration:null,
          scope:'Dedicated explicit Retry; an automatic pending without its trusted click and command is a fixture timing limit, not a pass.'},{phase:'executing',owner:OWNER});
        return {acquisition:'pass',oracle:'pass',manual:'pending'};
      }catch(error){primary=error;try{await captureScenarioScreenshot({cdp,sink,name:'join-retry-failure',owner:OWNER});}catch{}throw error;}
      finally{
        for(const [key,close]of[['probe',()=>probe.remove()],['input',()=>input.cleanup()]]){
          try{await close();state[key]=null;}catch(error){state.inputFailures.push(`${key}:${error?.code??error.message}`);}
        }
        if(primary===null&&state.inputFailures.length)throw new DesktopE2eError('harness','hub-join-retry-input-cleanup','Input or command probe cleanup failed',{failures:state.inputFailures});
      }
    },
    async quiesce(){
      for(const [key,method]of[['probe','remove'],['input','cleanup']])if(state[key]){try{await state[key][method]();state[key]=null;}catch(error){state.inputFailures.push(`${key}:${error?.code??error.message}`);}}
      state.close??=state.resource?await state.resource.close():{pass:true,started:false};
      return {input:state.close.pass&&state.inputFailures.length===0?'pass':'fail',resources:[{kind:'hub-browser',close:state.close,input_failures:state.inputFailures}]};
    },
    async cleanup(){return {input:state.close?.pass&&state.inputFailures.length===0?'pass':'fail',resources:[]};},
  });
}
