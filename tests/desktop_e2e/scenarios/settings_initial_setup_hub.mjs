import { readFile, stat } from 'node:fs/promises';
import { isDeepStrictEqual } from 'node:util';
import { DesktopE2eError } from '../core/execution.mjs';
import { DesktopCommandProbe, assertExactDesktopCommandSequence } from '../drivers/desktop_command_probe.mjs';
import { normalizeHubBrowserOptions, startHubBrowserResource } from '../drivers/hub_browser_resource.mjs';
import { WebviewInput } from '../drivers/webview_input.mjs';
import { prepareDesktopFixture } from './fixture.mjs';
import { captureScenarioScreenshot } from './observations.mjs';
import { observeInitialSetupSurface } from './settings_initial_setup.mjs';
import { action, trustedClick, wait, enrollDesktopFromHubBrowser, requestHubEnrollmentExit } from './hub_browser_enrollment.mjs';

const ID = 'settings.initial-setup-hub', OWNER = `scenario:${ID}`;
const WATCHED = ['device_network_initial_setup_import', 'finish_initial_setup', 'submit_prompt', 'submit_side_chat'];
const failure = (message, evidence) => new DesktopE2eError('product', 'initial-setup-hub-mismatch', message, evidence);

export function initialHubEntryReady(value, workspace) {
  const p = value?.surface?.projection, button = value?.entry;
  return p?.overlay === 'initial_setup' && p.startup?.initial_setup_required === true
    && p.startup.initial_setup_reason === 'config_missing' && p.startup.setup_target?.workspacePath === workspace
    && p.config_target?.workspacePath === workspace && value.surface.wizard?.current_step === 'start'
    && value.surface.wizard?.count === 1 && value.surface.wizard.visible === true
    && button?.count === 1 && button.visible === true && button.enabled === true
    && value.surface.visible_fatal_count === 0 && value.surface.visible_recoverable_error_count === 0;
}

export function initialHubShellReady(value, { workspace, deviceId, modelId }) {
  const p = value?.p, selection = p?.hub?.main_review?.selection;
  return p?.workspace_path === workspace && p.overlay === 'none' && p.startup?.initial_setup_required === false
    && p.startup.action_overlay === 'none' && p.device_network?.enrollment === 'active'
    && p.device_network.device_id === deviceId && p.hub?.status === 'connected'
    && p.hub.main_mode === 'hub' && p.hub.main_confirmation === 'confirmed'
    && selection?.preferred_model_id === modelId && isDeepStrictEqual(selection.allowed_model_ids, [modelId])
    && p.run_status_key === 'idle' && p.busy === false && p.navigation_loading === false
    && p.navigation_admission_open === true && p.background_mutation_pending === false
    && p.pending_async_operations?.length === 0 && p.provider_loading === false && p.can_submit === true
    && value.prompt?.count === 1 && value.prompt.enabled === true && value.prompt.center_hit === true
    && value.wizardCount === 0 && value.blockingDialogs === 0 && value.visibleErrors?.length === 0
    && isDeepStrictEqual(value.composerRunTarget, p.run_target);
}

async function observeEntry(cdp) {
  const surface = await observeInitialSetupSurface(cdp);
  const entry = await cdp.evaluate(`(() => {
    const nodes = [...document.querySelectorAll('[data-surface="initial-setup"] #initial-setup-hub')], e = nodes[0];
    const r = e?.getBoundingClientRect(), s = e ? getComputedStyle(e) : null;
    return { count:nodes.length, visible:Boolean(r?.width && r?.height && s?.display !== 'none' && s?.visibility !== 'hidden'
      && Number(s?.opacity) > 0 && !e.closest('[hidden],[inert],[aria-hidden="true"]')),
      enabled:Boolean(e && !e.disabled && e.getAttribute('aria-disabled') !== 'true') };
  })()`);
  return { surface, entry };
}

async function observeShell(cdp) {
  return cdp.evaluate(`(async () => {
    const p = await window.__TAURI_INTERNALS__.invoke('desktop_state');
    const visible = e => Boolean(e?.isConnected && e.getClientRects().length && getComputedStyle(e).display !== 'none'
      && getComputedStyle(e).visibility !== 'hidden' && Number(getComputedStyle(e).opacity) !== 0
      && !e.closest('[hidden],[inert],[aria-hidden="true"]'));
    const nodes = [...document.querySelectorAll('section.composer #prompt')], prompt = nodes[0], r = prompt?.getBoundingClientRect();
    const composer = prompt?.closest('section.composer');
    return { p, composerRunTarget:composer?.dataset.runTarget ? JSON.parse(composer.dataset.runTarget) : null,
      prompt:{count:nodes.length, enabled:visible(prompt) && !prompt.disabled && !prompt.readOnly,
        center_hit:Boolean(r?.width && r?.height && document.elementFromPoint(r.left+r.width/2,r.top+r.height/2) === prompt)},
      wizardCount:document.querySelectorAll('[data-surface="initial-setup"]').length,
      blockingDialogs:[...document.querySelectorAll('[role="dialog"],[role="alertdialog"],[data-modal]')].filter(visible).length,
      visibleErrors:[...document.querySelectorAll('.fatal,.ui-error-notice')].filter(visible).map(e=>e.textContent.trim()) };
  })()`);
}

export function createInitialSetupHubScenario(options = {}) {
  const settings = normalizeHubBrowserOptions(options);
  const state = { resource:null, input:null, commands:null, nativeOwner:null, nativeCandidate:null,
    nativeBefore:null, importDispatched:false, failures:[], close:null };
  async function settleProbes() {
    for (const key of ['commands', 'input']) if (state[key]) {
      try { await (key === 'commands' ? state[key].remove() : state[key].cleanup()); }
      catch (error) { state.failures.push({ resource:key, code:error?.code ?? 'cleanup-failed' }); }
      state[key] = null;
    }
  }
  return Object.freeze({ id:ID, productOracle:'pass', manualGate:'pending', databaseRequired:true,
    async prepare(args) {
      await prepareDesktopFixture({ ...args, owner:OWNER, configMode:'absent',
        sentinelName:'E2E_INITIAL_HUB.txt', sentinelText:'Isolated missing-config Initial Setup with real Hub.\n' });
      state.resource = await startHubBrowserResource({ ...args, options:settings });
    },
    requestGracefulExit: cdp => requestHubEnrollmentExit(cdp, state),
    async execute({ context, runtime, driver:cdp, sink }) {
      const resource = state.resource, { page, hub, provider } = resource;
      await cdp.call('Runtime.enable'); await cdp.call('DOM.enable'); await cdp.call('Accessibility.enable');
      const input = state.input = new WebviewInput(cdp, { probeId:ID });
      const commands = state.commands = new DesktopCommandProbe(cdp, { probeId:ID, commands:WATCHED });
      try {
        await input.installProbe(); await commands.install();
        const before = await wait('Missing config shows the actual Initial Hub entry', () => observeEntry(cdp), v => initialHubEntryReady(v, context.paths.workspace));
        await captureScenarioScreenshot({ cdp, sink, name:'initial-hub-start-entry', owner:OWNER });
        await page.goto(hub.url); await page.locator('#management-status').filter({ hasText:'Hub本体に接続中' }).waitFor();
        await page.locator('nav a[href="#models"]').click();
        await page.locator('#endpoint').fill(provider.url); await page.locator('#discover').click();
        await page.locator('#model option[value="fixture-alpha"]').waitFor({ state:'attached' });
        await page.locator('#model').selectOption('fixture-alpha'); await page.locator('#label').fill('Initial Hub Main');
        await page.locator('#allow-tools').check(); await page.locator('#register').click();
        await page.locator('#model-rows tr').filter({ hasText:'Initial Hub Main' }).waitFor();
        const models = (await hub.command('hub_snapshot')).store.catalog.models;
        if (models.length !== 1 || models[0].label !== 'Initial Hub Main') throw failure('Expected one browser-registered model', { models });
        await page.locator('nav a[href="#device-network"]').click();
        await page.locator('#network-ip').fill('127.0.0.1'); await page.locator('#network-port').fill(String(hub.networkPort));
        await page.locator('#network-start').click(); await page.locator('#network-stop').waitFor();
        await hub.observeNetwork();
        // Entry choice changes only the real Initial Setup activation. Download, exact native owner,
        // picker selection, pending/approval UI and lifecycle remain the shared enrollment path.
        const enrolled = await enrollDesktopFromHubBrowser({ resource, context, runtime, cdp, input, sink, nativeState:state, entry:'initial-setup' });
        const command = assertExactDesktopCommandSequence(await commands.snapshot(), { expected:[{
          command:'device_network_initial_setup_import', args:{ expectedSetupTarget:before.surface.projection.startup.setup_target,
            expectedConfigTarget:before.surface.projection.config_target },
        }] });
        await captureScenarioScreenshot({ cdp, sink, name:'initial-hub-approved-devices', owner:OWNER });
        await trustedClick(input, cdp, action('close-overlay', '[role="dialog"][data-modal="hub"] .hub-modal-footer'), sink);
        const shell = await wait('Initial Hub import completes setup and enables the normal Main shell', () => observeShell(cdp),
          v => initialHubShellReady(v, { workspace:context.paths.workspace, deviceId:enrolled.network.device_id, modelId:models[0].id }));
        const config = await readFile(context.paths.config_file, 'utf8'), configStat = await stat(context.paths.config_file);
        if (!config.includes(`https://127.0.0.1:${hub.networkPort}`) || !config.includes('BEGIN CERTIFICATE') || config.includes('PRIVATE KEY')) {
          throw failure('Initial Hub flow must persist the same endpoint and only public CA in config', { path:context.paths.config_file });
        }
        if (provider.requests.some(request => request.method !== 'GET')) throw failure('Initial Hub setup unexpectedly generated model content', { requests:provider.requests });
        if (resource.pageErrors().length) throw failure('Hub browser raised an unhandled error', resource.pageErrors());
        await resource.screenshot('initial-hub-browser-approved');
        await captureScenarioScreenshot({ cdp, sink, name:'initial-hub-completed-shell', owner:OWNER });
        await sink.record('initial-hub-complete', { command, device_id:enrolled.network.device_id, request_id:enrolled.requestId,
          model_id:models[0].id, shell, config:{ path:context.paths.config_file, size_bytes:configStat.size },
          provider_requests:provider.requests, scope:'Initial Hub button, native public config import, browser approval and Main shell; no generation, restart, Side route or physical peer.' },
          { phase:'executing', owner:OWNER });
        return { acquisition:'pass', oracle:'pass', manual:'pending' };
      } finally { await settleProbes(); }
    },
    async quiesce() {
      await settleProbes();
      state.close ??= state.resource ? await state.resource.close() : { pass:true, started:false };
      return { input:state.close.pass && state.failures.length === 0 ? 'pass':'fail', resources:[{ kind:ID, close:state.close, failures:state.failures }] };
    },
    async cleanup() { return { input:state.close?.pass && state.failures.length === 0 ? 'pass':'fail', resources:[] }; },
  });
}
