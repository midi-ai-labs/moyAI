import { createHash } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import { waitForObservation } from "../core/deadline.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { waitForSemanticTargetSettlement } from "../core/semantic_target_settlement.mjs";
import { canonicalUlid } from "../core/canonical_identity.mjs";
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from "../drivers/desktop_command_probe.mjs";
import { WebviewInput, assertTrustedProbeSequence } from "../drivers/webview_input.mjs";
import { startScriptedProvider } from "../drivers/scripted_provider.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { acquireInteractiveShell, requestGracefulExit } from "./shell_baseline.mjs";
import { captureScenarioScreenshot } from "./observations.mjs";
import { providerRestartFixtureConfig, quiesceProviderResource } from "./provider_restart.mjs";
import { trustedClick, trustedInsert } from "./side_chat_quote.mjs";
import { exerciseRawReviewInteraction } from "./prompt_review_raw_interaction.mjs";

export const REVIEW_SUBMIT_RAW = "review original request marker";
export const REVIEW_SUBMIT_PROPOSAL = "review proposed request marker";
export const REVIEW_SUBMIT_EDITED = "review edited request marker";
export const REVIEW_SUBMIT_REPLY = "REVIEW_SUBMIT_COMPLETE";
const PROMPT = { selector: "textarea#prompt", identity: { tag: "TEXTAREA", id: "prompt" } };
const DRAFT = { selector: "textarea#review-draft", identity: { tag: "TEXTAREA", id: "review-draft" } };
const REVIEW = '[role="dialog"][aria-labelledby="prompt-review-dialog-title"]';
const action = (id, prefix = `${REVIEW} `) => ({ selector: `${prefix}button[data-action="${id}"]`, identity: { tag: "BUTTON", action: id } });
const hash = value => createHash("sha256").update(value).digest("hex");

function fail(code, message, evidence) { return new DesktopE2eError("product", `review-submit-${code}`, message, evidence); }

export function reviewSubmitLedgerValid(ledger, prompts) {
  if (!Array.isArray(ledger) || !Array.isArray(prompts)) return false;
  if (ledger.some(row => row?.route !== "responses" && row?.route !== "models")) return false;
  if (ledger.some(row => row.route === "models" && (row.method !== "GET" || row.pathname !== "/v1/models" || row.response_status !== 200))) return false;
  const rows = ledger.filter(row => row.route === "responses");
  return rows.length === prompts.length && rows.every((row, index) => row.method === "POST"
    && row.pathname === "/v1/responses" && row.response_status === 200 && row.response_phase === "completed"
    && row.contract?.pass === true && row.contract.input_text_sha256 === hash(prompts[index]));
}

function samePreRunOwner(surface, expected) {
  const p = surface?.projection;
  return p?.workspace_path === expected.workspacePath
    && isDeepStrictEqual(p.draft_target, expected.draftTarget)
    && isDeepStrictEqual(p.run_target, expected.runTarget)
    && p.composer_commit_generation === expected.composerCommitGeneration
    && p.run_status_key === "idle" && p.busy === false && p.navigation_loading === false
    && p.background_mutation_pending === false && p.async_polling_required === false
    && p.transcript_rows.filter(row => ["user", "assistant"].includes(row.row_kind)).length === 0;
}

export function reviewSubmitOpenedFailures(surface, expected, draft) {
  const p = surface?.projection;
  const failures = [];
  if (!samePreRunOwner(surface, expected)) failures.push("pre-run-owner");
  if (surface?.errors !== 0) failures.push("visible-error");
  if (p?.overlay !== "prompt_review" || surface?.dialog_count !== 1 || !surface?.dialog_visible || !surface?.shell_inert) failures.push("dialog");
  if (p?.review_target === null || p?.review_target?.workspacePath !== expected.workspacePath
    || p?.review_target?.sessionId !== expected.draftTarget.sessionId
    || p?.review_target?.ownerGeneration !== expected.draftTarget.ownerGeneration) failures.push("review-target");
  if (p?.review_raw_text !== REVIEW_SUBMIT_RAW || surface?.raw !== REVIEW_SUBMIT_RAW) failures.push("raw-text");
  if (surface?.draft?.value !== draft || !surface?.draft?.visible || !surface?.draft?.enabled) failures.push("edited-draft");
  if (surface?.prompt?.value !== REVIEW_SUBMIT_RAW) failures.push("main-draft");
  const wanted = { "cancel-review": true, "send-review-raw": true, "send-review-enhanced": draft.trim().length > 0 };
  for (const [id, enabled] of Object.entries(wanted)) {
    const buttons = surface?.buttons?.filter(button => button.action === id) ?? [];
    if (buttons.length !== 1 || buttons[0].visible !== true || buttons[0].enabled !== enabled) failures.push(`button:${id}`);
  }
  return failures;
}

export function reviewSubmitCancelledFailures(surface, expected) {
  const p = surface?.projection;
  const failures = [];
  if (!samePreRunOwner(surface, expected)) failures.push("cancel-owner");
  if (surface?.errors !== 0) failures.push("visible-error");
  if (p?.overlay !== "none" || p?.review_target !== null || surface?.dialog_count !== 0 || surface?.shell_inert) failures.push("cancel-dialog");
  if (p?.review_raw_text !== "" || p?.review_draft_text !== "") failures.push("cancel-review-state");
  if (surface?.prompt?.value !== REVIEW_SUBMIT_RAW || !surface?.prompt?.enabled) failures.push("cancel-main-draft");
  return failures;
}

export function reviewSubmitCompletedFailures(surface, ledger, { choice, workspacePath }) {
  const dispatch = choice === "raw" ? REVIEW_SUBMIT_RAW : REVIEW_SUBMIT_EDITED;
  const p = surface?.projection;
  const failures = [];
  if (!reviewSubmitLedgerValid(ledger, [REVIEW_SUBMIT_RAW, REVIEW_SUBMIT_RAW, dispatch])) failures.push("provider-inputs");
  if (surface?.errors !== 0) failures.push("visible-error");
  if (p?.workspace_path !== workspacePath || !canonicalUlid(p?.draft_target?.sessionId)) failures.push("completed-owner");
  if (p?.run_status_key !== "completed" || p?.task_activity_state !== "idle" || p?.busy !== false
    || p?.agent_tree_active !== false || p?.navigation_loading !== false || p?.post_run_refresh_pending !== false
    || p?.background_mutation_pending !== false || p?.async_polling_required !== false) failures.push("terminal");
  if (p?.overlay !== "none" || p?.review_target !== null || surface?.dialog_count !== 0 || surface?.shell_inert) failures.push("completed-dialog");
  if (surface?.prompt?.value !== "" || !surface?.prompt?.enabled || surface?.stop_visible) failures.push("completed-composer");
  const bodies = kind => (p?.transcript_rows ?? []).filter(row => row.row_kind === kind).map(row => row.body);
  if (!isDeepStrictEqual(bodies("user"), [dispatch])) failures.push("canonical-user");
  if (!isDeepStrictEqual(bodies("assistant"), [REVIEW_SUBMIT_REPLY])) failures.push("canonical-assistant");
  if (!surface?.main_text?.includes(dispatch) || !surface?.main_text?.includes(REVIEW_SUBMIT_REPLY)) failures.push("rendered-history");
  return failures;
}

async function observe(cdp) {
  return cdp.evaluate(`(async () => {
    const projection=await window.__TAURI_INTERNALS__.invoke('desktop_state');
    const visible=node=>{
      if(!(node instanceof HTMLElement)||!node.isConnected)return false;
      const r=node.getBoundingClientRect(),s=getComputedStyle(node);
      return r.width>0&&r.height>0&&s.display!=='none'&&s.visibility!=='hidden'&&Number(s.opacity)!==0;
    };
    const enabled=node=>node instanceof HTMLElement&&!node.matches(':disabled')&&node.getAttribute('aria-disabled')!=='true'&&!node.closest('[inert]');
    const field=id=>{const n=document.querySelector('#'+id);return {value:n?.value??null,visible:visible(n),enabled:enabled(n),focused:document.activeElement===n,selectionStart:n?.selectionStart??null,selectionEnd:n?.selectionEnd??null};};
    const dialogs=Array.from(document.querySelectorAll(${JSON.stringify(REVIEW)}));
    const dialog=dialogs.length===1?dialogs[0]:null;
    const shell=document.querySelector('.app-frame > .shell');
    return {projection, dialog_count:dialogs.length,dialog_visible:visible(dialog),shell_inert:!!shell&&(shell.matches('[inert]')||shell.getAttribute('aria-hidden')==='true'),
      raw:dialog?.querySelector('.review-grid > pre')?.textContent??null,draft:field('review-draft'),prompt:field('prompt'),
      buttons:Array.from(dialog?.querySelectorAll('button[data-action]')??[]).map(n=>({action:n.dataset.action,visible:visible(n),enabled:enabled(n)})),
      main_text:document.querySelector('main.main-panel')?.textContent??document.querySelector('.conversation')?.textContent??document.querySelector('main')?.textContent??'',
      stop_visible:Array.from(document.querySelectorAll('section.composer button[data-action="cancel-run"]')).some(visible),
      errors:Array.from(document.querySelectorAll('.fatal,.ui-error-notice')).filter(visible).length};
  })()`);
}

export function createPromptReviewSubmitScenario({ choice, enhanceEntries = "composer", rawInteractionOnly = false } = {}) {
  if (!["raw", "enhanced"].includes(choice)) throw new TypeError("Prompt Review submit choice must be raw or enhanced");
  if (!["composer", "menu-palette"].includes(enhanceEntries)) throw new TypeError("Unknown Enhance entry plan");
  if(typeof rawInteractionOnly!=="boolean" || (rawInteractionOnly&&(choice!=="raw"||enhanceEntries!=="composer"))) throw new TypeError("Raw interaction uses the single composer Enhance flow");
  const id = rawInteractionOnly ? "prompt-review.raw-interaction" : enhanceEntries === "menu-palette" ? `prompt-review.entries-${choice}` : `prompt-review.submit-${choice}`;
  const owner = `scenario:${id}`;
  const resource = { provider:null, acceptedLedger:null, quiesceOutcome:null, inputCleanupFailure:null };
  const dispatch = choice === "raw" ? REVIEW_SUBMIT_RAW : REVIEW_SUBMIT_EDITED;
  return Object.freeze({
    id, productOracle:"pass", manualGate:rawInteractionOnly?"not_required":"pending", databaseRequired:true, requestGracefulExit,
    async prepare({ context, sink, phase }) {
      const turns=[
        {prompt:REVIEW_SUBMIT_RAW,responseText:REVIEW_SUBMIT_PROPOSAL},
        {prompt:REVIEW_SUBMIT_RAW,responseText:REVIEW_SUBMIT_PROPOSAL},
        {prompt:dispatch,responseText:REVIEW_SUBMIT_REPLY},
      ];
      resource.provider = await startScriptedProvider({ turns:rawInteractionOnly?turns.slice(0,1):turns });
      await prepareDesktopFixture({context,sink,phase,owner,
        configText:providerRestartFixtureConfig(resource.provider.baseUrl),
        sentinelName:"E2E_PROMPT_REVIEW_SUBMIT.txt",sentinelText:"Review submit GUI fixture only.\n"});
      await sink.record("scripted-provider-started",resource.provider.resourceObservation(),{phase,owner});
    },
    async execute({context,driver:cdp,sink}) {
      const provider=resource.provider;
      if(!provider)throw new Error("Review submit provider not prepared");
      await acquireInteractiveShell({context,driver:cdp,sink},{evidenceOwner:owner,screenshotStem:`${id}-shell`});
      const initial=await observe(cdp);
      const expected={workspacePath:context.paths.workspace,draftTarget:initial.projection.draft_target,runTarget:initial.projection.run_target,composerCommitGeneration:initial.projection.composer_commit_generation};
      if(!samePreRunOwner(initial,expected)||provider.requestLedger.length!==0) throw fail("initial","Expected clean idle GUI fixture",{initial,ledger:provider.requestLedger});
      const input=new WebviewInput(cdp,{probeId:id});
      const commands=new DesktopCommandProbe(cdp,{probeId:id,commands:["enhance_prompt","cancel_prompt_review","send_prompt_review","send_prompt"]});
      let primary=null;
      const record=(name,data)=>sink.record(name,data,{phase:"executing",owner});
      const wait=async(label,accept)=>{
        try {
          const result=await waitForObservation({label,timeoutMs:20_000,pollMs:80,retrySampleErrors:false,sample:async()=>({surface:await observe(cdp),ledger:provider.requestLedger}),accept});
          await record("review-submit-observation",{label,...result.value});return result.value;
        }catch(error){if(error?.code==="observation-timeout"&&!error.evidence?.last_error)throw fail("state",label,error.evidence);throw error;}
      };
      const settle=async target=>{
        const result=await waitForSemanticTargetSettlement({input,locator:target,label:`Review exact target ${target.selector}`,timeoutMs:5_000});
        if(result.value.classified.decision!=="pass")throw new DesktopE2eError("harness","review-submit-target","Review target is not exact",result.value);
      };
      const click=async target=>{await settle(target);await record("review-submit-trusted-click",{target,proof:await trustedClick(input,target)});};
      const insert=async(target,text)=>{await settle(target);await record("review-submit-trusted-insert",{target,proof:await trustedInsert(input,target,text)});};
      const key=async(target,keyName)=>{
        await settle(target);const snapshot=await input.snapshotProbe();
        if(!Object.entries(target.identity).every(([k,v])=>snapshot.active?.[k]===v))throw fail("focus","Review keyboard target lost focus",{target,snapshot});
        await input.pressKey(keyName);
        const proof=assertTrustedProbeSequence(await input.snapshotProbe(snapshot.sequence),{afterSequence:snapshot.sequence,expected:[
          {type:"keydown",identity:target.identity,key:keyName,code:keyName},
          ...(keyName==="Backspace"?[{type:"input",identity:target.identity,inputType:"deleteContentBackward"}]:[]),
          {type:"keyup",identity:target.identity,key:keyName,code:keyName},
        ]});await record("review-submit-trusted-key",{key:keyName,proof});
      };
      const ctrl=async(target,letter)=>{
        await settle(target);const snapshot=await input.snapshotProbe();
        if(!Object.entries(target.identity).every(([k,v])=>snapshot.active?.[k]===v))throw fail("focus","Review clipboard target lost focus",{target,snapshot});
        await input.keyDown("Control");try{await input.pressKey(letter);}finally{await input.keyUp("Control");}
        const proof=assertTrustedProbeSequence(await input.snapshotProbe(snapshot.sequence),{afterSequence:snapshot.sequence,expected:[
          {type:"keydown",identity:target.identity,key:"Control",code:"ControlLeft"},
          {type:"keydown",identity:target.identity,key:letter,code:`Key${letter.toUpperCase()}`},
          ...(letter==="v"?[{type:"input",identity:target.identity,inputType:"insertFromPaste"}]:[]),
          {type:"keyup",identity:target.identity,key:letter,code:`Key${letter.toUpperCase()}`},
          {type:"keyup",identity:target.identity,key:"Control",code:"ControlLeft"},
        ]});await record("review-submit-trusted-control-key",{letter,proof});
      };
      const exactCommands=async(start,wanted)=>record("review-submit-command-proof",assertExactDesktopCommandSequence(await commands.snapshot(start),{afterSequence:start,expected:wanted}));
      try{
        await input.installProbe();await commands.install();
        await insert(PROMPT,REVIEW_SUBMIT_RAW);
        await wait("Review raw composer ready",({surface,ledger})=>samePreRunOwner(surface,expected)&&surface.prompt.value===REVIEW_SUBMIT_RAW&&ledger.length===0);
        for(let index=0;index<2;index++){
          const before=await observe(cdp);const start=(await commands.snapshot()).sequence;
          if(enhanceEntries==="composer") await click(action("enhance-prompt","section.composer "));
          else if(index===0){
            await click(action("show-edit-menu",".app-titlebar "));
            await wait("Edit menu exposes Enhance",({surface})=>surface.projection.overlay==="edit_menu");
            await click(action("enhance-prompt",'[data-titlebar-menu="edit"] '));
          }else{
            await input.keyDown("Control");try{await input.pressKey("k");}finally{await input.keyUp("Control");}
            await wait("Palette opens for Enhance",({surface})=>surface.projection.overlay==="command_palette");
            const search={selector:'#local-search',identity:{tag:'INPUT',id:'local-search'}};
            await click(search);await ctrl(search,"a");await insert(search,"enhance-prompt");
            await wait("Palette Enhance query is accepted",({surface})=>surface.projection.local_search_text==="enhance-prompt");
            await click({selector:'[aria-labelledby="command-palette-dialog-title"] button[data-focus-key="palette-action:enhance-prompt"]',identity:{tag:'BUTTON',action:'enhance-prompt',focusKey:'palette-action:enhance-prompt'}});
          }
          await wait(`Review opens ${index+1}`,({surface,ledger})=>reviewSubmitOpenedFailures(surface,expected,REVIEW_SUBMIT_PROPOSAL).length===0&&reviewSubmitLedgerValid(ledger,Array(index+1).fill(REVIEW_SUBMIT_RAW)));
          await exactCommands(start,[{command:"enhance_prompt",args:{text:REVIEW_SUBMIT_RAW,expectedTarget:before.projection.draft_target,expectedRunTarget:before.projection.run_target}}]);
          if(rawInteractionOnly){
            const opened=await observe(cdp),noMutationStart=(await commands.snapshot()).sequence;
            await exerciseRawReviewInteraction({cdp,input,text:REVIEW_SUBMIT_RAW,record,
              assertUnchanged:label=>wait(label,({surface,ledger})=>reviewSubmitOpenedFailures(surface,expected,REVIEW_SUBMIT_PROPOSAL).length===0
                &&isDeepStrictEqual(surface.projection.review_target,opened.projection.review_target)&&reviewSubmitLedgerValid(ledger,[REVIEW_SUBMIT_RAW])),
              screenshot:name=>captureScenarioScreenshot({cdp,sink,name:`${id}-${name}`,owner}),
            });
            await exactCommands(noMutationStart,[]);
            const cancelStart=(await commands.snapshot()).sequence;
            await click(action("cancel-review"));
            const cancelled=await wait("Raw interaction explicit Cancel restores Main without dispatch",({surface,ledger})=>reviewSubmitCancelledFailures(surface,expected).length===0&&reviewSubmitLedgerValid(ledger,[REVIEW_SUBMIT_RAW]));
            await exactCommands(cancelStart,[{command:"cancel_prompt_review",args:{expectedTarget:opened.projection.review_target}}]);
            resource.acceptedLedger=structuredClone(cancelled.ledger);
            await captureScenarioScreenshot({cdp,sink,name:`${id}-cancelled`,owner});
            await record("review-raw-interaction-scope",{provider_requests:1,sent_tasks:0,selection:"actual trusted mouse drag; Range is read-only geometry",
              copy:"trusted Ctrl+C and native copy event/default permission observed; OS clipboard bytes are not asserted",
              acceptance:"automated interaction contract only; no manual visual, physical mouse/IME or release acceptance claim"});
            return {acquisition:"pass",oracle:"pass",manual:"not_required"};
          }
          if(index===0){
            const opened=await observe(cdp);const cancelStart=(await commands.snapshot()).sequence;
            await click(action("cancel-review"));
            await wait("Review explicit cancel restores Main",({surface,ledger})=>reviewSubmitCancelledFailures(surface,expected).length===0&&reviewSubmitLedgerValid(ledger,[REVIEW_SUBMIT_RAW]));
            await exactCommands(cancelStart,[{command:"cancel_prompt_review",args:{expectedTarget:opened.projection.review_target}}]);
            await captureScenarioScreenshot({cdp,sink,name:`${id}-cancelled`,owner});
          }
        }
        const noMutationStart=(await commands.snapshot()).sequence;
        await click(DRAFT);await ctrl(DRAFT,"a");await key(DRAFT,"Backspace");
        await wait("Review empty enhanced send is disabled",({surface,ledger})=>reviewSubmitOpenedFailures(surface,expected,"").length===0&&reviewSubmitLedgerValid(ledger,[REVIEW_SUBMIT_RAW,REVIEW_SUBMIT_RAW]));
        await captureScenarioScreenshot({cdp,sink,name:`${id}-empty-enhanced-disabled`,owner});
        await insert(DRAFT,REVIEW_SUBMIT_EDITED);
        await wait("Review local edited draft",({surface})=>reviewSubmitOpenedFailures(surface,expected,REVIEW_SUBMIT_EDITED).length===0);
        // Use the real system clipboard through trusted Ctrl+C/Ctrl+V. No DOM value injection or browser clipboard permission override.
        await ctrl(DRAFT,"a");await ctrl(DRAFT,"c");await key(DRAFT,"Backspace");
        await wait("Review copy roundtrip cleared",({surface})=>reviewSubmitOpenedFailures(surface,expected,"").length===0);
        await ctrl(DRAFT,"v");
        await wait("Review native copy paste restores exact edit",({surface,ledger})=>reviewSubmitOpenedFailures(surface,expected,REVIEW_SUBMIT_EDITED).length===0&&reviewSubmitLedgerValid(ledger,[REVIEW_SUBMIT_RAW,REVIEW_SUBMIT_RAW]));
        await exactCommands(noMutationStart,[]);
        await captureScenarioScreenshot({cdp,sink,name:`${id}-edited-copy-roundtrip`,owner});
        const submitting=await observe(cdp);const submitStart=(await commands.snapshot()).sequence;
        await click(action(`send-review-${choice}`));
        const completed=await wait(`Review ${choice} dispatch completes`,({surface,ledger})=>reviewSubmitCompletedFailures(surface,ledger,{choice,workspacePath:context.paths.workspace}).length===0);
        await exactCommands(submitStart,[{command:"send_prompt_review",args:{enhanced:choice==="enhanced",text:REVIEW_SUBMIT_EDITED,expectedTarget:submitting.projection.review_target,expectedRunTarget:submitting.projection.run_target}}]);
        resource.acceptedLedger=structuredClone(completed.ledger);
        await captureScenarioScreenshot({cdp,sink,name:`${id}-completed`,owner});
        await record("review-submit-scope",{choice,enhanceEntries,dispatch,edit:REVIEW_SUBMIT_EDITED,
          input:"actual Tauri browser-trusted pointer/key/insert; native clipboard copy/paste",
          provider:"existing scripted ordinary independent turns; no live LLM or real peer",
          limitations:["No raw pre native text-selection test", "No pending Enhance cancellation race", "Manual visual review remains required", "OS clipboard now contains the isolated review marker"]});
        return {acquisition:"pass",oracle:"pass",manual:"pending"};
      }catch(error){primary=error;throw error;}
      finally{
        let cleanupError=null;
        for(const [name,cleanup] of [["commands",()=>commands.remove()],["input",()=>input.cleanup()]]){
          try{await cleanup();}catch(error){resource.inputCleanupFailure={name,message:error.message};cleanupError??=error;}
        }
        if(!primary&&cleanupError)throw cleanupError;
      }
    },
    async quiesce({inputs}){
      if(resource.quiesceOutcome!==null)return structuredClone(resource.quiesceOutcome);
      resource.quiesceOutcome=await quiesceProviderResource({provider:resource.provider,acceptedLedger:resource.acceptedLedger,inputs});
      return structuredClone(resource.quiesceOutcome);
    },
    async cleanup(){return {input:resource.quiesceOutcome?.input==="pass"&&resource.inputCleanupFailure===null?"pass":"fail",resources:[{kind:"prompt-review-submit-verification",choice,quiesce:resource.quiesceOutcome,input_cleanup_failure:resource.inputCleanupFailure}]};},
  });
}

export function createPromptReviewRawInteractionScenario(){
  return createPromptReviewSubmitScenario({choice:"raw",rawInteractionOnly:true});
}
