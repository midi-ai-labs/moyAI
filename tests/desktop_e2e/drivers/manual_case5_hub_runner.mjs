import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { createDesktopRunContext } from '../core/run_context.mjs';
import { executeDesktopScenario } from '../core/desktop_execution.mjs';
import { DesktopE2eError } from '../core/execution.mjs';
import { WindowsTauriHost } from './windows_tauri_host.mjs';
import { createManualCase5Scenario, normalizeManualCase5Options, manualCase5Input, manualCase5ScopeFailures } from '../scenarios/manual_case5.mjs';
import { acquireInteractiveShell } from '../scenarios/shell_baseline.mjs';
import { startHubBrowserResource } from './hub_browser_resource.mjs';
import { prepareOnboardingHubModels, onboardingHubModelsAccepted, takeOnboardingUpstreamApiKey } from '../scenarios/onboarding_winab.mjs';
import { enrollDesktopFromHubBrowser, requestHubEnrollmentExit, trustedClick, trustedFocus, wait, byId, action } from '../scenarios/hub_browser_enrollment.mjs';
import { ManualLiveSession, normalizeManualLiveProviderBaseUrl, manualLiveClick, manualLivePermissionRequest, manualLivePermissionLocator, manualLiveGeneratedFiles, manualLiveExportTranscript, manualLiveHistory } from './manual_live_session.mjs';
import { observeProviderTurnSurface, providerRenderedTerminalFailures } from '../scenarios/provider_restart.mjs';
import { captureScenarioScreenshot } from '../scenarios/observations.mjs';
import { assertTrustedTextInsertion, assertTrustedProbeSequence } from './webview_input.mjs';
import { DesktopCommandProbe } from './desktop_command_probe.mjs';
import { operatorRequestFingerprint, waitForOperatorReview } from './operator_review.mjs';
import { inventoryCase52CleanSeed } from '../core/clean_seed.mjs';
import { MANUAL_CASE5_COPY_RULE } from '../scenarios/manual_case5.mjs';
import { freshExecutionId } from '../run_scenario.mjs';
import { canonicalUlid } from '../core/canonical_identity.mjs';

export function normalizeManualCase5HubArguments(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)
    || Object.keys(value).some(key => !['taskRoot', 'packageRoot', 'scenarioConfigPath', 'captureHttpRequests'].includes(key))) throw new TypeError('Expected taskRoot, packageRoot, scenarioConfigPath and optional captureHttpRequests');
  if (value.captureHttpRequests !== undefined && typeof value.captureHttpRequests !== 'boolean') throw new TypeError('captureHttpRequests must be boolean');
  const paths = Object.fromEntries(['taskRoot', 'packageRoot', 'scenarioConfigPath'].map(name => {
    const candidate = value[name];
    if (typeof candidate !== 'string' || !path.isAbsolute(candidate) || candidate.includes('\0')) throw new TypeError(`${name} must be an absolute path`);
    return [name, path.resolve(candidate)];
  }));
  return Object.freeze({ ...paths, ...(value.captureHttpRequests === undefined ? {} : { captureHttpRequests: value.captureHttpRequests }) });
}

export function normalizeManualCase5HubConfiguration(raw) {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) throw new TypeError('Hub Case5 requires explicit configuration');
  const { hub_side_provider_base_url, hub_approve_provider_base_url, ...scenarioRaw } = raw;
  const options = normalizeManualCase5Options(scenarioRaw);
  if (options.observationTimeoutMs !== undefined) throw new TypeError('This Hub Case5 route requires an unbounded case observation budget');
  if (options.approvalMode !== 'operator') throw new TypeError('Hub Case5 requires operator approval mode and retained stdin');
  if (!options.apiKeyEnv || !options.sideModel || !options.approveModel) throw new TypeError('Specify all three role models and the Main credential environment name');
  return Object.freeze({ ...options, scenarioRaw: Object.freeze(scenarioRaw),
    sideProviderBaseUrl: hub_side_provider_base_url === undefined ? options.providerBaseUrl : normalizeManualLiveProviderBaseUrl(hub_side_provider_base_url),
    approveProviderBaseUrl: hub_approve_provider_base_url === undefined ? options.providerBaseUrl : normalizeManualLiveProviderBaseUrl(hub_approve_provider_base_url) });
}

export function takeManualCase5HubCredentials(options, environment = process.env) {
  const roles = { main: options.apiKeyEnv, side_chat: options.sideApiKeyEnv, approve: options.approveApiKeyEnv };
  const names = [...new Set(Object.values(roles).filter(Boolean).map(name => name.toUpperCase()))];
  const credentials = new Map();
  try {
    for (const name of names) credentials.set(name, takeOnboardingUpstreamApiKey(name, environment));
    return Object.fromEntries(Object.entries(roles).map(([role, name]) => [role, name ? credentials.get(name.toUpperCase()) : null]));
  } finally {
    for (const name of Object.keys(environment)) if (names.includes(name.toUpperCase()) || name.toUpperCase() === 'MOYAI_HTTP_REQUEST_CAPTURE_DIR') delete environment[name];
  }
}

export function validateManualCase5HubBuild(identity, observedSha256) {
  if (!identity || typeof identity.version !== 'string' || !identity.version
    || !/^[a-f0-9]{40}$/u.test(identity.git_commit ?? '') || !/^[a-f0-9]{64}$/u.test(identity.sha256 ?? '')
    || identity.sha256 !== observedSha256) throw new TypeError('Hub binary does not match its build identity');
  return Object.freeze({ ...identity });
}

export function manualCase5HubRequiresClosedAudit(projection) {
  if (!Array.isArray(projection?.transcript_rows) || !Number.isSafeInteger(projection.turn_page_offset)
    || !Number.isSafeInteger(projection.turn_page_total) || projection.turn_page_offset < 0 || projection.turn_page_total < 0) return true;
  return projection.turn_page_offset > 0 || projection.turn_page_total > projection.transcript_rows.length;
}

// Hub heartbeats can keep polling while the exact conversation and rendered UI are idle.
export function manualCase5HubTerminalReady(s) {
  const p = s?.projection;
  if (!p) return false;
  return p.hub?.status === 'connected' && p.hub.main_mode === 'hub' && p.hub.main_confirmation === 'confirmed'
    && p.busy === false && p.post_run_refresh_pending === false && p.background_mutation_pending === false
    && p.pending_async_operations?.length === 0 && p.task_activity_state === 'idle' && p.agent_tree_active === false
    && p.run_status_key === 'completed' && p.run_target?.expectedState?.kind === 'idle'
    && p.run_target.sessionId === p.draft_target?.sessionId && p.composer_submit_mode === 'new_request'
    && p.can_submit === true && !p.confirmation_visible && s.visible_fatal_count === 0 && s.visible_recoverable_error_count === 0
    && p.navigation_loading === false && p.hub.active_main === null && p.hub.active_side_chat === null && p.hub.active_approve === null
    && canonicalUlid(p.run_target?.sessionId) && canonicalUlid(p.run_target?.expectedState?.latestTurnId)
    && providerRenderedTerminalFailures(s).length === 0;
}

export async function runManualCase5HubRunner(value) {
  const { taskRoot, packageRoot, scenarioConfigPath, captureHttpRequests = false } = normalizeManualCase5HubArguments(value);
  const configBytes = await readFile(scenarioConfigPath);
  const raw = JSON.parse(configBytes.toString('utf8'));
  const options = normalizeManualCase5HubConfiguration(raw);
  const fixture_source = options.fixtureSource;
  const credentials = takeManualCase5HubCredentials(options);
  const hubBinary = path.join(packageRoot, 'hub/bin/moyai-hub.exe');
  const [hubBytes, hubIdentityBytes] = await Promise.all([readFile(hubBinary), readFile(path.join(packageRoot, 'hub/build-identity.json'))]);
  const hubSha256 = createHash('sha256').update(hubBytes).digest('hex');
  const hubIdentity = validateManualCase5HubBuild(JSON.parse(hubIdentityBytes.toString('utf8')), hubSha256);
  const base = createManualCase5Scenario(options.scenarioRaw);
  const outputs = ['README.md', 'basic_design.md', 'detail_design.md'];
  const owner = 'scenario:manual.case5';
  const nativeState = {};
  let resource = null, live = null, sendProbe = null, liveCleanup = null, hubClose = null, baseline = null;
  const prepared = await createDesktopRunContext({ artifactParent: taskRoot, binary: path.join(packageRoot, 'app/bin/moyai-desktop.exe'),
    executionId: freshExecutionId(), scenarioId: 'manual.case5', desktopIsolation: 'user-wide',
    harnessRoot: path.resolve(import.meta.dirname, '..'), scenarioConfig: { path: scenarioConfigPath, sha256: createHash('sha256').update(configBytes).digest('hex'), size_bytes: configBytes.length } });
  const { context, sink } = prepared;
  await writeFile(path.join(taskRoot, 'active-execution.json'), JSON.stringify({ root: context.root, started_at: context.manifest.started_at }, null, 2));
  const record = (name, value) => sink.record(name, value, { phase: 'executing', owner });
  const fail = (code, message, evidence) => new DesktopE2eError('product', code, message, evidence);

  const scenario = {
    id: base.id, productOracle: 'pass', manualGate: 'pending', databaseRequired: true,
    environment: captureHttpRequests ? { MOYAI_HTTP_REQUEST_CAPTURE_DIR: path.join(context.paths.data, 'http-request-capture') } : {},
    async prepare(args) {
      await base.prepare(args);
      baseline = await manualLiveGeneratedFiles(context, sink, 'case5/baseline-hub', outputs);
      resource = await startHubBrowserResource({ ...args, options: { hubBinary: path.join(packageRoot, 'hub/bin/moyai-hub.exe'), headed: true } });
      const expected = await prepareOnboardingHubModels({ page: resource.page, hub: resource.hub, providerBaseUrl: options.providerBaseUrl,
        mainModel: options.model, sideModel: options.sideModel, approveModel: options.approveModel, apiKey: credentials.main,
        roleProviders: { side_chat: { providerBaseUrl: options.sideProviderBaseUrl, apiKey: credentials.side_chat },
          approve: { providerBaseUrl: options.approveProviderBaseUrl, apiKey: credentials.approve } } });
      scenario.expected = expected;
      await resource.page.locator('nav a[href="#device-network"]').click();
      await resource.page.locator('#network-ip').fill('127.0.0.1');
      await resource.page.locator('#network-port').fill(String(resource.hub.networkPort));
      await resource.page.locator('#network-start').click();
      await wait('Hub listens for this case', () => resource.hub.observeNetwork(), p => p.server.running === true);
      await resource.screenshot('case5-hub-models-and-network');
      await sink.writeJson('case5/hub-build.json', { path: hubBinary, sha256: hubSha256, identity: hubIdentity });
      await sink.writeJson('case5/capture-policy.json', { raw_http_request_capture: captureHttpRequests,
        private_reasoning_may_be_included: captureHttpRequests,
        handling: 'Keep raw task-local evidence private; report only reasoning field presence, length and hash.' });
    },
    async execute(args) {
      const { driver, runtime } = args;
      try {
      await acquireInteractiveShell(args, { evidenceOwner: owner, screenshotStem: 'case5-initial' });
      // These direct-role fields remain dormant after Hub enrollment; no key reaches Desktop.
      live = new ManualLiveSession({ context, driver, sink, options, owner, stem: 'case5', capturePaths: outputs });
      await live.open();
      await enrollDesktopFromHubBrowser({ resource, context: { ...context, paths: { ...context.paths, workspace: resource.downloads } },
        runtime, cdp: driver, input: live.input, sink, nativeState });
      await wait('Main/Sub use the requested Hub defaults', () => driver.evaluate("window.__TAURI_INTERNALS__.invoke('hub_projection')"),
        p => onboardingHubModelsAccepted(p, { ...scenario.expected, approve: null }));
      await trustedClick(live.input, driver, action('show-config', '[data-surface="hub"]'), sink);
      const approve = byId('ai-approve-model', 'SELECT');
      const values = await wait('Approve model is offered', () => driver.evaluate(`Array.from(document.querySelector('#ai-approve-model')?.options ?? []).filter(o => !o.disabled).map(o => o.value)`),
        rows => rows.includes(scenario.expected.approve));
      await trustedFocus(live.input, driver, approve);
      const selectionStart = (await live.input.snapshotProbe()).sequence;
      const selectionKeys = ['Home', ...Array(values.indexOf(scenario.expected.approve)).fill('ArrowDown'), 'Tab'];
      for (const key of selectionKeys) await live.input.pressKey(key);
      const selectionProbe = assertTrustedProbeSequence(await live.input.snapshotProbe(selectionStart), { afterSequence: selectionStart,
        expected: selectionKeys.map(key => ({ type: 'keydown', identity: approve.identity, key })) });
      await wait('Approve choice is retained', () => driver.evaluate("document.querySelector('#ai-approve-model')?.value"), value => value === scenario.expected.approve);
      await record('case5-hub-approve-keyboard', { target: approve, selected_id: scenario.expected.approve, probe: selectionProbe });
      await trustedClick(live.input, driver, action('hub-save-approve', '[data-ai-connection="approve"]'), sink);
      const hub = await wait('All role selections are confirmed', () => driver.evaluate("window.__TAURI_INTERNALS__.invoke('hub_projection')"), p => onboardingHubModelsAccepted(p, scenario.expected));
      await captureScenarioScreenshot({ cdp: driver, sink, name: 'case5-hub-selections', owner });
      await record('case5-hub-confirmed', hub);
      await trustedClick(live.input, driver, action('close-overlay', '.settings-modal'), sink);
      const prompt = manualCase5Input(await readFile(new URL('../../manual_ST/case5/spec.md', import.meta.url), 'utf8')).prompt;
      const promptTarget = { selector: 'section.composer textarea#prompt', identity: { tag: 'TEXTAREA', id: 'prompt' } };
      await manualLiveClick(live.input, promptTarget, sink, 'focus-prompt', { owner, stem: 'case5' });
      const before = await live.input.snapshotProbe();
      await live.input.insertText(promptTarget, prompt);
      const typing = assertTrustedTextInsertion(await live.input.snapshotProbe(before.sequence), { afterSequence: before.sequence, identity: promptTarget.identity, text: prompt });
      await wait('Canonical prompt is ready', () => observeProviderTurnSurface(driver), s => s.prompt?.value === prompt && s.send?.enabled === true);
      await live.commands.remove();
      live.commands = null;
      sendProbe = new DesktopCommandProbe(driver, { probeId: 'case5-submit', commands: ['submit_prompt', 'answer_permission', 'cancel_run'] });
      await sendProbe.install();
      live.observationBudget.beginOnce();
      await manualLiveClick(live.input, { selector: 'section.composer button[data-action="send"]', identity: { tag: 'BUTTON', action: 'send' } }, sink, 'send-canonical-prompt', { owner, stem: 'case5' });
      await record('case5-prompt-sent', { prompt, typing, commands: await sendProbe.snapshot(), deadline: null });
      await captureScenarioScreenshot({ cdp: driver, sink, name: 'case5-request-sent', owner });
      let lastProgress = 0, terminal = null, observedGateway = false;
      const approvals = [];
      while (terminal === null) {
        const surface = await observeProviderTurnSurface(driver);
        const p = surface.projection;
        const snapshot = await resource.hub.command('hub_snapshot');
        if (snapshot.gateway?.responding > 0 || snapshot.gateway?.running > 0) {
          if (!observedGateway) await record('case5-first-live-gateway', { projection: p, gateway: snapshot.gateway });
          observedGateway = true;
        }
        if (p.confirmation_visible) {
          const request = manualLivePermissionRequest(p);
          const fingerprint = operatorRequestFingerprint(request);
          const evidence = await sink.writeJson(`case5/approvals/${approvals.length + 1}-request.json`, { ...request, request_sha256: fingerprint });
          await captureScenarioScreenshot({ cdp: driver, sink, name: `case5-approval-${approvals.length + 1}`, owner });
          const reviewed = await waitForOperatorReview(request, { evidence, timeoutMs: 300000 });
          if (reviewed.status !== 'decided' || reviewed.decision !== 'approve') throw new DesktopE2eError('harness', 'case5-operator-incomplete', 'The pending operation was not approved', reviewed);
          const current = manualLivePermissionRequest((await observeProviderTurnSurface(driver)).projection);
          if (operatorRequestFingerprint(current) !== fingerprint) throw new Error('Permission owner changed during review');
          await manualLiveClick(live.input, manualLivePermissionLocator(request, 'approve'), sink, 'approve-reviewed-operation', { owner, stem: 'case5' });
          approvals.push({ request, reviewed });
        } else if (manualCase5HubTerminalReady(surface)) terminal = surface;
        else if ((['failed', 'cancelled', 'interrupted'].includes(p.run_status_key) && !p.busy)
          || surface.visible_fatal_count > 0 || surface.visible_recoverable_error_count > 0) {
          await sink.writeJson('case5/failed-terminal.json', surface);
          await captureScenarioScreenshot({ cdp: driver, sink, name: 'case5-failed-terminal', owner });
          throw fail('case5-normal-completion', 'Case5 did not reach a normal completed terminal', { status: p.run_status_key, detail: p.status_detail });
        }
        if (Date.now() - lastProgress >= 15000 || terminal !== null) {
          lastProgress = Date.now();
          await record('case5-progress', { status: p.run_status_key, message: p.status_message, detail: p.status_detail,
            run_target: p.run_target, hub: p.hub, paging: { offset: p.turn_page_offset, limit: p.turn_page_limit, total: p.turn_page_total },
            recent_rows: p.transcript_rows?.slice(-6), usage: { label: p.session_usage_label, title: p.session_usage_title },
            gateway: snapshot.gateway, observed_gateway: observedGateway });
          await writeFile(path.join(taskRoot, 'progress.json'), JSON.stringify({ at: new Date().toISOString(), root: context.root, status: p.run_status_key, status_message: p.status_message, status_detail: p.status_detail,
            history_rows: p.transcript_rows?.length, gateway: snapshot.gateway, active_main: p.hub?.active_main, observed_gateway: observedGateway }, null, 2));
          process.stdout.write(JSON.stringify({ type: 'case5-progress', at: new Date().toISOString(), status: p.run_status_key, status_message: p.status_message, history_rows: p.transcript_rows?.length, gateway: snapshot.gateway, observed_gateway: observedGateway }) + '\n');
        }
        if (terminal === null) await new Promise(resolve => setTimeout(resolve, 1000));
      }
      await sink.writeJson('case5/terminal.json', terminal);
      await captureScenarioScreenshot({ cdp: driver, sink, name: 'case5-terminal', owner });
      const generated = await manualLiveGeneratedFiles(context, sink, 'case5/generated', outputs);
      const failures = manualCase5ScopeFailures(baseline, generated);
      const sourceNow = await inventoryCase52CleanSeed(fixture_source, { copyRule: MANUAL_CASE5_COPY_RULE });
      const copiedNow = await inventoryCase52CleanSeed(context.paths.workspace, { copyRule: MANUAL_CASE5_COPY_RULE });
      const seed = JSON.parse(await readFile(path.join(context.root, 'evidence/case5/fixture-copy.json'), 'utf8'));
      if (sourceNow.aggregate_sha256 !== seed.aggregate_sha256) failures.push('fixture-source-changed');
      const originalFiles = new Map(seed.files.map(row => [row.path, row]));
      if (copiedNow.files.some(row => !outputs.includes(row.path) && (originalFiles.get(row.path)?.sha256 !== row.sha256))
        || seed.files.some(row => !copiedNow.files.some(after => after.path === row.path && after.sha256 === row.sha256))) failures.push('source-config-test-inventory-changed');
      if (!observedGateway) failures.push('no-observed-live-gateway');
      const history = manualLiveHistory(terminal.projection);
      history.paging = { offset: terminal.projection.turn_page_offset, limit: terminal.projection.turn_page_limit, total: terminal.projection.turn_page_total };
      history.complete_history_requires_closed_database_audit = manualCase5HubRequiresClosedAudit(terminal.projection);
      if (!history.complete_history_requires_closed_database_audit && JSON.stringify(history.users) !== JSON.stringify([prompt])) failures.push('canonical-user-request-mismatch');
      const commands = await sendProbe.snapshot();
      if (commands.calls.filter(c => c.command === 'submit_prompt').length !== 1 || commands.calls.some(c => c.command === 'cancel_run')) failures.push('send-command-count');
      const transcript = await manualLiveExportTranscript({ context, driver, input: live.input, sink, sessionId: terminal.projection.run_target.sessionId, prompt, owner, stem: 'case5' });
      await sink.writeJson('case5/summary.json', { machine_gate: failures.length ? 'fail' : 'pass', failures, generated, history, transcript, approvals, commands,
        observed_gateway: observedGateway, polling: { raw_async_polling_required: terminal.projection.async_polling_required, reason: 'connected Hub heartbeat', all_run_idle_conditions_checked: true }, manual_verdict: 'pending' });
      if (failures.length) throw fail('case5-machine-gate', 'Case5 failed its required predicates', { failures });
      return { acquisition: 'pass', oracle: 'pass', manual: 'pending' };
      } finally {
        if (sendProbe) { await sendProbe.remove(); sendProbe = null; }
        if (live) { liveCleanup = await live.close(); live = null; }
        if (liveCleanup?.input?.failure || liveCleanup?.commands?.failure) throw new DesktopE2eError('harness', 'case5-input-cleanup', 'Input probes did not settle', liveCleanup);
      }
    },
    requestGracefulExit: driver => requestHubEnrollmentExit(driver, nativeState),
    async quiesce() {
      const failures = [];
      if (sendProbe) { try { await sendProbe.remove(); } catch (e) { failures.push(e.message); } }
      if (live) { try { liveCleanup = await live.close(); if (liveCleanup.input?.failure || liveCleanup.commands?.failure) failures.push('live-input-cleanup'); } catch (e) { failures.push(e.message); } }
      await base.quiesce();
      hubClose = resource ? await resource.close() : { pass: true, started: false };
      return { input: failures.length === 0 && hubClose.pass ? 'pass' : 'fail', resources: [{ kind: 'hub-browser', close: hubClose, input_failures: failures }] };
    },
    async cleanup() { return { input: hubClose?.pass ? 'pass' : 'fail', resources: [] }; },
  };
  const result = await executeDesktopScenario({ context, scenario, host: new WindowsTauriHost(), sink });
  await writeFile(path.join(taskRoot, 'run-result.json'), JSON.stringify(result, null, 2));
  return result;
}

export function parseManualCase5HubRunnerArguments(argv) {
  const names = new Map([['--task-root', 'taskRoot'], ['--package-root', 'packageRoot'], ['--scenario-config', 'scenarioConfigPath'], ['--capture-http-requests', 'captureHttpRequests']]);
  const value = {};
  for (let index = 0; index < argv.length; index += 2) {
    const name = names.get(argv[index]);
    if (!name || Object.hasOwn(value, name) || !argv[index + 1] || argv[index + 1].startsWith('--')) throw new TypeError('Expected unique --task-root, --package-root, --scenario-config absolute paths');
    if (name === 'captureHttpRequests' && !['true', 'false'].includes(argv[index + 1])) throw new TypeError('--capture-http-requests must be true or false');
    value[name] = name === 'captureHttpRequests' ? argv[index + 1] === 'true' : argv[index + 1];
  }
  return normalizeManualCase5HubArguments(value);
}

if (path.resolve(process.argv[1] ?? '') === fileURLToPath(import.meta.url)) {
  const result = await runManualCase5HubRunner(parseManualCase5HubRunnerArguments(process.argv.slice(2)));
  process.stdout.write(JSON.stringify({ type: 'case5-result', execution_root: result.execution_root,
    classification: result.result.classification, inputs: result.result.inputs,
    started_at: result.result.started_at, finished_at: result.result.finished_at, elapsed_ms: result.result.elapsed_ms }, null, 2) + '\n');
  process.exitCode = result.result.classification === 'manual_pending' || result.result.classification === 'pass' ? 0 : 1;
}
