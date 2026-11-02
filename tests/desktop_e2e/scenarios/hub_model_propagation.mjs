import { isDeepStrictEqual } from "node:util";
import path from "node:path";
import { readFile, readdir } from "node:fs/promises";
import { createHash } from "node:crypto";
import { assertNoLocalRunner, preparePreviousPublication } from "../drivers/previous_publication_fixture.mjs";
import { DesktopE2eError } from "../core/execution.mjs";
import { normalizeHubBrowserOptions, startHubBrowserResource } from "../drivers/hub_browser_resource.mjs";
import { startSharedWorkflowProvider } from "../drivers/shared_work_runner_fixture.mjs";
import { WebviewInput } from "../drivers/webview_input.mjs";
import { DesktopCommandProbe } from "../drivers/desktop_command_probe.mjs";
import { acquireInteractiveShell } from "./shell_baseline.mjs";
import { prepareDesktopFixture } from "./fixture.mjs";
import { permissionRestartGuardianChatFixtureConfig } from "./permission_restart_guardian.mjs";
import { captureScenarioScreenshot, invokeDesktopCommand } from "./observations.mjs";
import { action, byId, chooseModel, trustedClick, wait, enrollDesktopFromHubBrowser, requestHubEnrollmentExit } from "./hub_browser_enrollment.mjs";

const ID = "hub.model-propagation", OWNER = `scenario:${ID}`;
const contexts = ["main", "side_chat", "approve"];
const fail = (message, evidence = {}) => new DesktopE2eError("product", "hub-model-propagation-mismatch", message, evidence);

export function catalogPropagationAccepted(before, after, revision) {
  return after?.status === "connected" && after.catalog?.revision === revision && after.hub_id === before.hub_id
    && after.settings_revision === before.settings_revision
    && contexts.every(context => after[`${context}_confirmation`] === "review_required"
      && isDeepStrictEqual(after[`${context}_review`], before[`${context}_review`])
      && after[`${context}_uses_default`] === before[`${context}_uses_default`]);
}

export function createHubModelPropagationScenario(options = {}) {
  const {liveProvider,upstreamCredentialFile,previousPublication = false,...hubOptions}=options;
  if (typeof previousPublication !== "boolean") throw new TypeError("previousPublication must be boolean");
  const settings = normalizeHubBrowserOptions(hubOptions);
  if(liveProvider && (!/^https?:$/.test(new URL(liveProvider.provider_base_url).protocol) || typeof liveProvider.model!=="string" || !liveProvider.model.trim())) throw new TypeError("Live generation requires an explicit HTTP(S) endpoint and model");
  if(upstreamCredentialFile && (!liveProvider || !path.isAbsolute(upstreamCredentialFile))) throw new TypeError("A credential file requires an explicit live endpoint and absolute path");
  const state = { resource: null, providers: [], input: null, commands: null, nativeOwner: null,
    nativeCandidate: null, nativeBefore: null, importDispatched: false, failures: [], close: null, held: null, receipt: null, requests: [], environment:{}, capture:null, credential:null };
  async function settleInput() {
    if (state.commands) { try { await state.commands.remove(); } catch { state.failures.push("command-cleanup"); } state.commands = null; }
    if (state.input) { try { await state.input.cleanup(); } catch { state.failures.push("input-cleanup"); } state.input = null; }
  }
  return Object.freeze({ id: ID, productOracle: "pass", manualGate: "pending", databaseRequired: true,
    get environment(){return state.environment;},
    async prepare(args) {
      if(liveProvider){
        state.capture=path.join(args.context.root,"http-request-capture");
        state.environment={MOYAI_HTTP_REQUEST_CAPTURE_DIR:state.capture};
        if(upstreamCredentialFile){
          const credential=JSON.parse(await readFile(upstreamCredentialFile,"utf8"));
          const canonical=value=>new URL(value).href.replace(/\/v1\/?$/,"").replace(/\/$/,"");
          if(canonical(credential.endpoint)!==canonical(liveProvider.provider_base_url) || credential.provider_profile!=="openai_compatible_chat" || typeof credential.key!=="string" || !credential.key) throw new Error("Credential does not match the live endpoint/profile");
          state.credential=credential.key;
        }
      }
      await prepareDesktopFixture({ ...args, owner:OWNER, configText:permissionRestartGuardianChatFixtureConfig("http://127.0.0.1:9").replace("request_timeout_ms = 30000",liveProvider ? "request_timeout_ms = 120000" : "request_timeout_ms = 30000"),
        sentinelName:"E2E_HUB_MODEL.txt", sentinelText:"Hub model integration fixture.\n" });
      if (previousPublication) {
        state.previousPublication = await preparePreviousPublication(args.context);
        await args.sink.record("previous-publication-fixture", { ...state.previousPublication,
          scope: "Saved previous Hub/device publication with a settled empty journal; no current execution consent or enrollment is seeded." }, { phase: args.phase, owner: OWNER });
      }
      state.receipt=path.join(args.context.paths.workspace,"hub-approval-effect.txt");
      for (const name of ["A", "B"]) state.providers.push(await startSharedWorkflowProvider({
        reply: async request => {
          const latest=JSON.stringify(request.messages.filter(message=>message.role==="user").at(-1));
          const role=request.messages.some(message=>message.role==="system" && String(message.content).includes("independent permission guardian"))
            ? "approve" : latest.includes("hub-model-side-") ? "side_chat" : "main";
          state.requests.push({role,upstream:name,request});
          if (state.held?.role===role && !state.held.seen) { state.held.seen=true; await state.held.promise; }
          if(role==="approve") return {delta:{role:"assistant",content:JSON.stringify({risk_level:"low",rationale:"Write the authorized receipt only inside this isolated test workspace."})},finish:"stop"};
          if(role==="main" && latest.includes("hub-model-approval-")) {
            const id="model-approval-"+latest.match(/hub-model-approval-([a-z0-9-]+)/)?.[1];
            if(!request.messages.some(message=>message.role==="tool" && message.tool_call_id===id)) return {
              delta:{role:"assistant",tool_calls:[{index:0,id,type:"function",function:{name:"shell",arguments:JSON.stringify({
                command:`[System.IO.File]::WriteAllText('${state.receipt.replaceAll("'","''")}', 'HUB_APPROVED_EFFECT')`,
                sandbox_permissions:"require_escalated",justification:"Write the explicitly requested integration receipt in the isolated workspace.",
              })}}]},finish:"tool_calls"};
          }
          return { delta: { role: "assistant", content: `HUB_MODEL_${name}_OK` }, finish: "stop" };
        },
      }));
      state.resource = await startHubBrowserResource({ ...args, options: settings });
    },
    async execute({ context, runtime, driver: cdp, sink }) {
      const { resource, providers } = state, { page, hub } = resource;
      await acquireInteractiveShell({ context, driver: cdp, sink }, { evidenceOwner: OWNER, screenshotStem: "model-propagation-shell" });
      const input = state.input = new WebviewInput(cdp, { probeId: ID });
      await input.installProbe();
      state.commands = new DesktopCommandProbe(cdp, { probeId: ID, commands: ["hub_refresh", "hub_save_review"] });
      await state.commands.install();
      const projection = () => invokeDesktopCommand(cdp, "hub_projection");
      const form = () => cdp.evaluate(`Object.fromEntries(['main','side_chat','approve'].map(context => {
        const node=document.querySelector('[data-ai-connection="'+context+'"]');
        return [context,{choice:node?.querySelector('select')?.value,options:[...(node?.querySelector('select')?.options??[])].map(o=>({value:o.value,text:o.textContent})),
          currentRevision:node?.querySelector('[data-settings-passive$="-comparison"]')?.textContent?.match(/現在の更新\\s+(\\d+)/)?.[1],
          feedback:node?.querySelector('[id$="-feedback"]')?.textContent,saveDisabled:node?.querySelector('[data-action^="hub-save-"]')?.disabled}];
      }))`);
      async function navigateRole(name) {
        const href = `#settings-${name === "main" ? "provider" : name === "side_chat" ? "side-chat" : "approve"}`;
        await trustedClick(input, cdp, { selector: `.settings-nav a[href="${href}"]`, identity: { tag: "A", href } }, sink);
      }
      async function defaults(modelId) {
        await page.locator('nav a[href="#models"]').click();
        for (const id of ["team-standard-model", "team-standard-side-model", "team-standard-approve-model"])
          await page.locator(`#${id}`).selectOption(modelId);
        await page.locator("#team-standard-save").click();
        const saved=await wait("Hub saves all three standard models", () => hub.command("hub_snapshot"), value => ["team_default_model_id", "team_default_side_model_id", "team_default_approve_model_id"].every(key => value.store.catalog[key] === modelId));
        await wait("Hub finishes reflecting the saved revision before another edit",()=>page.evaluate(()=>({
          revision:document.querySelector("#metric-revision")?.textContent,
          busy:document.querySelector("#team-standard-model")?.disabled,
          status:document.querySelector("#team-standard-status")?.textContent,
        })),value=>value.revision===String(saved.store.revision) && value.busy===false && !value.status.includes("未保存"));
        return saved;
      }
      async function propagated(name, before, snapshot, afterSequence, extra = () => true) {
        const started = Date.now();
        const after = await wait(`${name}: connected Desktop receives the new catalog without a refresh click`, projection,
          value => catalogPropagationAccepted(before, value, snapshot.store.revision) && extra(value), 45_000);
        const visible=await wait(`${name}: all three settings forms display the new catalog revision`, form,
          value=>contexts.every(context=>value[context].currentRevision===String(snapshot.store.revision)));
        const commands = await state.commands.snapshot(afterSequence);
        if (commands.dropped_through > afterSequence || commands.calls.length) throw fail("Passive catalog propagation issued an explicit review or refresh command", commands);
        await sink.record("hub-model-propagated", { name, elapsed_ms: Date.now()-started, before, after, form: visible, explicit_commands: commands.calls }, { phase: "executing", owner: OWNER });
        return after;
      }
      async function saveRole(name) {
        await navigateRole(name);
        await trustedClick(input, cdp, action(`hub-save-${name === "side_chat" ? "side" : name}`, `[data-ai-connection="${name}"]`), sink);
        return wait(`${name} explicitly confirms the current catalog`, projection, value => value[`${name}_confirmation`] === "confirmed");
      }
      async function confirmAll() {
        await navigateRole("main");
        await trustedClick(input, cdp, action("hub-refresh", '[data-ai-connection="main"]'), sink);
        for (const name of contexts) await saveRole(name);
      }
      async function closeSettings() {
        await trustedClick(input, cdp, action("close-overlay", ".settings-modal"), sink);
        await wait("Settings close and the conversation input becomes actionable",()=>cdp.evaluate(`(() => {
          const prompt=document.querySelector("main.conversation textarea#prompt");
          return !document.querySelector(".settings-modal") && Boolean(prompt && !prompt.disabled && prompt.getClientRects().length && getComputedStyle(prompt).visibility==="visible");
        })()`),Boolean);
      }
      async function sendMain(name, expectedProvider, expectedSystemPrompt = null) {
        await closeSettings();
        const counts = providers.map(provider => provider.requests.length);
        const prompt = `hub-model-propagation-${name}: reply with the configured fixture response`;
        await trustedClick(input, cdp, byId("prompt", "TEXTAREA"), sink);
        await input.keyDown("Control");
        try { await input.pressKey("a"); } finally { await input.keyUp("Control"); }
        await input.insertText(byId("prompt", "TEXTAREA"), prompt);
        await trustedClick(input, cdp, action("send", "section.composer"), sink);
        const response = `HUB_MODEL_${expectedProvider === 0 ? "A" : "B"}_OK`;
        const result = await wait("Next Main request reaches the selected Hub upstream and completes", async () => ({
          desktop: await invokeDesktopCommand(cdp, "desktop_state"),
          text: await cdp.evaluate("document.querySelector('.transcript')?.textContent ?? document.body.textContent"),
          counts: providers.map(provider => provider.requests.length),
        }), value => value.desktop.run_status_key === "completed" && value.desktop.can_submit === true
          && value.counts[expectedProvider] > counts[expectedProvider] && value.text.includes(response), 60_000);
        if (result.counts[1-expectedProvider] !== counts[1-expectedProvider] || providers.some(provider => provider.failures.length))
          throw fail("Main request reached the wrong upstream or fixture failed", { counts, result: result.counts, failures: providers.map(p => p.failures) });
        const requests = providers[expectedProvider].requests.slice(counts[expectedProvider]);
        if (expectedSystemPrompt && !requests.every(request => request.messages.some(message =>
          message.role === "system" && JSON.stringify(message.content).includes(expectedSystemPrompt))))
          throw fail("The next Main request did not receive the updated Hub model instructions", { name, expectedSystemPrompt });
        await wait("Main completion is visible before recording the screenshot", () => cdp.evaluate(
          'document.querySelector("main.conversation > header.topbar .status-line")?.innerText'),
          value => value?.includes(result.desktop.status_message));
        await sink.record("hub-model-next-main-request", { name, upstream: expectedProvider === 0 ? "A" : "B", counts_before: counts, counts_after: result.counts, response, expected_system_prompt_received: expectedSystemPrompt ? true : null }, { phase: "executing", owner: OWNER });
        await captureScenarioScreenshot({ cdp, sink, name: `model-main-${name}`, owner: OWNER });
        await trustedClick(input, cdp, action("show-config", "aside.sidebar"), sink);
      }
      async function beginRole(role, name) {
        await closeSettings();
        const start=state.requests.length;
        if(role==="side_chat") {
          if(!await cdp.evaluate('Boolean(document.querySelector("aside.side-chat-pane textarea#side-chat-prompt"))'))
            await trustedClick(input,cdp,action("show-side-chat-pane","body"),sink);
          await wait("Sub is ready for input",()=>invokeDesktopCommand(cdp,"desktop_state"),value=>value.side_chat.can_send);
        }
        const target=byId(role==="side_chat" ? "side-chat-prompt" : "prompt","TEXTAREA");
        await trustedClick(input,cdp,target,sink);
        await input.insertText(target,`hub-model-${role==="side_chat" ? "side" : role==="approve" ? "approval" : "main"}-${name}: run the isolated integration fixture`);
        await trustedClick(input,cdp,action(role==="side_chat" ? "send-side-chat" : "send",role==="side_chat" ? "aside.side-chat-pane" : "section.composer"),sink);
        return start;
      }
      async function finishRole(role,start,upstream,prompt,expectedStatus="completed") {
        const result=await wait(`${role} generation finishes through the captured Hub route`,()=>invokeDesktopCommand(cdp,"desktop_state"),value=>
          (role==="side_chat" ? value.side_chat.status==="completed" && !value.side_chat.can_cancel && !value.side_chat.last_error : value.run_status_key===expectedStatus)
          && state.requests.slice(start).some(row=>row.role===role),60_000);
        const rows=state.requests.slice(start).filter(row=>row.role===role);
        if(!rows.length || rows.some(row=>row.upstream!==upstream || (prompt && !row.request.messages.some(message=>message.role==="system" && String(message.content).includes(prompt)))))
          throw fail("Role generation used the wrong upstream or model instructions",{role,upstream,prompt,observed:rows.map(row=>({role:row.role,upstream:row.upstream}))});
        if(role==="approve" && await readFile(state.receipt,"utf8")!=="HUB_APPROVED_EFFECT") throw fail("Approved tool did not perform its bounded effect");
        if(expectedStatus==="failed" && (!result.status_message.includes("catalog_changed") || !result.status_message.includes("モデル選択を確認・保存")
          || !isDeepStrictEqual(state.requests.slice(start).map(row=>row.role),["main","approve"])))
          throw fail("The next request must stop with review instructions, without replaying work",{status:result.status_message,roles:state.requests.slice(start).map(row=>row.role)});
        if(role==="side_chat") {
          const answer=result.side_chat.messages.findLast(message=>message.role==="assistant");
          if(!answer?.content) throw fail("Sub completed without an assistant answer");
          await wait("Sub completion and its exact answer are visible before recording the screenshot",()=>cdp.evaluate(`({
            completed:Boolean(document.querySelector("aside.side-chat-pane .side-chat-status-completed")),
            messages:[...document.querySelectorAll("aside.side-chat-pane article.side-chat-message-assistant")].map(node=>({id:node.dataset.sideChatMessageId,text:node.innerText})),
          })`),value=>value.completed && value.messages.some(message=>message.id===answer.id && message.text.includes(answer.content)));
        } else await wait("Terminal result is visible before recording the screenshot",()=>cdp.evaluate('document.querySelector("main > header.topbar .status-line")?.innerText ?? document.querySelector(".topbar .status-line")?.innerText'),
          value=>value?.includes(result.status_message));
        await captureScenarioScreenshot({cdp,sink,name:`model-${role}-${start}-complete`,owner:OWNER});
        await trustedClick(input,cdp,action("show-config","aside.sidebar"),sink);
        return {role,upstream,requests:rows.length,status:role==="side_chat" ? result.side_chat.status : result.run_status_key,prompt_received:Boolean(prompt)};
      }
      try {
        await page.locator('nav a[href="#models"]').click();
        for (let i=0; i<providers.length; i++) {
          await page.locator("#endpoint").fill(providers[i].baseUrl.replace(/\/v1$/, ""));
          await page.locator("#profile").selectOption("openai_compatible_chat");
          await page.locator("#discover").click();
          await page.locator('#model option[value="shared-workflow"]').waitFor({ state: "attached" });
          await page.locator("#model").selectOption("shared-workflow");
          await page.locator("#label").fill(`Propagation ${i===0 ? "A" : "B"}`);
          await page.locator("#allow-tools").check();
          if(!await page.locator(".registration-options").evaluate(node=>node.open)) await page.locator(".registration-options > summary").click();
          await page.locator("#capacity").fill("3");
          await page.locator("#register").click();
          await page.locator("#model-rows tr").filter({ hasText: `Propagation ${i===0 ? "A" : "B"}` }).waitFor();
        }
        const models = (await hub.command("hub_snapshot")).store.catalog.models;
        const a=models.find(row=>row.label==="Propagation A")?.id, b=models.find(row=>row.label==="Propagation B")?.id;
        if (!a || !b || a===b) throw fail("Two independent upstreams must have distinct logical models", { models });
        await defaults(a);
        await page.locator('nav a[href="#device-network"]').click();
        await page.locator("#network-ip").fill("127.0.0.1");
        await page.locator("#network-port").fill(String(hub.networkPort));
        await page.locator("#network-start").click();
        await page.locator("#network-stop").waitFor();
        const enrolled = await enrollDesktopFromHubBrowser({ resource, context, runtime, cdp, input, sink, nativeState: state });
        const connectionBefore = await readFile(path.join(context.paths.config, "device-network", "device.json"));
        const executionBefore = await invokeDesktopCommand(cdp, "device_execution_projection");
        if (executionBefore.directory || executionBefore.can_pause || executionBefore.can_resume) throw fail("Hub authentication alone configured shared execution", executionBefore);
        if (context.desktopIsolation === "fixture") await assertNoLocalRunner(context);
        await trustedClick(input, cdp, action("show-config", '[data-surface="hub"]'), sink);
        await wait("All three initial standard selections point to upstream A", projection, value => contexts.every(name => value[`${name}_uses_default`] && value[`${name}_confirmation`]==="confirmed" && value[`${name}_review`]?.selection.preferred_model_id===a));
        await sendMain("initial", 0);
        if (context.desktopIsolation === "fixture") {
          await assertNoLocalRunner(context);
          await sink.record("hub-local-without-runner", { authenticated: true, shared_execution_configured: false,
            runner_absent_before_and_after_generation: true, main_completed: true }, { phase: "executing", owner: OWNER });
        }
        if (state.previousPublication) {
          const { registryFile, journal, journalHash } = state.previousPublication;
          const entries = JSON.parse(await readFile(registryFile, "utf8"));
          const unchangedConnection = connectionBefore.equals(await readFile(path.join(context.paths.config, "device-network", "device.json")));
          if (entries.length || !unchangedConnection || createHash("sha256").update(await readFile(journal)).digest("hex") !== journalHash)
            throw fail("Local generation did not retire only the previous publication while preserving enrollment and journal", { entries, unchangedConnection });
          await sink.record("local-hub-after-reenrollment", { device_id: enrolled.network.device_id, publication_removed: true,
            hub_credentials_preserved: true, previous_journal_preserved: true, shared_execution_configured: false,
            scope: "Actual Hub authentication and Main generation; previous settled identity is retired without resetting the Hub connection." }, { phase: "executing", owner: OWNER });
          await captureScenarioScreenshot({ cdp, sink, name: "local-hub-after-previous-publication", owner: OWNER });
        }

        let before=await projection(), afterSequence=(await state.commands.snapshot()).sequence;
        await propagated("standard-A-to-B", before, await defaults(b), afterSequence,
          value => contexts.every(name=>value[`recommended_${name}_selection`]?.preferred_model_id===b));
        const unchanged=await form();
        if (!contexts.every(name=>unchanged[name].choice===":hub-default")) throw fail("Passive standard update changed the user's choice type", unchanged);
        await captureScenarioScreenshot({ cdp, sink, name: "model-standard-update-review-required", owner: OWNER });
        await confirmAll();
        await wait("Explicit save adopts B separately for all contexts", projection, value => contexts.every(name=>value[`${name}_uses_default`] && value[`${name}_review`]?.selection.preferred_model_id===b));
        await sendMain("after-standard-save", 1);

        // Model names propagate to every client without overwriting per-deployment labels.
        await chooseModel(input, cdp, "main", a, sink);
        await input.pressKey("Tab");
        before=await projection(); afterSequence=(await state.commands.snapshot()).sequence;
        await page.locator('nav a[href="#models"]').click();
        await page.locator("#model-rows tr").filter({ hasText: "Propagation B" }).getByRole("button", { name: /を編集/ }).click();
        await page.locator("#edit-label").fill("Propagation B renamed");
        await page.locator("#save-edit").click(); await page.locator("#edit-dialog").waitFor({ state: "hidden" });
        const renamed=await hub.command("hub_snapshot");
        const renamedDesktop=await propagated("model-name-change-with-main-draft", before, renamed, afterSequence,
          value=>value.catalog.models.find(model=>model.id===b)?.label==="Propagation B renamed");
        await wait("All Desktop model choices display the changed shared name",form,
          value=>contexts.every(name=>value[name].options.some(option=>option.value===b && option.text==="Propagation B renamed")));
        if(renamed.store.catalog.deployments.find(row=>row.logical_model_id===b)?.label!=="Propagation B")
          throw fail("Renaming a shared model overwrote its deployment label");
        await sink.record("hub-model-name-propagated", {
          hub_deployment_label:renamed.store.catalog.deployments.find(row=>row.logical_model_id===b)?.label,
          hub_logical_label:renamed.store.catalog.models.find(row=>row.id===b)?.label,
          desktop_logical_label:renamedDesktop.catalog.models.find(row=>row.id===b)?.label,
          form:await form(), scope:"Shared model name propagated; registration label preserved",
        }, { phase:"executing", owner:OWNER });

        before=renamedDesktop; afterSequence=(await state.commands.snapshot()).sequence;
        const updatedPrompt="The Hub model instruction was changed while the Desktop choice was unsaved.";
        await page.locator("#model-rows tr").filter({ hasText: "Propagation A" }).getByRole("button", { name: /を編集/ }).click();
        await page.locator("#edit-system-prompt").fill(updatedPrompt);
        await page.locator("#save-edit").click(); await page.locator("#edit-dialog").waitFor({ state: "hidden" });
        await propagated("model-instruction-change-with-main-draft", before, await hub.command("hub_snapshot"), afterSequence,
          value=>value.catalog.models.some(model=>model.id===a && model.system_prompt===updatedPrompt));
        const draft=await wait("Main input survives the changed Hub instructions", form, value => value.main.choice===a);
        if (!draft.main.saveDisabled) throw fail("Stale draft may not save before reviewing the changed Hub", draft);
        await captureScenarioScreenshot({ cdp, sink, name: "model-instruction-update-draft-preserved", owner: OWNER });
        await confirmAll();
        const explicit=await projection();
        if (explicit.main_uses_default || explicit.main_review.selection.preferred_model_id!==a) throw fail("Review refresh discarded the explicit Main draft", explicit);
        await sendMain("after-instruction-save", 0, updatedPrompt);

        before=explicit; afterSequence=(await state.commands.snapshot()).sequence;
        await propagated("standard-change-preserves-explicit-main", before, await defaults(a), afterSequence);
        await confirmAll();
        const retained=await projection();
        if (retained.main_uses_default || retained.main_review.selection.preferred_model_id!==a) throw fail("Hub default overwrote an existing individual selection", retained);

        await chooseModel(input,cdp,"main",":hub-default",sink);
        await saveRole("main");
        const prompts=new Map([[a,updatedPrompt],[b,""]]);
        let current=a;
        for(const role of contexts) {
          const next=current===a ? b : a, upstream=current===a ? "A" : "B", nextUpstream=next===a ? "A" : "B";
          before=await projection();
          let release;
          state.held={role,seen:false,promise:new Promise(resolve=>{release=resolve;}),release:()=>release()};
          const start=await beginRole(role,`during-${role.replaceAll("_","-")}`);
          await wait(`${role} request is in flight at its real upstream`,()=>state.held.seen,value=>value===true);
          const captured=await projection();
          afterSequence=(await state.commands.snapshot()).sequence;
          await defaults(next);
          const nextPrompt=`Updated ${role} instructions for the next request only.`;
          await page.locator(`#model-rows tr`).filter({hasText:next===a ? "Propagation A" : "Propagation B renamed"}).getByRole("button",{name:/を編集/}).click();
          await page.locator("#edit-system-prompt").fill(nextPrompt);
          await page.locator("#save-edit").click(); await page.locator("#edit-dialog").waitFor({state:"hidden"});
          const changed=await hub.command("hub_snapshot");
          const activeKey=role==="side_chat" ? "active_side_chat" : "active_main";
          const pending=await wait("Catalog changes invalidate future work without replacing the active turn",projection,value=>
            value.main_confirmation==="review_required" && value[activeKey]?.turn_id===captured[activeKey]?.turn_id
            && value.connection_generation===captured.connection_generation && value.catalog.revision===captured.catalog.revision,20_000);
          if(!captured[activeKey] || pending[activeKey].logical_model_id!==captured[activeKey].logical_model_id)
            throw fail("A live role changed its captured model",{role,captured,pending});
          await captureScenarioScreenshot({cdp,sink,name:`model-${role}-hub-changed-during-generation`,owner:OWNER});
          state.held.release();
          const previousResult=await finishRole(role,start,upstream,prompts.get(current),role==="approve" ? "failed" : "completed");
          state.held=null;
          await propagated(`${role}-finished-after-hub-change`,before,changed,afterSequence);
          await confirmAll();
          prompts.set(next,nextPrompt);
          const nextStart=await beginRole(role,`after-${role.replaceAll("_","-")}`);
          const nextResult=await finishRole(role,nextStart,nextUpstream,nextPrompt);
          await sink.record("hub-role-active-change",{role,captured:previousResult,next:nextResult,started_response_preserved:true,unreviewed_continuation_blocked:role==="approve",new_catalog_reviewed:true},{phase:"executing",owner:OWNER});
          current=next;
        }

        before=await projection(); afterSequence=(await state.commands.snapshot()).sequence;
        await page.locator('nav a[href="#models"]').click();
        await page.locator("#model-rows tr").filter({ hasText: current===a ? "Propagation A" : "Propagation B renamed" }).getByRole("button", { name: /を編集/ }).click();
        await page.locator("#edit-enabled").uncheck();
        await page.locator("#save-edit").click(); await page.locator("#edit-dialog").waitFor({ state: "hidden" });
        await propagated("selected-deployment-disabled", before, await hub.command("hub_snapshot"), afterSequence);
        await captureScenarioScreenshot({ cdp, sink, name: "model-disabled-review-required", owner: OWNER });
        const counts=providers.map(provider=>provider.requests.length);
        await closeSettings();
        await trustedClick(input, cdp, byId("prompt", "TEXTAREA"), sink);
        await input.insertText(byId("prompt", "TEXTAREA"), "hub-model-propagation-disabled: must not reach another model");
        const blocked=await wait("Unreviewed model change disables the next Main submission", async () => ({
          desktop:await invokeDesktopCommand(cdp,"desktop_state"),
          sendDisabled:await cdp.evaluate('document.querySelector("section.composer button[data-action=send]")?.disabled'),
        }), value=>value.desktop.can_submit===false && value.sendDisabled===true);
        if (!isDeepStrictEqual(counts, providers.map(provider=>provider.requests.length))) throw fail("Unreviewed disabled model was used or silently fell back");
        await trustedClick(input,cdp,action("show-config","aside.sidebar"),sink);
        before=await projection(); afterSequence=(await state.commands.snapshot()).sequence;
        await page.locator("#model-rows tr").filter({hasText:current===a ? "Propagation A" : "Propagation B renamed"}).getByRole("button",{name:/を編集/}).click();
        await page.locator("#remove-model").click();
        await page.locator("#remove-model").click();
        await page.locator("#edit-dialog").waitFor({state:"hidden"});
        await propagated("selected-model-deleted",before,await hub.command("hub_snapshot"),afterSequence,
          value=>!value.catalog.models.some(model=>model.id===current));
        if (!isDeepStrictEqual(counts, providers.map(provider=>provider.requests.length))) throw fail("Model deletion silently submitted work to another upstream");
        const remaining=current===a ? b : a;
        for(const role of contexts) await chooseModel(input,cdp,role,remaining,sink);
        await confirmAll();
        await sendMain("after-explicit-deletion-recovery",remaining===a ? 0 : 1,prompts.get(remaining));
        if(liveProvider){
          before=await projection(); afterSequence=(await state.commands.snapshot()).sequence;
          await page.locator('nav a[href="#models"]').click();
          await page.locator("#endpoint").fill(liveProvider.provider_base_url);
          await page.locator("#profile").selectOption("openai_compatible_chat");
          if(state.credential){
            try { await page.locator("#api-key").fill(state.credential); }
            catch { throw new Error("Could not enter the existing live-provider credential"); }
            finally { state.credential=null; }
          }
          await page.locator("#discover").click();
          await page.locator(`#model option[value=${JSON.stringify(liveProvider.model)}]`).waitFor({state:"attached"});
          await page.locator("#model").selectOption(liveProvider.model);
          if(!await page.locator(".registration-options").evaluate(node=>node.open)) await page.locator(".registration-options > summary").click();
          await page.locator("#logical-model").selectOption("");
          await page.locator("#label").fill("実モデル生成の確認");
          await page.locator("#model-system-prompt").fill("");
          await page.locator("#allow-tools").check();
          await page.locator("#register").click();
          await page.locator("#model-rows tr").filter({hasText:"実モデル生成の確認"}).waitFor();
          const live=(await hub.command("hub_snapshot")).store.catalog.models.find(model=>model.label==="実モデル生成の確認");
          if(!live) throw fail("Live model registration was not saved");
          await propagated("live-model-added",before,await hub.command("hub_snapshot"),afterSequence);
          for(const role of contexts) await chooseModel(input,cdp,role,live.id,sink);
          await confirmAll();
          const fixtureCounts=providers.map(provider=>provider.requests.length);
          const liveReceipt=path.join(context.paths.workspace,"live-approval-effect.txt");
          for(const role of contexts){
            const prior=new Set((await readdir(state.capture)).filter(name=>name.endsWith(".request.json")));
            const marker=role==="main" ? "LIVE_MAIN_OK 42" : role==="side_chat" ? "LIVE_SUB_OK 56" : "LIVE_APPROVE_OK";
            const assistant=value=>role==="side_chat"
              ? value.side_chat.messages.filter(message=>message.role==="assistant").at(-1)?.content
              : value.transcript_rows.filter(row=>row.row_kind==="assistant").at(-1)?.body;
            await closeSettings();
            const target=byId(role==="side_chat" ? "side-chat-prompt" : "prompt","TEXTAREA");
            const prompt=role==="main" ? "ツールを使わず6×7を計算し、LIVE_MAIN_OK 42 とだけ返してください。" : role==="side_chat" ? "ツールを使わず7×8を計算し、LIVE_SUB_OK 56 とだけ返してください。" :
              `この隔離試験フォルダー内のファイル1個だけを作成することを許可します。shellツールを1回だけ、sandbox_permissions= require_escalated で使い、次のPowerShellコマンドを実行してください。他の操作は不要です。\n[System.IO.File]::WriteAllText('${liveReceipt.replaceAll("'","''")}', 'LIVE_APPROVE_OK')\n完了したら LIVE_APPROVE_OK と返してください。`;
            await trustedClick(input,cdp,target,sink); await input.insertText(target,prompt);
            await trustedClick(input,cdp,action(role==="side_chat" ? "send-side-chat" : "send",role==="side_chat" ? "aside.side-chat-pane" : "section.composer"),sink);
            await wait(`Live ${role} prepares a new request`,()=>readdir(state.capture),files=>files.some(name=>name.endsWith(".request.json")&&!prior.has(name)));
            const result=await wait(`Live ${role} completes with a new assistant response`,()=>invokeDesktopCommand(cdp,"desktop_state"),value=>role==="side_chat"
              ? value.side_chat.status==="failed" || value.side_chat.status==="completed" && !value.side_chat.can_cancel && !value.side_chat.last_error && assistant(value)?.includes(marker)
              : value.run_status_key==="failed" || value.run_status_key==="completed" && assistant(value)?.includes(marker),role==="approve" ? 600_000 : 180_000);
            if(role!=="side_chat" && result.run_status_key!=="completed") throw fail("Live generation failed",{role,status:result.status_message});
            if(role==="side_chat" && result.side_chat.status!=="completed") throw fail("Live Sub generation failed",{error:result.side_chat.last_error});
            const selector=role==="side_chat" ? ".side-chat-message-assistant" : "main.conversation #thread article.message.assistant";
            await wait("Live assistant response is visible",()=>cdp.evaluate(`Array.from(document.querySelectorAll(${JSON.stringify(selector)})).at(-1)?.innerText`),value=>value?.includes(marker));
            if(role!=="side_chat") await wait("Live terminal status is visible",()=>cdp.evaluate('document.querySelector(".topbar .status-line")?.innerText'),value=>value?.includes(result.status_message));
            const captured=[];
            for(const file of (await readdir(state.capture)).filter(name=>name.endsWith(".request.json")&&!prior.has(name))) captured.push(JSON.parse(await readFile(path.join(state.capture,file),"utf8")));
            const guardianRequests=captured.filter(body=>body.messages?.some(message=>message.role==="system"&&String(message.content).includes("independent permission guardian")));
            if(!captured.length || !isDeepStrictEqual(fixtureCounts,providers.map(provider=>provider.requests.length))) throw fail("Live generation did not use the exclusively selected real upstream");
            if(role==="approve" && (!guardianRequests.length || await readFile(liveReceipt,"utf8")!=="LIVE_APPROVE_OK")) throw fail("Live approval did not complete both actual Guardian generation and the bounded effect");
            await sink.record("hub-live-role-generation",{role,model:liveProvider.model,logical_model_id:live.id,prepared_requests:captured.length,guardian_requests:guardianRequests.length,completed:true,bounded_effect:role==="approve",scope:"Actual configured model generation through the isolated Hub; this is a smoke result, not a general quality score."},{phase:"executing",owner:OWNER});
            await captureScenarioScreenshot({cdp,sink,name:`model-live-${role}`,owner:OWNER});
            await trustedClick(input,cdp,action("show-config","aside.sidebar"),sink);
          }
        }
        await sink.record("hub-model-propagation-complete", { explicit_main_preserved:true, default_change_requires_review:true, roles_with_active_and_next_generation:contexts, updated_instructions_forwarded:true, disabled_send_control:blocked.sendDisabled, disabled_request_forwarded:false, scope:"Actual Main/Sub/Approve Gateway requests and an authorized tool effect, with deterministic upstream responses. No live LLM quality claim." }, { phase:"executing", owner:OWNER });
        if (resource.pageErrors().length) throw fail("Hub browser page errors", resource.pageErrors());
        return { acquisition:"pass", oracle:"pass", manual:"pending" };
      } catch(error) {
        await resource.screenshot("model-propagation-hub-failure");
        await sink.record("hub-model-edit-failure",await page.locator("#edit-error").textContent(),{phase:"executing",owner:OWNER});
        await sink.record("hub-model-propagation-failure", { hub:await projection(), form:await form(), counts:providers.map(p=>p.requests.length) }, { phase:"executing", owner:OWNER });
        await captureScenarioScreenshot({ cdp, sink, name:"model-propagation-failure", owner:OWNER });
        throw error;
      } finally { state.held?.release(); await settleInput(); }
    },
    requestGracefulExit: cdp=>requestHubEnrollmentExit(cdp,state),
    async quiesce() {
      state.held?.release();
      await settleInput();
      state.close ??= state.resource ? await state.resource.close() : { pass:true, started:false };
      for (const provider of state.providers.splice(0)) { try { await provider.close(); } catch { state.failures.push("provider-close"); } }
      return { input:state.close.pass && !state.failures.length ? "pass":"fail", resources:[{kind:"hub-model-propagation",close:state.close,failures:state.failures}] };
    },
    async cleanup() { return { input:state.close?.pass && !state.failures.length ? "pass":"fail",resources:[] }; },
  });
}
