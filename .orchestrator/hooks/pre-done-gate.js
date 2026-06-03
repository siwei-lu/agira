#!/usr/bin/env node
let input;
try {
  input = JSON.parse(require('fs').readFileSync('/dev/stdin', 'utf8'));
} catch {
  process.exit(0);
}
const cmd = input?.tool_input?.command || '';

// Only intercept git commit commands
if (!/^git\s+commit/.test(cmd)) process.exit(0);

const fs = require('fs');
const TASKS_PATH = '.orchestrator/tasks.json';

try {
  const data = JSON.parse(fs.readFileSync(TASKS_PATH, 'utf8'));
  if (!data?.tasks?.length) process.exit(0); // no orchestration active

  // Find tasks in active states that have no verification results
  const suspicious = data.tasks.filter(t => {
    if (['done', 'failed', 'pending', 'dependency_failed'].includes(t.state)) return false;
    return !t.verification_results || t.verification_results.length === 0;
  });

  // Find tasks that jumped to done with too few state transitions
  const jumped = data.tasks.filter(t => {
    if (t.state !== 'done') return false;
    const history = t.history || [];
    // A proper done task: pending→enriching, enriching→architecting, architecting→in_progress,
    // in_progress→verifying, verifying→done = at least 5 transitions
    return history.length < 5;
  });

  const issues = [];
  if (suspicious.length > 0) {
    issues.push(`${suspicious.length} active task(s) have no verification results: ${suspicious.map(t => t.id).join(', ')}`);
  }
  if (jumped.length > 0) {
    issues.push(`${jumped.length} task(s) reached done with suspiciously few state transitions: ${jumped.map(t => t.id).join(', ')}`);
  }

  if (issues.length > 0) {
    console.log(JSON.stringify({
      decision: 'block',
      reason: `Commit blocked — pipeline integrity check failed:\n${issues.join('\n')}\nRun the full pipeline (enriching → architecting → in_progress → verifying) before committing.`
    }));
    process.exit(2);
  }
} catch {
  // No tasks.json or parse error — allow commit
}

process.exit(0);
