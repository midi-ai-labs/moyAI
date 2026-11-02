import assert from 'node:assert/strict';
import test from 'node:test';
import { readFile } from 'node:fs/promises';
import { auditGuiCoverage } from '../gui_coverage.mjs';
import { SETTINGS_CONTROL_PLAN } from '../scenarios/settings_control_plan.mjs';
import { scenarioIds } from '../scenario_registry.mjs';

test('every current Desktop action has a state, operation and expected result in the GUI runbook', async () => {
  const audit = auditGuiCoverage(await readFile(new URL('../MANUAL_GUI_COVERAGE.md', import.meta.url), 'utf8'));
  assert.deepEqual(audit.missing_actions, [], 'new public actions need a GUI case');
  assert.deepEqual(audit.unknown_actions, [], 'retired public actions must not be counted as current cases');
  assert.deepEqual(audit.duplicate_cases, []);
  assert.deepEqual(audit.incomplete_cases, []);
});

test('field runbook lists the complete GUI operation plan without stale field keys', async () => {
  const doc = await readFile(new URL('../MANUAL_GUI_COVERAGE.md', import.meta.url), 'utf8');
  const keys = [...doc.matchAll(/^\| `([a-z_]+\.[a-z_]+)` \|/gm)].map(match => match[1]).sort();
  assert.deepEqual(keys, SETTINGS_CONTROL_PLAN.map(row => row.key).sort());
});

test('coverage audit detects additions, removals, empty criteria and duplicate case IDs', () => {
  const doc = '| C01 | `save` | dirty | click | saved |\n| C01 | `retired` | idle | click | |';
  const audit = auditGuiCoverage(doc, ['save', 'cancel']);
  assert.deepEqual(audit.missing_actions, ['cancel']);
  assert.deepEqual(audit.unknown_actions, ['retired']);
  assert.deepEqual(audit.duplicate_cases, ['C01']);
  assert.deepEqual(audit.incomplete_cases, ['C01']);
});

test('runbook automation references identify current scenarios', async () => {
  const doc = await readFile(new URL('../MANUAL_GUI_COVERAGE.md', import.meta.url), 'utf8');
  const references = doc.split(/\r?\n/).filter(line => line.startsWith('自動補助:'))
    .flatMap(line => [...line.matchAll(/`([a-z][a-z0-9.-]+)`/g)].map(match => match[1]));
  assert.deepEqual(references.filter(id => !scenarioIds.includes(id)), []);
});

test('Hub integration audit lists the registered connection and shared-work scenarios once', async () => {
  const doc = await readFile(new URL('../HUB_DESKTOP_COVERAGE.md', import.meta.url), 'utf8');
  const listed = [...doc.matchAll(/^\| `([a-z][a-z0-9.-]+)` \|/gm)].map(match => match[1]);
  const current = scenarioIds.filter(id => id.startsWith('hub.')
    || /^settings\.(shared-work(?:-.*)?|project-folder-recovery|device-execution|initial-setup-hub(?:-.*)?)$/.test(id)
    || ['onboarding.win-a-to-win-b', 'project.four-device-acceptance'].includes(id));
  assert.deepEqual(listed.sort(), current.sort());
});
