import path from "node:path";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { isDeepStrictEqual } from "node:util";
import { DesktopE2eError } from "../core/execution.mjs";
import { normalizeHubBrowserOptions, startHubBrowserResource } from "../drivers/hub_browser_resource.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { acquireInteractiveShell, prepareShellBaseline } from "./shell_baseline.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { byId, trustedClick, wait, enrollDesktopFromHubBrowser, requestHubEnrollmentExit } from "./hub_browser_enrollment.mjs";

const ID = "hub.offline-reset", OWNER = `scenario:${ID}`;
const fail = (message, evidence = {}) => new DesktopE2eError("product", "offline-reset-mismatch", message, evidence);
const directIdentity = p => ({ endpoint:p?.provider_effective_base_url, profile:p?.provider_effective_profile, model:p?.provider_effective_model_id });
export function resetAccepted(network, hub) {
  return network?.enrollment === "unconfigured" && network.hub_url === "" && network.device_id === null
    && network.request_id === null && network.peers.length === 0 && !network.receiver.enabled
    && hub?.main_mode === "direct" && hub.side_chat_mode === "direct" && hub.main_review === null && hub.side_chat_review === null;
}

export function createHubOfflineResetScenario(options = {}) {
  const settings = normalizeHubBrowserOptions(options);
  const state = { resource:null, replacement:null, input:null, failures:[], closed:null };
  async function startNetwork(resource) {
    const {page,hub} = resource;
    await page.locator('nav a[href="#device-network"]').click();
    await page.locator("#network-ip").fill("127.0.0.1");
    await page.locator("#network-port").fill(String(hub.networkPort));
    await page.locator("#network-start").click();
    await wait("Isolated Hub is listening",()=>hub.observeNetwork(),value=>value.server.running===true);
  }
  return Object.freeze({
    id:ID,productOracle:"pass",manualGate:"not_required",databaseRequired:true,
    async prepare(args) {
      await prepareShellBaseline(args);
      const networkDirectory = path.join(path.dirname(args.context.paths.config_file),"device-network");
      await mkdir(networkDirectory,{recursive:true});
      await writeFile(path.join(networkDirectory,"device.json"),JSON.stringify({
        schema_version:1,revision:"1",hub_id:null,device_id:null,label:"",certificate_pem:null,certificate_sha256:null,expires_at_ms:null,
        selected_peers:[{device_id:"old-device-a",profile_id:"old-profile-a"},{device_id:"old-device-b",profile_id:"old-profile-b"}],
        receiver:{profile_id:"01ARZ3NDEKTSV4RRFFQ69G5FAV",target:{kind:"temp"},access_mode:"default",model_mode:"hub",confirmed:false,enabled:false},
      }),{flag:"wx"});
      await args.sink.record("legacy-target-fixture",{count:2,scope:"Prelaunch saved selections only; no registration, permission or running job is seeded."},{phase:args.phase,owner:OWNER});
      state.resource = await startHubBrowserResource({...args,options:settings});
      const root = path.join(args.context.root,"replacement-hub"); await mkdir(root);
      state.replacement = await startHubBrowserResource({...args,context:{...args.context,root},sink:args.sink.scope("replacement"),options:settings});
    },
    async execute({context,runtime,driver:cdp,sink}) {
      await acquireInteractiveShell({context,driver:cdp,sink},{evidenceOwner:OWNER,screenshotStem:"reset-initial-shell"});
      const before = await invokeDesktopCommand(cdp,"desktop_state");
      const direct = directIdentity(before);
      if (!direct.model) throw fail("Fixture requires a saved manual AI connection");
      const sentinel = path.join(context.paths.workspace,"RESET_PRESERVES_WORK.txt");
      await writeFile(sentinel,"Keep the work and its folder.\n",{flag:"wx"});
      const input = state.input = new WebviewInput(cdp,{probeId:ID}); await input.installProbe();
      try {
        await trustedClick(input,cdp,{selector:'aside.sidebar button[data-action="show-hub"]',identity:{tag:"BUTTON",action:"show-hub"}},sink);
        await wait("Saved unavailable targets remain available for cleanup",()=>invokeDesktopCommand(cdp,"device_network_projection"),p=>p.peers.filter(peer=>peer.selected).length===2);
        await trustedClick(input,cdp,{selector:'#device-network-details > summary',identity:{tag:"DETAILS",detailsKey:"device-network-details"}},sink);
        await trustedClick(input,cdp,{selector:'details[data-details-key="device-network-saved-peers"] > summary',identity:{tag:"DETAILS",detailsKey:"device-network-saved-peers"}},sink);
        const legacyCheck=byId("device-network-delete-peer-0","INPUT");
        await trustedClick(input,cdp,legacyCheck,sink);
        await trustedClick(input,cdp,legacyCheck,sink);
        if ((await invokeDesktopCommand(cdp,"device_network_projection")).peers.filter(peer=>peer.selected).length!==2) throw fail("Cancelling saved-target deletion changed selections");
        await trustedClick(input,cdp,legacyCheck,sink);
        await trustedClick(input,cdp,{selector:'button[data-action="device-network-delete-peer"]:not([disabled])',identity:{tag:"BUTTON",action:"device-network-delete-peer"}},sink);
        await wait("Only the confirmed legacy target is removed",()=>invokeDesktopCommand(cdp,"device_network_projection"),p=>p.peers.filter(peer=>peer.selected).length===1&&p.peers.some(peer=>peer.device_id==="old-device-b"&&peer.profile_id==="old-profile-b"&&peer.selected));
        const saved=JSON.parse(await readFile(path.join(path.dirname(context.paths.config_file),"device-network","device.json"),"utf8"));
        if (!isDeepStrictEqual(saved.selected_peers,[{device_id:"old-device-b",profile_id:"old-profile-b"}])) throw fail("Legacy cleanup did not persist the exact retained target");
        await captureScenarioScreenshot({cdp,sink,name:"legacy-target-deletion-complete",owner:OWNER});
        await trustedClick(input,cdp,{selector:'#device-network-details > summary',identity:{tag:"DETAILS",detailsKey:"device-network-details"}},sink);
        await startNetwork(state.resource);
        const joined = await enrollDesktopFromHubBrowser({resource:state.resource,context,runtime,cdp,input,sink:sink.scope("old-enrollment"),nativeState:state});
        await state.resource.page.locator('nav a[href="#device-network"]').click();
        await state.resource.page.locator("#network-stop").click();
        await wait("Old Hub is offline before reset",()=>state.resource.hub.observeNetwork(),p=>p.server.running===false);
        const summary = {selector:'#device-network-reset-details > summary',identity:{tag:"DETAILS",detailsKey:"device-network-reset-details"}};
        await trustedClick(input,cdp,summary,sink);
        const disabled = await cdp.evaluate(`document.querySelector('#device-network-reset').disabled`);
        if (!disabled) throw fail("Reset must require explicit impact confirmation");
        await trustedClick(input,cdp,byId("device-network-reset-confirmed","INPUT"),sink);
        // Cancellation is local: unchecking must leave the exact old registration intact.
        await trustedClick(input,cdp,byId("device-network-reset-confirmed","INPUT"),sink);
        if ((await invokeDesktopCommand(cdp,"device_network_projection")).device_id !== joined.network.device_id) throw fail("Cancel changed registration");
        await trustedClick(input,cdp,byId("device-network-reset-confirmed","INPUT"),sink);
        await trustedClick(input,cdp,byId("device-network-reset"),sink);
        await wait("Offline local reset completes",async()=>({network:await invokeDesktopCommand(cdp,"device_network_projection"),hub:await invokeDesktopCommand(cdp,"hub_projection")}),p=>resetAccepted(p.network,p.hub));
        const after = await invokeDesktopCommand(cdp,"desktop_state");
        if (!isDeepStrictEqual(directIdentity(after),direct) || !isDeepStrictEqual(after.draft_target,before.draft_target)
          || await readFile(sentinel,"utf8") !== "Keep the work and its folder.\n") throw fail("Reset altered manual AI settings, local session ownership or work files");
        if (!(await state.resource.hub.observeNetwork()).devices.some(row=>row.device_id===joined.network.device_id)) throw fail("Local reset must not claim old Hub revocation");
        await captureScenarioScreenshot({cdp,sink,name:"offline-reset-complete",owner:OWNER});
        await trustedClick(input,cdp,{selector:'.hub-modal button[data-action="show-config"]',identity:{tag:"BUTTON",action:"show-config"}},sink);
        await wait("The same AI form returns to editable manual settings",()=>cdp.evaluate(`(()=>{
          const url=document.querySelector('input[data-config-key="model.base_url"]');
          return {value:url?.value,editable:Boolean(url&&!url.readOnly&&!url.disabled&&url.getClientRects().length),hubModels:document.querySelectorAll('[data-ai-connection]').length};
        })()`),p=>p.value===direct.endpoint&&p.editable&&p.hubModels===0);
        await captureScenarioScreenshot({cdp,sink,name:"offline-reset-manual-ai-restored",owner:OWNER});
        await trustedClick(input,cdp,{selector:'.settings-modal button[aria-label="閉じる"][data-action="close-overlay"]',identity:{tag:"BUTTON",action:"close-overlay"}},sink);
        await startNetwork(state.replacement);
        const fresh = await enrollDesktopFromHubBrowser({resource:state.replacement,context,runtime,cdp,input,sink:sink.scope("new-enrollment"),nativeState:state});
        if (fresh.network.device_id===joined.network.device_id || fresh.keySha256===joined.keySha256 || fresh.certificateSha256===joined.certificateSha256) throw fail("New Hub reused old registration or private key");
        await sink.record("offline-reset-verified",{old_device:joined.network.device_id,new_device:fresh.network.device_id,old_hub_offline:true,old_registration_retained:true,manual_ai_retained:true,workspace_retained:true,new_identity:true,scope:"Actual Tauri confirmation/cancel/reset/import and separate Hub CA approval; old Runner uncertainty and journal retention are covered by focused backend tests."},{phase:"executing",owner:OWNER});
        await captureScenarioScreenshot({cdp,sink,name:"offline-reset-new-hub-ready",owner:OWNER});
        return {acquisition:"pass",oracle:"pass",manual:"not_required"};
      } finally { try { await input.cleanup(); } catch { state.failures.push("input-cleanup"); } state.input=null; }
    },
    requestGracefulExit:cdp=>requestHubEnrollmentExit(cdp,state),
    async quiesce() {
      if (state.closed === null) {
        state.closed = [];
        // Both resources share the append-only evidence sink; preserve its
        // lifecycle order instead of racing the two final receipt writes.
        for (const resource of [state.replacement,state.resource].filter(Boolean)) state.closed.push(await resource.close());
      }
      return {input:state.closed.every(value=>value.pass)&&!state.failures.length?"pass":"fail",resources:state.closed};
    },
    async cleanup(){return {input:state.closed?.every(value=>value.pass)&&!state.failures.length?"pass":"fail",resources:[]};},
  });
}
