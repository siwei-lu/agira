#!/usr/bin/env node
const { execSync } = require('child_process');
let input;
try {
  input = JSON.parse(require('fs').readFileSync('/dev/stdin', 'utf8'));
} catch {
  process.exit(0);
}
const filePath = input?.tool_input?.file_path || input?.tool_input?.path || '';

if (!filePath || !require('fs').existsSync(filePath)) process.exit(0);

// Only format Rust source files
if (!filePath.endsWith('.rs')) process.exit(0);

try {
  execSync(`rustfmt ${JSON.stringify(filePath)}`, { stdio: 'ignore' });
} catch {
  // Formatter failure is non-blocking
}

process.exit(0);
