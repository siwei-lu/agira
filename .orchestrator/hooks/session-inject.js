#!/usr/bin/env node
const fs = require('fs');
const path = require('path');

const TASKS_PATH = '.orchestrator/tasks.json';

const parts = [];

try {
  const data = JSON.parse(fs.readFileSync(TASKS_PATH, 'utf8'));
  if (data?.tasks?.length) {
    const active = data.tasks.find(t =>
      ['enriching', 'architecting', 'in_progress', 'verifying'].includes(t.state)
    );
    const pending = data.tasks.filter(t => t.state === 'pending').length;
    const done = data.tasks.filter(t => t.state === 'done').length;
    const total = data.tasks.length;

    parts.push(`## Active Orchestration Round`);
    parts.push(`Branch: main`);
    parts.push(`Progress: ${done}/${total} tasks done, ${pending} pending`);

    if (active) {
      parts.push(`\nCurrent task: [${active.id}] ${active.title} (${active.state})`);
      if (active.retry_count > 0) {
        parts.push(`⚠️  Retry ${active.retry_count} / ${data.max_retries || 3}`);
        const lastFailure = [...(active.history || [])].reverse()
          .find(h => h.reason && /fail/i.test(h.reason));
        if (lastFailure) parts.push(`Last failure: ${lastFailure.reason}`);
      }
      if (active.enrichment?.acceptance_criteria?.length) {
        parts.push(`Acceptance criteria:`);
        active.enrichment.acceptance_criteria.forEach(c => parts.push(`  - ${c}`));
      }
      if (active.enrichment?.checkpoint?.approach) {
        const cp = active.enrichment.checkpoint;
        parts.push(`\nCheckpoint (${cp.status}): ${cp.approach}`);
        if (cp.touch_files?.length) parts.push(`Files: ${cp.touch_files.join(', ')}`);
        if (cp.risk) parts.push(`Risk: ${cp.risk}`);
        if (cp.status === 'revised' && cp.user_response) parts.push(`Human direction: ${cp.user_response}`);
      }
    }

    const awaitingCheckpoint = data.tasks.filter(t =>
      t.enrichment?.needs_checkpoint && t.enrichment?.checkpoint?.status === 'pending'
      && t !== active
    );
    if (awaitingCheckpoint.length > 0) {
      parts.push(`\n📋 ${awaitingCheckpoint.length} other task(s) awaiting checkpoint approval`);
    }

    try {
      const status = require('child_process').execSync('git status --porcelain', { encoding: 'utf8' }).trim();
      if (status) {
        const changedFiles = status.split('\n').length;
        parts.push(`\n⚠️  Working tree has ${changedFiles} uncommitted file(s) — expected if a task is in progress. Check git status before committing.`);
      }
    } catch { /* not a git repo or git not available */ }

    parts.push(`\nRead .orchestrator/tasks.json for full state.`);
  }
} catch { /* no tasks.json yet */ }

const output = { additionalContext: parts.join('\n') };
console.log(JSON.stringify(output));
