import path from "node:path";
import { mkdir, readFile } from "node:fs/promises";
import { isDeepStrictEqual } from "node:util";
import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { waitForSemanticTargetSettlement } from "../core/semantic_target_settlement.mjs";
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";
import { WebviewInput, assertTrustedProbeSequence, assertTrustedTextInsertion } from "../drivers/webview_input.mjs";
import { startScriptedProvider } from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { captureScenarioScreenshot, selectedNavigationIdentity } from "./observations.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { providerRestartFixtureConfig, quiesceProviderResource } from "./provider_restart.mjs";
import { completedSessionRootReady, observeSessionSettingsSurface, exactSessionProviderLedger } from "./settings_session.mjs";
import { trustedClick, trustedInsert } from "./side_chat_quote.mjs";

const OWNER = "scenario:navigation.session-management";
const SENTINEL = "E2E_MANAGEMENT_KEEP.txt";
const SENTINEL_TEXT = "Deleting a Desktop project must retain this workspace file.\n";
export const MANAGEMENT_TURNS = Object.freeze([
  { prompt: "management alpha marker", responseText: "ALPHA_MANAGEMENT_OK" },
  { prompt: "management beta marker", responseText: "BETA_MANAGEMENT_OK" },
  { prompt: "management quick marker", responseText: "QUICK_MANAGEMENT_OK" },
]);
const PROMPT = { selector: "textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
const SEARCH = { selector: "input#session-search", identity: { tag: "INPUT", id: "session-search" } };
const button = (action, prefix = "") => ({
  selector: `${prefix}button[data-action="${action}"]`, identity: { tag: "BUTTON", action },
});
const CONFIRM_PREFIX = '[role="alertdialog"][aria-labelledby="local-confirm-title"] ';
const MUTATIONS = ["archive_session", "unarchive_session", "delete_session", "delete_chat_session", "delete_project"];
const COMMANDS = [...MUTATIONS, "set_session_search", "set_session_search_include_archived"];
const ROWS = { project: "project_rows", session: "session_rows", "chat-session": "chat_session_rows" };

function fail(code, message, evidence) {
  throw new DesktopE2eError("product", `session-management-${code}`, message, evidence);
}

export function managementRowLocator(kind, id, action = kind) {
  if (!Object.hasOwn(ROWS, kind) || !/^[0-9A-HJKMNP-TV-Z]{26}$/.test(id)) throw new TypeError("invalid management row identity");
  const suffix = action === kind ? "select" : kind === "project" && action === "delete-project"
    ? "delete" : kind === "project" && action === "new-project-session" ? "new-session" : action;
  const focusKey = `${kind}:${id}:${suffix}`;
  return { selector: `button[data-action="${action}"][data-focus-key="${focusKey}"]`, identity: { tag: "BUTTON", action, focusKey } };
}

export function managementRowCommand(projection, kind, id, command) {
  const rows = projection?.[ROWS[kind]] ?? [];
  const matches = rows.map((row, index) => ({ row, index })).filter(({ row }) => (row.project_id ?? row.session_id) === id);
  if (matches.length !== 1) throw new TypeError("management command requires one current row");
  return {
    command,
    args: {
      index: matches[0].index,
      expectedTarget: {
        workspacePath: projection.workspace_path,
        ownerProjectId: projection.project_rows[projection.selected_project_index]?.project_id ?? null,
        // The public row command contract uses project session rows even for Quick Chat.
        ownerSessionId: projection.session_rows[projection.selected_session_index]?.session_id ?? null,
        rowId: id,
      },
    },
  };
}

export function managementSnapshot(surface) {
  const p = surface.projection;
  return {
    workspace: p.workspace_path, draft_target: p.draft_target, prompt: surface.prompt,
    projects: p.project_rows,
    sessions: p.session_rows.map(({ session_id, title, archived }) => ({ session_id, title, archived })),
    chats: p.chat_session_rows.map(({ session_id, title, archived }) => ({ session_id, title, archived })),
    history: p.transcript_rows.filter((row) => ["user", "assistant"].includes(row.row_kind))
      .map((row) => ({ kind:row.row_kind, identity:row.stable_history_identity, body:row.body })),
  };
}

export function managementRowsMatch(surface, kind, expectedIds, archived = {}) {
  const rows = surface?.projection?.[ROWS[kind]];
  if (!Array.isArray(rows) || !Array.isArray(surface?.rows)) return false;
  const idOf = (row) => row.project_id ?? row.session_id;
  const ids = rows.map(idOf);
  if (new Set(ids).size !== ids.length || !isDeepStrictEqual([...ids].sort(), [...expectedIds].sort())) return false;
  const dom = surface.rows.filter((row) => row.action === kind);
  if (dom.length !== rows.length) return false;
  return rows.every((row) => {
    const id = idOf(row);
    const matches = dom.filter((entry) => entry.focus_key === `${kind}:${id}:select`);
    return matches.length === 1 && matches[0].visible === true && matches[0].enabled === true
      && matches[0].title === row.label
      && (!Object.hasOwn(archived, id) || row.archived === archived[id]);
  });
}

export function managementSearchReady(surface, { query, matchedIds, before }) {
  const ownerId = before.draft_target.sessionId;
  const expectedIds = [...new Set([...matchedIds, ...(ownerId ? [ownerId] : [])])];
  const after = managementSnapshot(surface);
  return surface.search === query && surface.projection.session_search_text === query
    && after.workspace === before.workspace && after.prompt === before.prompt
    && isDeepStrictEqual(after.draft_target, before.draft_target)
    && isDeepStrictEqual(after.history, before.history)
    && managementRowsMatch(surface, "session", expectedIds);
}

export function managementConfirmationReady(surface, { title, detail, verb, confirmationAction }) {
  const dialog = surface?.confirmation;
  return surface?.errors === 0 && surface.dialog_count === 1 && dialog?.visible === true
    && dialog.aria_modal === "true" && dialog.focus_inside === true
    && dialog.title?.endsWith(`を${verb}しますか？`) === true
    && dialog.summary === title && dialog.target === detail
    && dialog.consequence?.includes("実ファイルは削除しません") === (verb !== "復元")
    && (verb !== "復元" || dialog.consequence?.includes("実ファイルは変更しません") === true)
    && dialog.actions.length === 2
    && dialog.actions.every((row) => row.enabled && row.visible)
    && dialog.actions.filter((row) => row.action === "cancel-local-confirm").length === 1
    && dialog.actions.filter((row) => row.action === confirmationAction).length === 1;
}

async function observeSurface(cdp) {
  return cdp.evaluate(`(async () => {
    const projection = await window.__TAURI_INTERNALS__.invoke('desktop_state');
    const visible = node => {
      if (!(node instanceof HTMLElement) || !node.isConnected) return false;
      const r=node.getBoundingClientRect(), s=getComputedStyle(node);
      return r.width>0 && r.height>0 && s.display!=='none' && s.visibility!=='hidden' && Number(s.opacity)!==0;
    };
    const dialogs=Array.from(document.querySelectorAll('[role="dialog"], [role="alertdialog"]')).filter(visible);
    const dialog=document.querySelector('[role="alertdialog"][aria-labelledby="local-confirm-title"]');
    const text=node=>node?.textContent?.trim() ?? null;
    return {
      projection, prompt:document.querySelector('#prompt')?.value ?? null,
      rows:Array.from(document.querySelectorAll('button.nav-row[data-focus-key]')).map(node=>({
        action:node.dataset.action, focus_key:node.dataset.focusKey, title:text(node.querySelector('.nav-title')),
        visible:visible(node), enabled:!node.disabled, selected:node.getAttribute('aria-current')==='page'
      })),
      search:document.querySelector('#session-search')?.value ?? null,
      archived_filter:document.querySelector('[data-action="toggle-session-archived-search"]')?.classList.contains('selected') ?? null,
      dialog_count:dialogs.length,
      errors:Array.from(document.querySelectorAll('.fatal, .ui-error-notice')).filter(visible).length,
      confirmation:!dialog?null:{
        visible:visible(dialog), aria_modal:dialog.getAttribute('aria-modal'), focus_inside:dialog.contains(document.activeElement),
        title:text(dialog.querySelector('h2')), summary:text(dialog.querySelector('.confirm-summary')),
        target:text(dialog.querySelector('dd')), consequence:text(dialog.querySelectorAll('dd')[1]),
        actions:Array.from(dialog.querySelectorAll('button[data-action]')).map(node=>({action:node.dataset.action,visible:visible(node),enabled:!node.disabled}))
      }
    };
  })()`);
}

function idle(surface) {
  const p=surface?.projection;
  return surface?.errors===0 && p?.overlay==="none" && p.busy===false
    && p.navigation_loading===false && p.background_mutation_pending===false
    && p.async_polling_required===false && p.post_run_refresh_pending===false;
}

async function waitSurface(cdp, sink, label, accept) {
  try {
    const result=await waitForObservation({ label, timeoutMs:15_000, pollMs:60,
      sample:()=>observeSurface(cdp), accept:value=>idle(value) && accept(value), retrySampleErrors:false });
    await sink.record("management-observed", { label, surface:result.value }, {phase:"executing",owner:OWNER});
    return result.value;
  } catch(error) {
    if(error?.code==="observation-timeout" && !error.evidence?.last_error) fail("state",label,error.evidence);
    throw error;
  }
}

async function settleTarget(input, locator, sink) {
  // Rust settlement can precede DOM availability, and hovered actions fade in.
  // Acquire the exact visible/enabled DOM owner before each trusted input group.
  const settled = await waitForSemanticTargetSettlement({input,locator,label:"management action visible and enabled",timeoutMs:2_000});
  if(settled.value.classified.decision!=="pass") {
    throw new DesktopE2eError("harness","management-action","Management action is not exact and actionable",settled.value);
  }
  await sink.record("management-action-settled",settled.value,{phase:"executing",owner:OWNER});
}

async function click(input, locator, sink) {
  await settleTarget(input,locator,sink);
  const proof=await trustedClick(input, locator);
  await sink.record("management-trusted-click",{locator,proof},{phase:"executing",owner:OWNER});
}

async function hoverRow(input, kind, id, sink) {
  const locator=managementRowLocator(kind,id);
  await settleTarget(input,locator,sink);
  const start=(await input.snapshotProbe()).sequence;
  const result=await input.hover(locator);
  const proof=assertTrustedProbeSequence(await input.snapshotProbe(start),{afterSequence:start,
    expected:[{type:"pointermove",identity:locator.identity,buttons:0}]});
  await sink.record("management-trusted-row-hover",{locator,result,proof},{phase:"executing",owner:OWNER});
}

async function insert(input,locator,text,sink) {
  await settleTarget(input,locator,sink);
  const proof=await trustedInsert(input,locator,text);
  await sink.record("management-trusted-insert",{locator,text,proof},{phase:"executing",owner:OWNER});
  return proof;
}

export function managementFocusedTarget(snapshot,identity) {
  return snapshot?.active != null && Object.entries(identity).every(([field,value])=>snapshot.active[field]===value);
}

async function requireKeyboardTarget(input,locator,sink) {
  await settleTarget(input,locator,sink);
  const snapshot=await input.snapshotProbe();
  if(!managementFocusedTarget(snapshot,locator.identity)) {
    throw new DesktopE2eError("product","management-keyboard-focus","Expected management control does not own keyboard focus",{locator,snapshot});
  }
  return snapshot;
}

async function commandsExactly(probe, start, expected, sink) {
  const result=assertExactDesktopCommandSequence(await probe.snapshot(start),{afterSequence:start,expected});
  await sink.record("management-command-proof",result,{phase:"executing",owner:OWNER});
}

async function replaceSearch(input, text, sink) {
  await click(input,SEARCH,sink);
  const start=(await requireKeyboardTarget(input,SEARCH,sink)).sequence;
  await input.keyDown("Control");
  try { await input.pressKey("a"); } finally { await input.keyUp("Control"); }
  assertTrustedProbeSequence(await input.snapshotProbe(start),{afterSequence:start,expected:[
    {type:"keydown",identity:SEARCH.identity,key:"Control",code:"ControlLeft"},
    {type:"keydown",identity:SEARCH.identity,key:"a",code:"KeyA"},
    {type:"keyup",identity:SEARCH.identity,key:"a",code:"KeyA"},
    {type:"keyup",identity:SEARCH.identity,key:"Control",code:"ControlLeft"},
  ]});
  const inputStart=(await requireKeyboardTarget(input,SEARCH,sink)).sequence;
  if(text) {
    await input.insertText(SEARCH,text);
    assertTrustedTextInsertion(await input.snapshotProbe(inputStart),{afterSequence:inputStart,identity:SEARCH.identity,text});
  } else {
    await input.pressKey("Backspace");
    assertTrustedProbeSequence(await input.snapshotProbe(inputStart),{afterSequence:inputStart,expected:[
      {type:"keydown",identity:SEARCH.identity,key:"Backspace",code:"Backspace"},
      {type:"input",identity:SEARCH.identity,inputType:"deleteContentBackward"},
      {type:"keyup",identity:SEARCH.identity,key:"Backspace",code:"Backspace"},
    ]});
  }
  await sink.record("management-search-input",{text,probe:await input.snapshotProbe(start)},{phase:"executing",owner:OWNER});
}

async function createRoot({input,cdp,provider,sink,index}) {
  const turn=MANAGEMENT_TURNS[index];
  const proof=await insert(input,PROMPT,turn.prompt,sink);
  await sink.record("management-root-prompt",{turn,proof},{phase:"executing",owner:OWNER});
  await click(input,button("send","section.composer "),sink);
  const result=await waitForObservation({label:`management root ${index+1}`,timeoutMs:90_000,pollMs:100,retrySampleErrors:false,
    sample:()=>observeSessionSettingsSurface(cdp),
    accept:surface=>completedSessionRootReady(surface,provider.requestLedger,{prompt:turn.prompt,response:turn.responseText,responseCount:index+1})});
  return result.value.projection;
}

async function confirmation({input,cdp,probe,sink,kind,id,action,command,verb,commit,escape=false}) {
  const before=await waitSurface(cdp,sink,`${action} before`,value=>value.dialog_count===0);
  const expected=managementRowCommand(before.projection,kind,id,command);
  const row=before.projection[ROWS[kind]][expected.args.index];
  const start=(await probe.snapshot()).sequence;
  await hoverRow(input,kind,id,sink);
  await click(input,managementRowLocator(kind,id,action),sink);
  const confirmationAction=command.includes("archive")?"confirm-local-archive-state":"confirm-local-delete";
  await waitSurface(cdp,sink,`${action} exact confirmation`,value=>managementConfirmationReady(value,{
    title:row.label,detail:kind==="project"?row.path:id,verb,confirmationAction,
  }) && isDeepStrictEqual(managementSnapshot(value),managementSnapshot(before)));
  await commandsExactly(probe,start,[],sink);
  await captureScenarioScreenshot({cdp,sink,name:`management-${action}-${commit?"confirm":"cancel"}`,owner:OWNER});
  if(escape) {
    const cancelTarget=button("cancel-local-confirm",CONFIRM_PREFIX);
    const inputStart=(await requireKeyboardTarget(input,cancelTarget,sink)).sequence;
    await input.pressKey("Escape");
    assertTrustedProbeSequence(await input.snapshotProbe(inputStart),{afterSequence:inputStart,expected:[
      {type:"keydown",key:"Escape",code:"Escape",identity:cancelTarget.identity}, {type:"keyup",key:"Escape",code:"Escape"},
    ]});
  } else await click(input,button(commit?confirmationAction:"cancel-local-confirm",CONFIRM_PREFIX),sink);
  const after=await waitSurface(cdp,sink,`${action} ${commit?"committed":"cancelled"}`,value=>value.dialog_count===0
    && (commit || isDeepStrictEqual(managementSnapshot(value),managementSnapshot(before))));
  await commandsExactly(probe,start,commit?[expected]:[],sink);
  return after;
}

export function createSessionManagementScenario() {
  const state={provider:null,acceptedLedger:null,quiesce:null,resources:[]};
  return Object.freeze({
    id:"navigation.session-management",productOracle:"pass",manualGate:"pending",databaseRequired:true,requestGracefulExit,
    async prepare({context,sink,phase}) {
      state.provider=await startScriptedProvider({turns:MANAGEMENT_TURNS.map(turn=>({...turn}))});
      await prepareDesktopFixture({context,sink,phase,owner:OWNER,configText:providerRestartFixtureConfig(state.provider.baseUrl),
        sentinelName:SENTINEL,sentinelText:SENTINEL_TEXT});
      // Keep project/command discovery inside this execution, as the palette fixture does.
      await mkdir(path.join(context.paths.workspace,".git"));
      await sink.record("management-scope",{
        setup:"config, empty workspace sentinel and isolated .git boundary only; all durable conversations created by GUI submit",
        excluded:["rename: current Desktop has no project/session rename action","folder-picker creation","restart persistence","real model","physical peer","running-session destructive action","rollback/fork"],
        manual_review:"Screenshots and a separate direct visual receipt required; machine assertions do not certify all action states.",
      },{phase,owner:OWNER});
    },
    async execute({context,driver:cdp,sink}) {
      const input=new WebviewInput(cdp,{probeId:"session-management"});
      const probe=new DesktopCommandProbe(cdp,{probeId:"session-management",commands:COMMANDS});
      let primaryError=null;
      try {
        await acquireInteractiveShell({context,driver:cdp,sink},{evidenceOwner:OWNER,screenshotStem:"management-shell-ready"});
        if(state.provider.requestLedger.length!==0) fail("cold-network","Unexpected provider request before GUI submit",state.provider.requestLedger);
        await input.installProbe(); await probe.install();
        const alpha=await createRoot({input,cdp,provider:state.provider,sink,index:0});
        const alphaId=selectedNavigationIdentity(alpha).session_id, projectId=selectedNavigationIdentity(alpha).project_id;
        if(!projectId || !alphaId) fail("alpha-owner","GUI-created alpha has no project/session owner",alpha);
        const projectPath=alpha.project_rows.find(row=>row.project_id===projectId)?.path;
        if(typeof projectPath!=="string" || path.resolve(projectPath)!==path.resolve(context.paths.workspace)) {
          fail("fixture-project-boundary","Management must target only the isolated execution workspace",{projectPath,workspace:context.paths.workspace});
        }
        await hoverRow(input,"project",projectId,sink);
        await click(input,managementRowLocator("project",projectId,"new-project-session"),sink);
        await waitSurface(cdp,sink,"fresh project session",value=>value.dialog_count===0 && value.projection.draft_target.sessionId===null
          && value.projection.thread_empty===true && value.prompt==="");
        const beta=await createRoot({input,cdp,provider:state.provider,sink,index:1});
        const betaId=selectedNavigationIdentity(beta).session_id;
        if(!betaId || betaId===alphaId || selectedNavigationIdentity(beta).project_id!==projectId) fail("beta-owner","Second root is not distinct in the same project",beta);
        await waitSurface(cdp,sink,"two created project sessions",value=>managementRowsMatch(value,"session",[alphaId,betaId]));
        const search=async(text,ids)=>{
          const before=await observeSurface(cdp), start=(await probe.snapshot()).sequence;
          await replaceSearch(input,text,sink);
          const searched = await waitSurface(cdp,sink,`search ${text||"clear"}`,value=>value.dialog_count===0
            && managementSearchReady(value,{query:text,matchedIds:ids,before:managementSnapshot(before)}));
          await commandsExactly(probe,start,[{command:"set_session_search",args:{text,expectedTarget:{workspacePath:before.projection.workspace_path,projectId}}}],sink);
          await sink.record("management-search-scope",{query:text,matched_session_ids:ids,
            retained_open_session_id:before.projection.draft_target.sessionId,
            rendered_session_ids:searched.projection.session_rows.map(row=>row.session_id),
            contract:"The current open session remains in navigation even when it does not match; extra unrelated rows fail.",
          },{phase:"executing",owner:OWNER});
          await captureScenarioScreenshot({cdp,sink,name:`management-search-${text?ids.length?"match":"no-match":"clear"}`,owner:OWNER});
        };
        await search("no-management-match-zzzz",[]);
        await search("alpha",[alphaId]);
        await search("",[alphaId,betaId]);
        const common={input,cdp,probe,sink};
        const archive={...common,kind:"session",id:alphaId,action:"archive-session",command:"archive_session",verb:"アーカイブ"};
        await confirmation({...archive,commit:false});
        await confirmation({...archive,commit:true});
        await waitSurface(cdp,sink,"archived alpha hidden",value=>managementRowsMatch(value,"session",[betaId]));
        const toggle=async(includeArchived,ids,archived={})=>{
          const before=await observeSurface(cdp), start=(await probe.snapshot()).sequence;
          await click(input,button("toggle-session-archived-search"),sink);
          await waitSurface(cdp,sink,`archive filter ${includeArchived}`,value=>value.dialog_count===0
            && value.projection.session_search_include_archived===includeArchived && value.archived_filter===includeArchived
            && value.search==="" && managementRowsMatch(value,"session",ids,archived));
          await commandsExactly(probe,start,[{command:"set_session_search_include_archived",args:{includeArchived,
            expectedTarget:{workspacePath:before.projection.workspace_path,projectId}}}],sink);
        };
        await toggle(true,[alphaId,betaId],{[alphaId]:true,[betaId]:false});
        await confirmation({...common,kind:"session",id:alphaId,action:"unarchive-session",command:"unarchive_session",verb:"復元",commit:true});
        await toggle(false,[alphaId,betaId],{[alphaId]:false,[betaId]:false});
        await click(input,managementRowLocator("session",alphaId),sink);
        await waitSurface(cdp,sink,"restored alpha canonical history",value=>value.projection.draft_target.sessionId===alphaId
          && isDeepStrictEqual(managementSnapshot(value).history,managementSnapshot({projection:alpha,prompt:""}).history));
        await insert(input,PROMPT,"削除確認の取消で残す未送信メモ",sink);
        const deleteAlpha={...common,kind:"session",id:alphaId,action:"delete-session",command:"delete_session",verb:"削除"};
        await confirmation({...deleteAlpha,commit:false,escape:true});
        await confirmation({...deleteAlpha,commit:true});
        await waitSurface(cdp,sink,"only beta survives session deletion",value=>managementRowsMatch(value,"session",[betaId]));
        await click(input,button("new-chat",".rail-section "),sink);
        await waitSurface(cdp,sink,"fresh Quick Chat",value=>value.projection.draft_target.sessionId===null
          && value.projection.selected_project_index<0 && value.prompt==="" && value.projection.thread_empty===true);
        const quick=await createRoot({input,cdp,provider:state.provider,sink,index:2});
        const quickId=selectedNavigationIdentity(quick).session_id;
        if(!quickId || selectedNavigationIdentity(quick).project_id!==null) fail("quick-owner","Quick Chat did not use projectless navigation",quick);
        const deleteQuick={...common,kind:"chat-session",id:quickId,action:"delete-chat-session",command:"delete_chat_session",verb:"削除"};
        await confirmation({...deleteQuick,commit:false});
        await confirmation({...deleteQuick,commit:true});
        await waitSurface(cdp,sink,"Quick Chat deleted",value=>managementRowsMatch(value,"chat-session",[]));
        await click(input,managementRowLocator("project",projectId),sink);
        await waitSurface(cdp,sink,"beta remains before project deletion",value=>selectedNavigationIdentity(value.projection).project_id===projectId
          && managementRowsMatch(value,"session",[betaId]));
        const deleteProject={...common,kind:"project",id:projectId,action:"delete-project",command:"delete_project",verb:"削除"};
        await confirmation({...deleteProject,commit:false});
        await confirmation({...deleteProject,commit:true});
        await waitSurface(cdp,sink,"project and its session removed",value=>!value.projection.project_rows.some(row=>row.project_id===projectId)
          && !value.rows.some(row=>row.focus_key===`project:${projectId}:select` || row.focus_key===`session:${betaId}:select`));
        const sentinel=await readFile(path.join(context.paths.workspace,SENTINEL),"utf8");
        if(sentinel!==SENTINEL_TEXT) fail("workspace-file","Project deletion changed the workspace sentinel",{sentinel});
        if(!exactSessionProviderLedger(state.provider.requestLedger,3)) fail("provider-count","Management actions generated extra provider work",state.provider.requestLedger);
        state.acceptedLedger=structuredClone(state.provider.requestLedger);
        await captureScenarioScreenshot({cdp,sink,name:"management-final",owner:OWNER});
        await sink.record("management-complete",{projectId,alphaId,betaId,quickId,sentinel_retained:true,ledger:state.acceptedLedger},{phase:"executing",owner:OWNER});
        return {acquisition:"pass",oracle:"pass",manual:"pending"};
      } catch(error) {primaryError=error;throw error;}
      finally {
        const cleanup={failures:[]};
        try {cleanup.input=await input.cleanup();} catch(error) {cleanup.failures.push({owner:"input",message:error.message});}
        try {cleanup.commands=await probe.remove();} catch(error) {cleanup.failures.push({owner:"commands",message:error.message});}
        state.resources.push(cleanup);
        if(cleanup.failures.length && !primaryError) throw new DesktopE2eError("harness","management-probe-cleanup","Management probes did not settle",cleanup);
      }
    },
    async quiesce({inputs}) {
      state.quiesce??=await quiesceProviderResource({provider:state.provider,acceptedLedger:state.acceptedLedger,inputs});
      return structuredClone(state.quiesce);
    },
    async cleanup() {
      return {input:state.quiesce?.input==="pass" && state.resources.every(row=>row.failures.length===0)?"pass":"fail",
        resources:[{kind:"session-management",quiesce:state.quiesce,probes:state.resources}]};
    },
  });
}
