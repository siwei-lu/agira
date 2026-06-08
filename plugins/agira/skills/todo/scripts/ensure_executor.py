#!/usr/bin/env python3
"""Ensure one tmux-backed Claude executor is running for an Agira project."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from typing import Any


SKILL_DIR = Path(__file__).resolve().parents[1]
EXECUTOR_PROMPT_PATH = SKILL_DIR / "executor_prompt.md"

SHELL_COMMANDS = {
    "bash",
    "csh",
    "dash",
    "fish",
    "ksh",
    "nu",
    "sh",
    "tcsh",
    "zsh",
}

RUNNING_MARKERS = (
    "esc to interrupt",
    "ctrl+c to interrupt",
    "ctrl-c to interrupt",
    "interrupt to stop",
    "running in the background",
    "running tool",
    "running command",
    "pollinating",
    "waiting for task",
    "waiting for completion notification",
    "等待完成通知",
    "完成通知中",
)

STOPPED_OR_WAITING_MARKERS = (
    "api error",
    "overloaded",
    "rate limit",
    "try again in a moment",
    "waiting for input",
)

CLAUDE_READY_TIMEOUT_SECONDS = 20.0
CLAUDE_READY_POLL_SECONDS = 0.25
REMOTE_CONTROL_READY_TIMEOUT_SECONDS = 20.0
CLAUDE_PASTE_SETTLE_SECONDS = 0.10


class ExecutorError(RuntimeError):
    """Expected user-facing failure."""


def load_executor_prompt() -> str:
    try:
        prompt = EXECUTOR_PROMPT_PATH.read_text(encoding="utf-8")
    except OSError as exc:
        raise ExecutorError(f"failed to read executor prompt: {EXECUTOR_PROMPT_PATH}") from exc

    prompt = prompt.rstrip()
    if not prompt:
        raise ExecutorError(f"executor prompt is empty: {EXECUTOR_PROMPT_PATH}")
    if not prompt.startswith("/goal "):
        raise ExecutorError(f"executor prompt must start with /goal: {EXECUTOR_PROMPT_PATH}")
    return prompt


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Launch or reuse a tmux-backed Claude executor for Agira tasks.",
    )
    parser.add_argument(
        "request",
        nargs="?",
        help='Request in the form "<project-slug-or-name> at <path-to-project>".',
    )
    parser.add_argument("--project-name", help="Project slug/name. Overrides request parsing.")
    parser.add_argument("--project-path", help="Project directory. Overrides request parsing.")
    parser.add_argument("--tmux-bin", default="tmux", help="tmux executable name or path.")
    parser.add_argument("--claude-bin", default="claude", help="Claude executable name or path.")
    parser.add_argument("--model", default="sonnet", help="Claude model to pass to --model.")
    parser.add_argument("--pane-lines", type=int, default=200, help="Recent tmux lines to inspect.")
    parser.add_argument("--dry-run", action="store_true", help="Print intended action without changing tmux.")
    parser.add_argument("--status-only", action="store_true", help="Inspect only; do not launch or restart.")
    parser.add_argument("--json", action="store_true", help="Emit JSON output.")
    return parser.parse_args()


def emit(payload: dict[str, Any], as_json: bool) -> None:
    if as_json:
        print(json.dumps(payload, indent=2, sort_keys=True))
        return

    print(f"status: {payload['status']}")
    print(f"session: {payload.get('session_name')}")
    print(f"project: {payload.get('project_path')}")
    if payload.get("action"):
        print(f"action: {payload['action']}")
    if payload.get("pane_current_command"):
        print(f"pane_current_command: {payload['pane_current_command']}")
    if payload.get("message"):
        print(payload["message"])


def fail(message: str, as_json: bool, **extra: Any) -> None:
    payload = {"status": "error", "message": message, **extra}
    emit(payload, as_json)
    raise SystemExit(1)


def parse_request(args: argparse.Namespace) -> tuple[str, Path]:
    if args.project_name or args.project_path:
        if not args.project_name:
            raise ExecutorError("--project-name is required when using --project-path")
        if not args.project_path:
            raise ExecutorError("--project-path is required when using --project-name")
        raw_name = args.project_name.strip()
        raw_project = args.project_path.strip()
    else:
        request = (args.request or "").strip()
        if not request:
            raise ExecutorError("missing executor request argument")
        if " at " not in request:
            raise ExecutorError('expected "<project-slug-or-name> at <path-to-project>"')
        raw_name, raw_project = request.split(" at ", 1)
        raw_name = raw_name.strip()
        raw_project = raw_project.strip()

    if not raw_name:
        raise ExecutorError("project slug/name is empty")
    if not raw_project:
        raise ExecutorError("project path is empty")

    session_name = raw_name if raw_name.startswith("agira-executor-") else f"agira-executor-{raw_name}"
    project = Path(raw_project).expanduser().resolve()

    if not project.exists():
        raise ExecutorError(f"project path does not exist: {project}")
    if not project.is_dir():
        raise ExecutorError(f"project path is not a directory: {project}")

    return session_name, project


def require_executable(binary: str, label: str) -> str:
    resolved = shutil.which(binary)
    if resolved:
        return resolved
    if Path(binary).exists():
        return binary
    raise ExecutorError(f"{label} is not installed or not on PATH: {binary}")


def run_tmux(tmux_bin: str, argv: list[str], cwd: Path, check: bool = False) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(
        [tmux_bin, *argv],
        cwd=cwd,
        text=True,
        capture_output=True,
        check=False,
    )
    if check and completed.returncode != 0:
        command = " ".join(shlex.quote(part) for part in [tmux_bin, *argv])
        stderr = completed.stderr.strip()
        raise ExecutorError(f"tmux command failed: {command}" + (f"\n{stderr}" if stderr else ""))
    return completed


def tmux_session_target(name: str) -> str:
    return f"={name}"


def tmux_pane_target(name: str) -> str:
    return f"={name}:"


def remote_control_prompt(name: str) -> str:
    return f"/remote-control {name}"


def has_session(tmux_bin: str, name: str, project: Path) -> bool:
    completed = run_tmux(tmux_bin, ["has-session", "-t", tmux_session_target(name)], project)
    if completed.returncode == 0:
        return True
    stderr = completed.stderr.lower()
    if "can't find session" in stderr or "no current client" in stderr or completed.returncode == 1:
        return False
    raise ExecutorError(completed.stderr.strip() or f"tmux has-session failed with {completed.returncode}")


def capture_pane(tmux_bin: str, name: str, project: Path, lines: int) -> str:
    completed = run_tmux(tmux_bin, ["capture-pane", "-pt", tmux_pane_target(name), "-S", f"-{lines}"], project, check=True)
    return completed.stdout


def current_command(tmux_bin: str, name: str, project: Path) -> str:
    completed = run_tmux(
        tmux_bin,
        ["display-message", "-p", "-t", tmux_pane_target(name), "#{pane_current_command}"],
        project,
        check=True,
    )
    return completed.stdout.strip()


def command_name(command: str) -> str:
    return Path(command.strip()).name.lower()


def is_claude_command(command: str) -> bool:
    name = command_name(command)
    return name == "claude" or name.startswith("claude-")


def pane_looks_like_claude(pane: str) -> bool:
    recent_lines = "\n".join(pane.splitlines()[-60:]).lower()
    has_status_bar = "model:" in recent_lines and "session:" in recent_lines
    has_claude_ui_marker = (
        "/goal active" in recent_lines
        or "auto mode on" in recent_lines
        or "for agents" in recent_lines
        or "remote control active" in recent_lines
    )
    return has_status_bar and has_claude_ui_marker


def is_claude_pane(command: str, pane: str) -> bool:
    return is_claude_command(command) or pane_looks_like_claude(pane)


def is_shell_command(command: str) -> bool:
    return command_name(command) in SHELL_COMMANDS


def last_marker_index(text: str, markers: tuple[str, ...]) -> int:
    return max((text.rfind(marker) for marker in markers), default=-1)


def workflow_looks_running(pane: str, command: str) -> bool:
    text = pane.lower()
    recent_lines = "\n".join(pane.splitlines()[-40:]).lower()
    has_goal = (
        "/goal shell agira task todo" in text
        or "/goal orchestrate agira todo execution" in text
        or "agira task todo" in text
        or "you are the orchestrator of the workflow" in text
    )
    latest_running_marker = last_marker_index(recent_lines, RUNNING_MARKERS)
    latest_stopped_or_waiting_marker = last_marker_index(recent_lines, STOPPED_OR_WAITING_MARKERS)
    return (
        is_claude_pane(command, pane)
        and has_goal
        and latest_running_marker >= 0
        and latest_running_marker > latest_stopped_or_waiting_marker
    )


def claude_command(claude_bin: str, model: str) -> str:
    return shlex.join([claude_bin, "--model", model])


def wait_for_claude_pane(tmux_bin: str, name: str, project: Path, lines: int) -> None:
    deadline = time.monotonic() + CLAUDE_READY_TIMEOUT_SECONDS
    last_command = ""
    last_tail = ""

    while time.monotonic() < deadline:
        pane = capture_pane(tmux_bin, name, project, lines)
        command = current_command(tmux_bin, name, project)
        if is_claude_pane(command, pane):
            return
        last_command = command
        last_tail = "\n".join(pane.splitlines()[-10:])
        time.sleep(CLAUDE_READY_POLL_SECONDS)

    raise ExecutorError(
        "launched Claude but the tmux pane did not become ready before sending the executor sequence"
        + (f"; pane_current_command={last_command!r}" if last_command else "")
        + (f"\npane_tail:\n{last_tail}" if last_tail else "")
    )


def remote_control_active(pane: str) -> bool:
    return "remote control active" in "\n".join(pane.splitlines()[-80:]).lower()


def wait_for_remote_control_ready(tmux_bin: str, name: str, project: Path, lines: int) -> None:
    deadline = time.monotonic() + REMOTE_CONTROL_READY_TIMEOUT_SECONDS
    last_tail = ""

    while time.monotonic() < deadline:
        pane = capture_pane(tmux_bin, name, project, lines)
        if remote_control_active(pane):
            return
        last_tail = "\n".join(pane.splitlines()[-10:])
        time.sleep(CLAUDE_READY_POLL_SECONDS)

    raise ExecutorError(
        "remote control did not become ready before sending the executor prompt"
        + (f"\npane_tail:\n{last_tail}" if last_tail else "")
    )


def send_claude_message(tmux_bin: str, name: str, project: Path, message: str) -> None:
    target = tmux_pane_target(name)
    buffer_name = f"agira-executor-prompt-{os.getpid()}-{time.monotonic_ns()}"
    temp_path: Path | None = None

    try:
        with tempfile.NamedTemporaryFile("w", encoding="utf-8", delete=False) as handle:
            handle.write(message)
            temp_path = Path(handle.name)

        run_tmux(tmux_bin, ["load-buffer", "-b", buffer_name, str(temp_path)], project, check=True)
        run_tmux(tmux_bin, ["paste-buffer", "-p", "-r", "-b", buffer_name, "-t", target], project, check=True)
        time.sleep(CLAUDE_PASTE_SETTLE_SECONDS)
        run_tmux(tmux_bin, ["send-keys", "-t", target, "Enter"], project, check=True)
    finally:
        run_tmux(tmux_bin, ["delete-buffer", "-b", buffer_name], project)
        if temp_path is not None:
            temp_path.unlink(missing_ok=True)


def send_executor_sequence_to_claude(
    tmux_bin: str,
    name: str,
    project: Path,
    pane_lines: int,
    executor_prompt: str,
    dry_run: bool,
) -> str:
    if dry_run:
        return "would_send_named_remote_control_and_executor_prompt_to_existing_claude"
    send_claude_message(tmux_bin, name, project, remote_control_prompt(name))
    wait_for_remote_control_ready(tmux_bin, name, project, pane_lines)
    send_claude_message(tmux_bin, name, project, executor_prompt)
    return "sent_named_remote_control_and_executor_prompt_to_existing_claude"


def launch_claude_in_existing_shell(
    tmux_bin: str,
    claude_bin: str,
    model: str,
    name: str,
    project: Path,
    pane_lines: int,
    executor_prompt: str,
    dry_run: bool,
) -> str:
    command = claude_command(claude_bin, model)
    if dry_run:
        return "would_launch_claude_and_send_named_remote_control_executor_sequence_in_existing_shell"
    run_tmux(tmux_bin, ["send-keys", "-t", name, command, "Enter"], project, check=True)
    wait_for_claude_pane(tmux_bin, name, project, pane_lines)
    send_executor_sequence_to_claude(tmux_bin, name, project, pane_lines, executor_prompt, dry_run=False)
    return "launched_claude_and_sent_named_remote_control_executor_sequence_in_existing_shell"


def start_new_session(
    tmux_bin: str,
    claude_bin: str,
    model: str,
    name: str,
    project: Path,
    pane_lines: int,
    executor_prompt: str,
    dry_run: bool,
) -> str:
    command = claude_command(claude_bin, model)
    if dry_run:
        return "would_start_new_tmux_session_and_send_named_remote_control_executor_sequence"
    run_tmux(tmux_bin, ["new-session", "-d", "-s", name, "-c", str(project), command], project, check=True)
    wait_for_claude_pane(tmux_bin, name, project, pane_lines)
    send_executor_sequence_to_claude(tmux_bin, name, project, pane_lines, executor_prompt, dry_run=False)
    return "started_new_tmux_session_and_sent_named_remote_control_executor_sequence"


def ensure_executor(args: argparse.Namespace) -> dict[str, Any]:
    session_name, project = parse_request(args)
    executor_prompt = load_executor_prompt()
    tmux_bin = require_executable(args.tmux_bin, "tmux")
    claude_bin = args.claude_bin
    if not args.status_only and not args.dry_run:
        claude_bin = require_executable(args.claude_bin, "claude")

    base: dict[str, Any] = {
        "session_name": session_name,
        "project_path": str(project),
        "model": args.model,
        "remote_control_prompt": remote_control_prompt(session_name),
        "executor_prompt_path": str(EXECUTOR_PROMPT_PATH),
        "executor_prompt": executor_prompt,
    }

    if args.dry_run:
        base["dry_run"] = True

    session_exists = has_session(tmux_bin, session_name, project)
    base["session_exists"] = session_exists

    if not session_exists:
        if args.status_only:
            return {**base, "status": "absent", "action": "none"}
        action = start_new_session(
            tmux_bin,
            claude_bin,
            args.model,
            session_name,
            project,
            args.pane_lines,
            executor_prompt,
            args.dry_run,
        )
        return {**base, "status": "started" if not args.dry_run else "dry_run", "action": action}

    pane = capture_pane(tmux_bin, session_name, project, args.pane_lines)
    command = current_command(tmux_bin, session_name, project)
    claude_pane = is_claude_pane(command, pane)
    running = workflow_looks_running(pane, command)
    base.update(
        {
            "pane_current_command": command,
            "pane_looks_like_claude": claude_pane,
            "workflow_looks_running": running,
            "pane_tail": "\n".join(pane.splitlines()[-20:]),
        }
    )

    if running:
        return {**base, "status": "running", "action": "none"}
    if args.status_only:
        return {**base, "status": "idle_or_unclear", "action": "none"}

    if claude_pane:
        action = send_executor_sequence_to_claude(
            tmux_bin,
            session_name,
            project,
            args.pane_lines,
            executor_prompt,
            args.dry_run,
        )
        return {**base, "status": "restarted" if not args.dry_run else "dry_run", "action": action}

    if is_shell_command(command):
        action = launch_claude_in_existing_shell(
            tmux_bin,
            claude_bin,
            args.model,
            session_name,
            project,
            args.pane_lines,
            executor_prompt,
            args.dry_run,
        )
        return {**base, "status": "restarted" if not args.dry_run else "dry_run", "action": action}

    raise ExecutorError(
        f"existing session pane is running non-Claude command {command!r}; not sending keys into a busy pane"
    )


def main() -> int:
    args = parse_args()
    try:
        payload = ensure_executor(args)
    except ExecutorError as exc:
        fail(str(exc), args.json)
    emit(payload, args.json)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
