import path from "node:path";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion } from "../drivers/webview_input.mjs";
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";
import { acquireInteractiveShell, prepareShellBaseline, quiesceShellBaseline, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { trustedClick } from "./side_chat_quote.mjs";

const OWNER = "scenario:navigation.workspace-controls";
const DIALOG = '[role="dialog"][aria-labelledby="workspace-dialog-title"]';
const button = (action, prefix = "") => ({ selector: `${prefix}button[data-action="${action}"]`, identity: { tag: "BUTTON", action } });
const PATH_INPUT = { selector: `${DIALOG} input#workspace-input`, identity: { tag: "INPUT", id: "workspace-input" } };
const SEARCH = { selector: "input#local-search", identity: { tag: "INPUT", id: "local-search" } };
const PROMPT = { selector: "textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
const canonicalPath = value => path.resolve(value).toLowerCase();

export function workspaceSelectionSucceeded(surface, destination) {
  const prefix = "workspace set to ";
  return surface.fatal===0 && surface.errors.length===0 && !surface.dialog && surface.p.overlay==="none"
    && !surface.p.navigation_loading && surface.p.navigation_admission_open
    && canonicalPath(surface.p.workspace_path)===canonicalPath(destination)
    && surface.p.status_message.startsWith(prefix)
    && canonicalPath(surface.p.status_message.slice(prefix.length))===canonicalPath(destination);
}

async function observeWorkspaceSuccessStability(cdp,sink,label,destination) {
  const started=performance.now(),samples=[];
  do {
    const value=await observe(cdp);
    samples.push({elapsed_ms:Math.round(performance.now()-started),workspace:value.p.workspace_path,status:value.p.status_message});
    if(!workspaceSelectionSucceeded(value,destination))throw new DesktopE2eError("product","workspace-success-status-lost",
      "Successful workspace selection must retain its success status across ordinary refreshes",{label,destination,samples,surface:value});
    if(performance.now()-started>=1200)break;
    await new Promise(resolve=>setTimeout(resolve,80));
  } while(true);
  await sink.record("workspace-success-stable",{label,destination,minimum_ms:1200,samples},{phase:"executing",owner:OWNER});
}

async function observe(cdp) {
  return cdp.evaluate(`(async () => {
    const p = await window.__TAURI_INTERNALS__.invoke('desktop_state');
    await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const d = document.querySelector(${JSON.stringify(DIALOG)});
    const visible = e => Boolean(e?.isConnected && e.getBoundingClientRect().width && e.getBoundingClientRect().height && getComputedStyle(e).visibility !== 'hidden');
    return { p, dialog:visible(d), path:d?.querySelector('input')?.value ?? null,
      feedback:visible(d?.querySelector('#workspace-feedback')) ? d.querySelector('#workspace-feedback').textContent.trim() : null,
      prompt:document.querySelector('#prompt')?.value ?? null,
      focus:document.activeElement?.id ?? null, focusInDialog:Boolean(d?.contains(document.activeElement)),
      errors:[...document.querySelectorAll('.ui-error-notice')].filter(visible).map(e=>e.textContent.trim()),
      fatal:[...document.querySelectorAll('.fatal')].filter(visible).length,
      actions:[...(d?.querySelectorAll('button[data-action]')??[])].map(e=>({action:e.dataset.action,disabled:e.disabled,visible:visible(e)})),
      palette:[...document.querySelectorAll('.command button[data-action]')].map(e=>e.dataset.action)
    };
  })()`);
}

async function wait(cdp, sink, label, accept) {
  try {
    const result = await waitForObservation({label,timeoutMs:15_000,pollMs:60,retrySampleErrors:false,
      sample:()=>observe(cdp),accept:v=>v.fatal===0 && accept(v)});
    await sink.record("workspace-control-observed",{label,surface:result.value},{phase:"executing",owner:OWNER});
    return result.value;
  } catch(error) {
    if(error?.code!=="observation-timeout" || error.evidence?.last_error) throw error;
    throw new DesktopE2eError("product","workspace-control-result",label,error.evidence);
  }
}

async function click(input, locator, sink) {
  await sink.record("workspace-control-click",{locator,proof:await trustedClick(input,locator)},{phase:"executing",owner:OWNER});
}

async function replace(input, locator, text, sink) {
  await click(input,locator,sink);
  const before = (await input.snapshotProbe()).sequence;
  await input.keyDown("Control");
  try { await input.pressKey("a"); } finally { await input.keyUp("Control"); }
  await input.pressKey("Backspace");
  const clear = assertTrustedProbeSequence(await input.snapshotProbe(before),{afterSequence:before,expected:[
    {type:"keydown",identity:locator.identity,key:"Control"}, {type:"keydown",identity:locator.identity,key:"a"},
    {type:"keyup",identity:locator.identity,key:"a"}, {type:"keyup",identity:locator.identity,key:"Control"},
    {type:"keydown",identity:locator.identity,key:"Backspace"}, {type:"keyup",identity:locator.identity,key:"Backspace"},
  ]});
  let insert = null;
  if(text) {
    const start=(await input.snapshotProbe()).sequence;
    await input.insertText(locator,text);
    insert=assertTrustedTextInsertion(await input.snapshotProbe(start),{afterSequence:start,identity:locator.identity,text});
  }
  await sink.record("workspace-control-text",{locator,text,clear,insert},{phase:"executing",owner:OWNER});
}

async function openDialog(input,cdp,sink) {
  await click(input,button("show-command-palette","section.composer "),sink);
  await wait(cdp,sink,"palette opened",v=>v.p.overlay==="command_palette");
  await replace(input,SEARCH,"ワークスペースを切り替え",sink);
  await wait(cdp,sink,"workspace picker action found",v=>v.palette.includes("show-workspace-picker"));
  await click(input,button("show-workspace-picker",".command "),sink);
  return wait(cdp,sink,"workspace dialog with four enabled controls",v=>v.dialog && v.focusInDialog
    && v.actions.length===4 && v.actions.every(a=>a.visible&&!a.disabled));
}

export function createWorkspaceControlsScenario() {
  const state={alternate:null,file:null,resources:[]};
  return Object.freeze({id:"navigation.workspace-controls",productOracle:"pass",manualGate:"pending",databaseRequired:true,
    requestGracefulExit,quiesce:quiesceShellBaseline,
    async prepare(args) {
      await prepareShellBaseline(args);
      await mkdir(path.join(args.context.paths.workspace,".git"));
      state.alternate=path.join(args.context.root,"日本語 別プロジェクト");
      await mkdir(path.join(state.alternate,".git"),{recursive:true});
      state.file=path.join(state.alternate,"keep.txt");
      await writeFile(state.file,"WORKSPACE_CONTROLS_KEEP\n",{flag:"wx"});
    },
    async cleanup(){return {input:state.resources.every(r=>r.pass)?"pass":"fail",resources:state.resources};},
    async execute({context,driver:cdp,sink}) {
      await acquireInteractiveShell({context,driver:cdp,sink},{evidenceOwner:OWNER,screenshotStem:"workspace-ready"});
      const input=new WebviewInput(cdp,{probeId:"workspace-controls"});
      const probe=new DesktopCommandProbe(cdp,{probeId:"workspace-controls",commands:["switch_workspace"]});
      try {
        await input.installProbe(); await probe.install();
        await replace(input,PROMPT,"workspace original draft",sink);
        const baseline=await wait(cdp,sink,"original draft",v=>v.prompt==="workspace original draft");
        const opened=await openDialog(input,cdp,sink);
        if(canonicalPath(opened.path)!==canonicalPath(baseline.p.workspace_path)) throw new DesktopE2eError("product","workspace-initial-path","Workspace dialog did not show current path",opened);
        for(const [name,text,reason] of [["empty","","workspace path is empty"],["missing",path.join(context.root,"does-not-exist"),"workspace path is not accessible"],["file",state.file,"workspace path is not a directory"]]) {
          await replace(input,PATH_INPUT,text,sink);
          const start=(await probe.snapshot()).sequence;
          await click(input,button("switch-workspace",`${DIALOG} `),sink);
          const rejected=await wait(cdp,sink,`${name} workspace rejected`,v=>v.dialog && v.feedback?.includes(reason)
            && v.p.status_message.includes(reason) && canonicalPath(v.p.workspace_path)===canonicalPath(baseline.p.workspace_path)
            && v.prompt===baseline.prompt && !v.p.navigation_loading);
          const exact=assertExactDesktopCommandSequence(await probe.snapshot(start),{afterSequence:start,expected:[{
            command:"switch_workspace",args:{text,expectedTarget:baseline.p.draft_target}
          }]});
          await sink.record("workspace-rejection",{name,exact,surface:rejected},{phase:"executing",owner:OWNER});
          await captureScenarioScreenshot({cdp,sink,name:`workspace-invalid-${name}`,owner:OWNER});
        }
        await replace(input,PATH_INPUT,state.alternate,sink);
        await input.pressKey("Escape");
        await wait(cdp,sink,"workspace edit cancelled",v=>!v.dialog && v.p.overlay==="none"
          && canonicalPath(v.p.workspace_path)===canonicalPath(baseline.p.workspace_path) && v.prompt===baseline.prompt);
        const reopened=await openDialog(input,cdp,sink);
        if(canonicalPath(reopened.path)!==canonicalPath(baseline.p.workspace_path)) throw new DesktopE2eError("product","workspace-cancel-reset","Cancelled path became current",reopened);
        for(const [label,destination] of [["alternate",state.alternate],["original",baseline.p.workspace_path]]) {
          await replace(input,PATH_INPUT,destination,sink);
          await click(input,button("switch-workspace",`${DIALOG} `),sink);
          await wait(cdp,sink,`${label} workspace selected`,v=>workspaceSelectionSucceeded(v,destination));
          await observeWorkspaceSuccessStability(cdp,sink,label,destination);
          await captureScenarioScreenshot({cdp,sink,name:`workspace-selected-${label}`,owner:OWNER});
          if(label==="alternate") await openDialog(input,cdp,sink);
        }
        if(await readFile(state.file,"utf8")!=="WORKSPACE_CONTROLS_KEEP\n") throw new DesktopE2eError("product","workspace-file-changed","Workspace selection changed fixture file",{path:state.file});
        return {acquisition:"pass",oracle:"pass",manual:"pending"};
      } finally {
        for(const [owner,resource] of [["desktop-command-probe",probe],["webview-input",input]]) {
          try {state.resources.push({owner,pass:true,result:await (owner === "desktop-command-probe" ? resource.remove() : resource.cleanup())});}
          catch(error){state.resources.push({owner,pass:false,message:error.message});}
        }
      }
    }
  });
}
