#!/usr/bin/env node
/**
 * task-utils.js — Shared utility for reading and writing .orchestrator/tasks.json
 *
 * Can be required as a module or run as a CLI for quick queries.
 *
 * CLI usage:
 *   node task-utils.js status              — Show summary of all tasks
 *   node task-utils.js current             — Show the current active task
 *   node task-utils.js is-done <task-id>   — Exit 0 if done, 1 if not
 *   node task-utils.js all-done            — Exit 0 if all tasks done, 1 if not
 *   node task-utils.js housekeeper-count   — Print housekeeper cycle count
 *   node task-utils.js has-pending         — Exit 0 if pending tasks exist, 1 if not
 *   node task-utils.js awaiting            — Show tasks awaiting enrichment confirmation
 *   node task-utils.js awaiting-checkpoint  — Show tasks awaiting checkpoint approval
 *   node task-utils.js set-state <id> <state> [reason] — Transition a task with history log
 *   node task-utils.js set-field <id> <field.path> <json-value> — Set a nested field on a task
 */

const fs = require('fs');
const path = require('path');

const TASKS_PATH = path.join(process.cwd(), '.orchestrator', 'tasks.json');

function readTasks() {
  try {
    return JSON.parse(fs.readFileSync(TASKS_PATH, 'utf8'));
  } catch {
    return null;
  }
}

function writeTasks(data) {
  fs.writeFileSync(TASKS_PATH, JSON.stringify(data, null, 2));
}

function getTaskById(data, id) {
  return data.tasks?.find(t => t.id === id) || null;
}

function getCurrentTask(data) {
  if (!data?.tasks) return null;
  const activeStates = [
    'enriching', 'architecting', 'in_progress', 'verifying'
  ];
  for (const state of activeStates) {
    const task = data.tasks.find(t => t.state === state);
    if (task) return task;
  }
  return data.tasks.find(t => t.state === 'pending') || null;
}

function getAwaitingConfirmation(data) {
  if (!data?.tasks) return [];
  return data.tasks.filter(
    t => t.state === 'enriching' && t.enrichment?.awaiting_confirmation === true
  );
}

function getAwaitingCheckpoint(data) {
  if (!data?.tasks) return [];
  return data.tasks.filter(
    t => t.enrichment?.needs_checkpoint === true && t.enrichment?.checkpoint?.status === 'pending'
  );
}

function allDone(data) {
  if (!data?.tasks || data.tasks.length === 0) return false;
  return data.tasks.every(t => t.state === 'done' || t.state === 'failed');
}

function hasPending(data) {
  if (!data?.tasks) return false;
  return data.tasks.some(t => t.state !== 'done' && t.state !== 'failed');
}

function setTaskState(data, id, newState, reason) {
  const task = getTaskById(data, id);
  if (!task) return null;
  const oldState = task.state;
  task.state = newState;
  task.history.push({
    from_state: oldState,
    to_state: newState,
    timestamp: new Date().toISOString(),
    reason: reason || `State changed via CLI: ${oldState} → ${newState}`
  });
  writeTasks(data);
  return task;
}

function setTaskField(data, id, fieldPath, value) {
  const task = getTaskById(data, id);
  if (!task) return null;
  const keys = fieldPath.split('.');
  let obj = task;
  for (let i = 0; i < keys.length - 1; i++) {
    if (obj[keys[i]] == null) obj[keys[i]] = {};
    obj = obj[keys[i]];
  }
  obj[keys[keys.length - 1]] = value;
  writeTasks(data);
  return task;
}

function getHousekeeperCount(data) {
  return data?.housekeeper_cycle_count || 0;
}

function summary(data) {
  if (!data?.tasks) return 'No tasks.json found or empty.';
  const counts = {};
  data.tasks.forEach(t => { counts[t.state] = (counts[t.state] || 0) + 1; });
  const lines = [`Total tasks: ${data.tasks.length}`];
  for (const [state, count] of Object.entries(counts)) {
    lines.push(`  ${state}: ${count}`);
  }

  const awaiting = getAwaitingConfirmation(data);
  if (awaiting.length > 0) {
    lines.push(`\n⚠️  ${awaiting.length} task(s) awaiting enrichment confirmation:`);
    awaiting.forEach(t => lines.push(`  - [${t.id}] ${t.title}`));
  }

  const checkpoints = getAwaitingCheckpoint(data);
  if (checkpoints.length > 0) {
    lines.push(`\n📋 ${checkpoints.length} task(s) awaiting checkpoint approval:`);
    checkpoints.forEach(t => lines.push(`  - [${t.id}] ${t.title}: ${t.enrichment.checkpoint.approach}`));
  }

  lines.push(`\nHousekeeper cycles: ${getHousekeeperCount(data)} / ${data.max_housekeeper_cycles || 3}`);
  if (data.requirements?.branch) lines.push(`Branch: ${data.requirements.branch}`);
  return lines.join('\n');
}

// ── CLI Mode ─────────────────────────────────────────────────────

if (require.main === module) {
  const cmd = process.argv[2];
  const data = readTasks();

  switch (cmd) {
    case 'status':
      console.log(data ? summary(data) : 'No tasks.json found.');
      break;

    case 'current': {
      const task = data ? getCurrentTask(data) : null;
      if (task) {
        console.log(JSON.stringify(task, null, 2));
      } else {
        console.log('No active task.');
      }
      break;
    }

    case 'awaiting': {
      const tasks = data ? getAwaitingConfirmation(data) : [];
      if (tasks.length > 0) {
        console.log(JSON.stringify(tasks, null, 2));
      } else {
        console.log('No tasks awaiting enrichment confirmation.');
      }
      break;
    }

    case 'awaiting-checkpoint': {
      const tasks = data ? getAwaitingCheckpoint(data) : [];
      if (tasks.length > 0) {
        tasks.forEach(t => {
          const cp = t.enrichment.checkpoint;
          console.log(`📋 [${t.id}] ${t.title}`);
          console.log(`   Approach:    ${cp.approach}`);
          console.log(`   Touch files: ${cp.touch_files.join(', ')}`);
          console.log(`   Risk:        ${cp.risk || 'none'}`);
          console.log();
        });
      } else {
        console.log('No tasks awaiting checkpoint approval.');
      }
      break;
    }

    case 'is-done': {
      const taskId = process.argv[3];
      if (!taskId) { console.error('Usage: task-utils.js is-done <task-id>'); process.exit(1); }
      const task = data ? getTaskById(data, taskId) : null;
      process.exit(task?.state === 'done' ? 0 : 1);
      break;
    }

    case 'all-done':
      process.exit(data && allDone(data) ? 0 : 1);
      break;

    case 'housekeeper-count':
      console.log(data ? getHousekeeperCount(data) : 0);
      break;

    case 'has-pending':
      process.exit(data && hasPending(data) ? 0 : 1);
      break;

    case 'set-state': {
      const id = process.argv[3];
      const state = process.argv[4];
      const reason = process.argv[5];
      if (!id || !state) { console.error('Usage: task-utils.js set-state <task-id> <state> [reason]'); process.exit(1); }
      if (!data) { console.error('No tasks.json found.'); process.exit(1); }
      const updated = setTaskState(data, id, state, reason);
      if (updated) { console.log(`${id}: ${updated.history.at(-2)?.to_state || 'null'} → ${state}`); }
      else { console.error(`Task ${id} not found.`); process.exit(1); }
      break;
    }

    case 'set-field': {
      const id = process.argv[3];
      const fieldPath = process.argv[4];
      const rawValue = process.argv[5];
      if (!id || !fieldPath || rawValue === undefined) {
        console.error('Usage: task-utils.js set-field <task-id> <field.path> <json-value>');
        process.exit(1);
      }
      if (!data) { console.error('No tasks.json found.'); process.exit(1); }
      let parsed;
      try { parsed = JSON.parse(rawValue); } catch { parsed = rawValue; }
      const result = setTaskField(data, id, fieldPath, parsed);
      if (result) { console.log(`${id}.${fieldPath} = ${JSON.stringify(parsed)}`); }
      else { console.error(`Task ${id} not found.`); process.exit(1); }
      break;
    }

    default:
      console.log('Usage: node task-utils.js <status|current|is-done|all-done|housekeeper-count|has-pending|awaiting|awaiting-checkpoint|set-state|set-field>');
      break;
  }
}

// ── Module Exports ───────────────────────────────────────────────

module.exports = {
  TASKS_PATH,
  readTasks,
  writeTasks,
  getTaskById,
  getCurrentTask,
  getAwaitingConfirmation,
  getAwaitingCheckpoint,
  setTaskState,
  setTaskField,
  allDone,
  hasPending,
  getHousekeeperCount,
  summary
};
