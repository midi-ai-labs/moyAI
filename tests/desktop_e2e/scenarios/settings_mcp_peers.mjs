import { readFile } from "node:fs/promises";
import { DesktopE2eError } from "../core/execution.mjs";
import { waitForObservation } from "../core/deadline.mjs";
import { WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion } from "../drivers/webview_input.mjs";
import { DesktopCommandProbe } from "../drivers/desktop_command_probe.mjs";
import { startScriptedProvider } from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { quiesceProviderResource } from "./provider_restart.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { settingsPreferencesFixtureConfig, tabToSettingsControl, trustedClickProbeEvents } from "./settings_preferences.mjs";

const ID = "settings.mcp-peer-controls";
const OWNER = `scenario:${ID}`;
const PANEL = '[role="dialog"][aria-labelledby="config-dialog-title"]';
const TOKEN = "moyai_e2e_direct_peer_0123456789abcdef";
const WRONG_TOKEN = "moyai_e2e_wrong_token_0123456789abcdef";
const GOOD = "audit-peer-good";
const BAD = "audit-peer-bad";
const field = (id, tag = "INPUT") => ({ selector: `${PANEL} #${id}`, identity: { tag, id } });
const action = (name, value) => ({ selector: `${PANEL} button[data-action="${name}"]${value ? `[data-value="${value}"]` : ""}`, identity: { tag: "BUTTON", action: name } });
const fail = (code, message, evidence) => new DesktopE2eError("product", code, message, evidence);

export async function observeMcpPeerControls(cdp) {
  return cdp.evaluate(`(async () => {
    const [projection, peers] = await Promise.all([window.__TAURI_INTERNALS__.invoke('desktop_state'), window.__TAURI_INTERNALS__.invoke('mcp_peer_projection')]);
    await new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)));
    const panel=document.querySelector(${JSON.stringify(PANEL)});
    const el=id=>panel?.querySelector('#'+id);
    return { projection, peers:peers.rows, panel:Boolean(panel), dirty:panel?.querySelector('.dirty-badge')?.classList.contains('visible')??false,
      fields:Object.fromEntries(['id','url','token','certificate'].map(name=>[name,{value:el('mcp-peer-'+name)?.value??null,type:el('mcp-peer-'+name)?.type??null,disabled:el('mcp-peer-'+name)?.disabled??true}])),
      buttons:[...(panel?.querySelectorAll('button[data-action^="mcp-peer-"]')??[])].map(button=>({action:button.dataset.action,id:button.dataset.value??null,disabled:button.disabled})),
      rows:[...(panel?.querySelectorAll('.mcp-peer-panel article')??[])].map(row=>({id:row.querySelector('strong')?.textContent,text:row.textContent})),
      feedback:el('mcp-peer-feedback')?.textContent??'',
      visible_errors:[...(panel?.querySelectorAll('.ui-error-notice:not([hidden])')??[])].map(node=>node.textContent)
    };
  })()`);
}

export function peerRowsMatch(surface, ids) {
  const actual = surface.peers?.map(row => row.id).sort();
  return JSON.stringify(actual) === JSON.stringify([...ids].sort())
    && surface.rows?.length === ids.length
    && ids.every(id => surface.rows.some(row => row.id === id));
}

export function exactMcpCheckLedger(ledger, statuses) {
  return ledger.length === statuses.length && ledger.every((row, index) => row.route === "mcp_peer"
    && row.method === "POST" && row.pathname === "/mcp" && row.query_present === false
    && row.contract?.pass === true && row.contract.rpc_method === "tools/list"
    && row.response_phase === "completed" && row.response_status === statuses[index]);
}

async function wait(cdp, label, accept) {
  try { return (await waitForObservation({ label, timeoutMs: 15_000, pollMs: 50, retrySampleErrors: false, sample: () => observeMcpPeerControls(cdp), accept })).value; }
  catch(error) { if(error?.code==='observation-timeout'&&!error.evidence?.last_error) throw fail('mcp-peer-control-state',label,error.evidence); throw error; }
}
async function click(input, locator) {
  const start=(await input.snapshotProbe()).sequence;
  await input.click(locator);
  return assertTrustedProbeSequence(await input.snapshotProbe(start),{afterSequence:start,expected:trustedClickProbeEvents(locator)});
}
async function activate(input, cdp, name, value) {
  const locator=action(name,value);
  await tabToSettingsControl(input,cdp,locator,{maxSteps:160});
  const start=(await input.snapshotProbe()).sequence;
  await input.pressKey('Enter');
  return assertTrustedProbeSequence(await input.snapshotProbe(start),{afterSequence:start,expected:[
    {type:'keydown',key:'Enter',code:'Enter',identity:locator.identity},
    {type:'click',identity:locator.identity,button:0,detail:0},
    {type:'keyup',key:'Enter',code:'Enter'},
  ]});
}
async function edit(input,cdp,id,text,tag="INPUT") {
  const locator=field(id,tag);
  await tabToSettingsControl(input,cdp,locator,{maxSteps:160});
  await input.keyDown("Control");
  try { await input.pressKey("a"); } finally { await input.keyUp("Control"); }
  if(text==='') await input.pressKey("Backspace");
  else { const start=(await input.snapshotProbe()).sequence; await input.insertText(locator,text);
    assertTrustedTextInsertion(await input.snapshotProbe(start),{afterSequence:start,identity:locator.identity,text}); }
  return wait(cdp,`${id} retains exact input`,state=>state.fields[id.replace('mcp-peer-','')].value===text);
}
async function open(input,cdp) {
  await click(input,{selector:'aside.sidebar button.settings[data-action="show-config"][title="設定"]',identity:{tag:'BUTTON',action:'show-config'}});
  await wait(cdp,'Global settings opens',state=>state.panel&&state.projection.overlay==='config');
  await click(input,{selector:`${PANEL} nav.settings-nav a[href="#settings-tools"]`,identity:{tag:'A',href:'#settings-tools'}});
  await wait(cdp,'Peer fields load',state=>!state.fields.id.disabled);
}
function button(state,name,id=null) {return state.buttons.find(row=>row.action===name&&row.id===id);}

export function createSettingsMcpPeerControlsScenario() {
  let provider=null,acceptedLedger=null,quiesceOutcome=null;
  const cleanup=[];
  return Object.freeze({id:ID,productOracle:'pass',manualGate:'not_required',databaseRequired:true,requestGracefulExit,
    async prepare({context,sink,phase}) {
      provider=await startScriptedProvider({mcpPeerToken:TOKEN});
      await prepareDesktopFixture({context,sink,phase,owner:OWNER,configText:settingsPreferencesFixtureConfig(provider.baseUrl),sentinelName:'E2E_DIRECT_MCP.txt',sentinelText:'Isolated direct MCP connection form audit.\n'});
    },
    async execute({context,driver:cdp,sink}) {
      await acquireInteractiveShell({context,driver:cdp,sink},{evidenceOwner:OWNER,screenshotStem:'settings-mcp-peer-shell'});
      const input=new WebviewInput(cdp,{probeId:ID,maxProbeEvents:8192});
      const commands=new DesktopCommandProbe(cdp,{probeId:ID,commands:['mcp_peer_add','mcp_peer_remove','mcp_peer_check','save_global_config','send_prompt','submit']});
      let primary=null;
      try {
        await input.installProbe();await commands.install();await open(input,cdp);
        let state=await wait(cdp,'Empty form disables save',state=>peerRowsMatch(state,[])&&button(state,'mcp-peer-add')?.disabled);
        if(state.fields.token.type!=='password') throw fail('mcp-peer-token-not-password','Credential entry must remain a password input',state.fields.token);
        await edit(input,cdp,'mcp-peer-id',GOOD);
        for(const invalid of ['not a URL','ftp://127.0.0.1/mcp',`${provider.baseUrl}/mcp?token=fixture`,`${provider.baseUrl}/mcp#fragment`]) {
          await edit(input,cdp,'mcp-peer-url',invalid);
          state=await wait(cdp,'Invalid URL disables peer save',state=>button(state,'mcp-peer-add')?.disabled);
          await sink.record('mcp-peer-invalid-url',{value:invalid,save_disabled:true},{phase:'executing',owner:OWNER});
        }
        await edit(input,cdp,'mcp-peer-url',`${provider.baseUrl}/mcp`);
        const untouched=await readFile(context.paths.config_file);
        await activate(input,cdp,'mcp-peer-add');
        await wait(cdp,'Missing token has an actionable error',state=>state.feedback.includes('接続用トークンを入力してください')&&peerRowsMatch(state,[]));
        for(const invalid of [{token:'short',certificate:''},{token:TOKEN,certificate:'-----BEGIN CERTIFICATE-----\ninvalid\n-----END CERTIFICATE-----'}]) {
          await edit(input,cdp,'mcp-peer-token',invalid.token);
          await edit(input,cdp,'mcp-peer-certificate',invalid.certificate,'TEXTAREA');
          await activate(input,cdp,'mcp-peer-add');
          await wait(cdp,'Invalid credential/certificate is rejected without a saved peer',state=>peerRowsMatch(state,[])&&!state.fields.id.disabled&&state.feedback.includes('更新できません'));
          if(!(await readFile(context.paths.config_file)).equals(untouched)) throw fail('mcp-peer-rejected-save-changed-file','Rejected peer draft must not write config',{});
        }
        await captureScenarioScreenshot({cdp,sink,name:'settings-mcp-peer-invalid-certificate',owner:OWNER});
        await edit(input,cdp,'mcp-peer-certificate','','TEXTAREA');
        await edit(input,cdp,'mcp-peer-token',TOKEN);
        await activate(input,cdp,'mcp-peer-add');
        state=await wait(cdp,'Valid local peer saves and clears credentials',state=>peerRowsMatch(state,[GOOD])&&!state.fields.id.disabled&&state.fields.token.value===''&&state.fields.certificate.value==='');
        if(!state.peers[0].credential_configured||!state.peers[0].enabled) throw fail('mcp-peer-saved-metadata','Saved peer metadata must report enabled credential',state.peers);
        await activate(input,cdp,'mcp-peer-check',GOOD);
        state=await wait(cdp,'Good peer advertises agent tools',state=>state.rows.find(row=>row.id===GOOD)?.text.includes('エージェント受付を確認しました'));
        if(!exactMcpCheckLedger(provider.requestLedger,[200])) throw fail('mcp-peer-positive-wire','One check must use the saved credential for one tools/list',provider.requestLedger);
        await edit(input,cdp,'mcp-peer-id',GOOD);
        await edit(input,cdp,'mcp-peer-url',`${provider.baseUrl}/mcp`);
        await wait(cdp,'Duplicate ID disables add',state=>button(state,'mcp-peer-add')?.disabled);
        await edit(input,cdp,'mcp-peer-id',BAD);
        await edit(input,cdp,'mcp-peer-token',WRONG_TOKEN);
        await activate(input,cdp,'mcp-peer-refresh');
        await wait(cdp,'Refresh preserves unsaved peer draft',state=>state.fields.id.value===BAD&&state.fields.url.value===`${provider.baseUrl}/mcp`&&state.fields.token.value===WRONG_TOKEN&&!state.fields.id.disabled);
        await activate(input,cdp,'mcp-peer-add');
        await wait(cdp,'Second peer saves independently',state=>peerRowsMatch(state,[GOOD,BAD])&&!state.fields.id.disabled&&state.fields.token.value==='');
        await activate(input,cdp,'mcp-peer-check',BAD);
        state=await wait(cdp,'Wrong token is shown as a connection failure for its row',state=>state.rows.find(row=>row.id===BAD)?.text.includes('接続できません')&&state.rows.find(row=>row.id===GOOD)?.text.includes('エージェント受付を確認しました'));
        if(!exactMcpCheckLedger(provider.requestLedger,[200,401])) throw fail('mcp-peer-negative-wire','Wrong credentials must fail without calling tools',provider.requestLedger);
        await captureScenarioScreenshot({cdp,sink,name:'settings-mcp-peer-good-bad-checks',owner:OWNER});
        const toggle={selector:`${PANEL} input[data-config-key="shell.hide_windows"]`,identity:{tag:'INPUT',configKey:'shell.hide_windows'}};
        await tabToSettingsControl(input,cdp,toggle,{maxSteps:160});await input.pressKey(' ');
        await wait(cdp,'Dirty config prevents peer add/remove',state=>state.dirty&&state.buttons.filter(row=>['mcp-peer-add','mcp-peer-remove'].includes(row.action)).every(row=>row.disabled));
        await activate(input,cdp,'discard-config-draft');
        await wait(cdp,'Discard restores clean config',state=>!state.dirty&&!button(state,'mcp-peer-remove',BAD)?.disabled);
        await activate(input,cdp,'close-overlay');
        await wait(cdp,'Settings closes',state=>!state.panel);await open(input,cdp);
        state=await wait(cdp,'Reopened saved peers remain and token is not redisplayed',state=>peerRowsMatch(state,[GOOD,BAD])&&state.fields.token.value==='');
        await activate(input,cdp,'mcp-peer-remove',BAD);
        await wait(cdp,'Remove targets only the selected peer',state=>peerRowsMatch(state,[GOOD])&&!state.fields.id.disabled);
        await activate(input,cdp,'mcp-peer-remove',GOOD);
        await wait(cdp,'Last peer removes cleanly',state=>peerRowsMatch(state,[])&&!state.fields.id.disabled);
        await activate(input,cdp,'mcp-peer-refresh');
        await wait(cdp,'Empty refresh reflects persisted removal',state=>peerRowsMatch(state,[])&&!state.fields.id.disabled);
        await captureScenarioScreenshot({cdp,sink,name:'settings-mcp-peer-removed',owner:OWNER});
        const calls=(await commands.snapshot()).calls;
        if(calls.some(call=>['send_prompt','submit','save_global_config'].includes(call.command))) throw fail('mcp-peer-unexpected-command','Peer form must not submit chat or generic config save',calls.map(row=>row.command));
        await sink.record('mcp-peer-controls-completed',{fields:['id','url','token','certificate'],actions:['add','refresh','check','remove'],invalid:['empty','url','missing token','short token','invalid PEM','duplicate ID'],dirty_mutations_disabled:true,reopened_credentials_redacted:true,wire:provider.requestLedger,commands:calls.map(row=>({command:row.command,id:row.args.id??row.args.peer?.id??null}))},{phase:'executing',owner:OWNER});
        await activate(input,cdp,'close-overlay');
        acceptedLedger=structuredClone(provider.requestLedger);
        return {acquisition:'pass',oracle:'pass',manual:'not_required'};
      }catch(error){
        primary=error;
        try { await captureScenarioScreenshot({cdp,sink,name:'settings-mcp-peer-failure',owner:OWNER}); }
        catch(captureError) { cleanup.push({kind:'failure-capture-error',error:String(captureError)}); }
        throw error;
      }
      finally {
        const errors=[];
        try{cleanup.push({kind:'input',result:await input.cleanup()});}catch(error){errors.push(String(error));}
        try{cleanup.push({kind:'commands',result:await commands.remove()});}catch(error){errors.push(String(error));}
        if(errors.length){cleanup.push({kind:'errors',errors});if(primary===null)throw new Error(errors.join('; '));}
      }
    },
    async quiesce({inputs}) {if(quiesceOutcome===null)quiesceOutcome=await quiesceProviderResource({provider,acceptedLedger,inputs});return structuredClone(quiesceOutcome);},
    async cleanup(){return {input:cleanup.some(row=>row.errors?.length)||quiesceOutcome?.input!=='pass'?'fail':'pass',resources:cleanup};},
  });
}
