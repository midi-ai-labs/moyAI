import assert from 'node:assert/strict';
import test from 'node:test';
import path from 'node:path';
import { mkdir, mkdtemp, readFile, rmdir, stat, unlink, writeFile } from 'node:fs/promises';
import { createInitialSetupHubScenario, initialHubEntryReady, initialHubShellReady, initialHubPreferencesFailureAccepted, withBlockedInitialPreferences } from '../scenarios/settings_initial_setup_hub.mjs';
import { importDesktopHubParticipationFile } from '../scenarios/hub_browser_enrollment.mjs';
import { createScenario } from '../scenario_registry.mjs';

const workspace = 'C:/isolated/workspace';
function entry() {
  return { surface:{ projection:{ overlay:'initial_setup', config_target:{ workspacePath:workspace },
    startup:{ initial_setup_required:true, initial_setup_reason:'config_missing', setup_target:{ workspacePath:workspace } } },
    wizard:{ count:1, visible:true, current_step:'start' }, visible_fatal_count:0, visible_recoverable_error_count:0 },
    entry:{ count:1, visible:true, enabled:true } };
}
function shell() {
  const run = { workspacePath:workspace, sessionId:null, expectedState:{ kind:'idle' } };
  return { p:{ workspace_path:workspace, overlay:'none', startup:{ initial_setup_required:false, action_overlay:'none' },
    device_network:{ enrollment:'active', device_id:'device-1' }, hub:{ status:'connected', main_mode:'hub', main_confirmation:'confirmed',
      main_review:{ selection:{ preferred_model_id:'model-1', allowed_model_ids:['model-1'] } } },
    run_status_key:'idle', busy:false, navigation_loading:false, navigation_admission_open:true,
    background_mutation_pending:false, pending_async_operations:[], provider_loading:false, can_submit:true, run_target:run },
    prompt:{ count:1, enabled:true, center_hit:true }, composerRunTarget:structuredClone(run), wizardCount:0, blockingDialogs:0, visibleErrors:[] };
}

test('Initial Hub scenario uses the common lifecycle and a separate missing-config entry contract', () => {
  const scenario = createInitialSetupHubScenario();
  assert.equal(scenario.id, 'settings.initial-setup-hub'); assert.equal(scenario.manualGate, 'pending');
  for (const name of ['prepare','execute','requestGracefulExit','quiesce','cleanup']) assert.equal(typeof scenario[name], 'function');
  assert.equal('launch' in scenario, false);
});

test('Initial Hub options reach the factory through the actual CLI scenario registry', () => {
  const scenario = createScenario('settings.initial-setup-hub', { browserChannel:'msedge', headed:true });
  assert.equal(scenario.id, 'settings.initial-setup-hub');
  assert.throws(() => createScenario('settings.initial-setup-hub', { ignored:true }), /Hub browser options/);
});

test('Initial Hub entry rejects other setup step, owner and unavailable actual button', () => {
  assert.equal(initialHubEntryReady(entry(), workspace), true);
  for (const mutate of [
    v => { v.surface.projection.startup.setup_target.workspacePath = 'C:/other'; },
    v => { v.surface.projection.config_target.workspacePath = 'C:/other'; },
    v => { v.surface.projection.overlay = 'hub'; },
    v => { v.surface.projection.startup.initial_setup_reason = 'config_invalid'; },
    v => { v.surface.wizard.current_step = 'provider'; },
    v => { v.entry.count = 2; }, v => { v.entry.enabled = false; }, v => { v.entry.visible = false; },
  ]) { const value = entry(); mutate(value); assert.equal(initialHubEntryReady(value, workspace), false); }
});

test('Initial Hub completion requires exact approved device and Main selection with a settled actual shell', () => {
  const expected = { workspace, deviceId:'device-1', modelId:'model-1' };
  assert.equal(initialHubShellReady(shell(), expected), true);
  for (const mutate of [
    v => { v.p.startup.initial_setup_required = true; },
    v => { v.p.device_network.device_id = 'device-2'; },
    v => { v.p.device_network.enrollment = 'pending'; },
    v => { v.p.hub.main_review.selection.allowed_model_ids.push('other'); },
    v => { v.p.hub.main_confirmation = 'review_required'; },
    v => { v.p.hub.main_mode = 'direct'; },
    v => { v.p.pending_async_operations = ['config-save']; },
    v => { v.composerRunTarget.sessionId = 'old'; },
    v => { v.prompt.enabled = false; }, v => { v.prompt.center_hit = false; },
    v => { v.wizardCount = 1; }, v => { v.blockingDialogs = 1; }, v => { v.visibleErrors.push('failure'); },
  ]) { const value = shell(); mutate(value); assert.equal(initialHubShellReady(value, expected), false); }
});

test('Unknown native entry is rejected before any Desktop or native interaction', async () => {
  await assert.rejects(importDesktopHubParticipationFile({ entry:'injected-command' }), /Unknown Hub participation GUI entry/);
});

test('Preferences failure scenario uses the existing Hub options and lifecycle', () => {
  const scenario = createScenario('settings.initial-setup-hub-preferences-failure', { browserChannel:'msedge', headed:true });
  assert.equal(scenario.id, 'settings.initial-setup-hub-preferences-failure');
  assert.equal(scenario.manualGate, 'pending');
  for (const name of ['prepare','execute','requestGracefulExit','quiesce','cleanup']) assert.equal(typeof scenario[name], 'function');
  assert.equal('launch' in scenario, false);
});

test('Partial save oracle requires saved connection, live service and visible completion-only warning', () => {
  const expected = { workspace, url:'https://127.0.0.1:12345', warningCode:'initial_setup_preferences_save_failed' };
  const value = entry();
  value.surface.projection.workspace_path = workspace;
  value.surface.projection.status_code = expected.warningCode;
  Object.assign(value, { network:{ hub_url:expected.url, enrollment:'pending', request_id:'request-1' },
    visibleWarning:true, hubPending:true, configSaved:true, preferencesBlocked:true });
  assert.equal(initialHubPreferencesFailureAccepted(value, expected), true);
  for (const mutate of [
    v => { v.surface.projection.startup.initial_setup_required = false; },
    v => { v.surface.projection.status_code = 'plain'; },
    v => { v.surface.projection.overlay = 'hub'; },
    v => { v.surface.wizard.visible = false; },
    v => { v.network.hub_url = 'https://other'; },
    v => { v.network.enrollment = 'unconfigured'; },
    v => { v.network.request_id = null; },
    v => { v.visibleWarning = false; }, v => { v.hubPending = false; },
    v => { v.configSaved = false; }, v => { v.preferencesBlocked = false; },
    v => { v.surface.visible_recoverable_error_count = 1; },
  ]) { const changed = structuredClone(value); mutate(changed); assert.equal(initialHubPreferencesFailureAccepted(changed, expected), false); }
});

test('Preferences fault restores exact original bytes when the observed operation throws', async () => {
  const parent = path.resolve(import.meta.dirname, '../../../../project_sandbox/onboarding-followthrough-20260920/preferences-fixture-self-tests');
  await mkdir(parent, { recursive:true });
  const root = await mkdtemp(path.join(parent, 'isolated-'));
  const paths = Object.fromEntries(Object.entries({ config:'config', config_file:'config/config.toml', data:'data', prefs:'prefs', prefs_file:'prefs/desktop.toml', webview:'webview' })
    .map(([key, suffix]) => [key, path.join(root, suffix)]));
  await mkdir(paths.prefs);
  const original = 'last_workspace = "C:/fixture"\nonboarding_intent = "welcome"\n';
  await writeFile(paths.prefs_file, original);
  try {
    await assert.rejects(withBlockedInitialPreferences({ root, paths }, async () => {
      assert.equal((await stat(paths.prefs_file)).isDirectory(), true);
      throw new Error('intentional observation failure');
    }), /intentional observation failure/);
    assert.equal(await readFile(paths.prefs_file, 'utf8'), original);
    await assert.rejects(stat(path.join(paths.prefs, 'desktop.before-save-failure.toml')), { code:'ENOENT' });
  } finally {
    await unlink(paths.prefs_file); await rmdir(paths.prefs); await rmdir(root);
  }
});
