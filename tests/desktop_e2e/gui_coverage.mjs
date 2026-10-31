import { ACTION_IDS } from '../../ui/desktop-web/src/actions.ts';

// Check the public action contract against the runbook's action column. A listed
// action is a planned case, never evidence that its states have been exercised.
export function auditGuiCoverage(markdown, actionIds = ACTION_IDS) {
  const cases = markdown.split(/\r?\n/).filter(line => /^\| [A-Z]\d+ \|/.test(line)).map(line => {
    const columns = line.split('|').slice(1, -1).map(column => column.trim());
    return { id: columns[0], actions: [...columns[1].matchAll(/`([a-z][a-z0-9-]+)`/g)].map(match => match[1]),
      state: columns[2], operation: columns.length === 4 ? columns[2] : columns[3],
      expected: columns.length === 4 ? columns[3] : columns[4] };
  });
  const actual = new Set(cases.flatMap(row => row.actions));
  const expected = new Set(actionIds);
  const seen = new Set(), duplicates = [];
  for (const row of cases) { if (seen.has(row.id)) duplicates.push(row.id); seen.add(row.id); }
  return { action_count: expected.size, case_count: cases.length,
    missing_actions: [...expected].filter(id => !actual.has(id)),
    unknown_actions: [...actual].filter(id => !expected.has(id)),
    duplicate_cases: duplicates,
    incomplete_cases: cases.filter(row => !row.state || !row.operation || !row.expected).map(row => row.id),
    cases };
}
