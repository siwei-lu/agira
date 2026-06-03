#!/usr/bin/env node
let input;
try {
  input = JSON.parse(require('fs').readFileSync('/dev/stdin', 'utf8'));
} catch {
  process.exit(0);
}
const cmd = input?.tool_input?.command || '';

// Block rm -rf on critical directories
if (/rm\s+-rf\s+(\/|\.|\.\.)\b/.test(cmd)) {
  console.log(JSON.stringify({ decision: 'block', reason: 'Blocked: destructive rm -rf on critical path' }));
  process.exit(2);
}

// Block force push
if (/git\s+push\s+.*--force/.test(cmd)) {
  console.log(JSON.stringify({ decision: 'block', reason: 'Blocked: force push is not allowed during orchestration' }));
  process.exit(2);
}

process.exit(0);
