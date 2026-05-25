#!/usr/bin/env python3
from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable


IGNORED_DIRS = {
    ".git",
    ".zig-cache",
    "zig-out",
    ".kira",
    "node_modules",
    "vendor",
    "toolchains",
    "cache",
    "build",
    "dist",
    "target",
}
MANIFEST_NAMES = ("kira.toml", "project.toml", "Kira.toml")
FORBIDDEN_OUTPUT = (
    "panic",
    "thread ",
    "unreachable",
    "index out of bounds",
    "attempt to use null",
    "segmentation fault",
    "stack trace",
    "internal compiler error",
    "KIC001",
    "KICE001",
)
FORBIDDEN_LIVE_OUTPUT = (
    "KCL028",
    "KCL029",
    "no build.zig file found",
    "initialize build.zig template file",
)
DESKTOP_LIVE_READY_EVENTS = (
    "event: live.bundle.compiled",
    "event: live.server.started",
    "event: live.runner.resolved",
    "event: live.runner.launched",
    "event: live.client.connected",
    "event: live.bundle.graph.sent",
    "live.bundle.graph.received",
    "live.client.bundle.received",
    "live.bundle.loaded",
    "live.bundle.linked",
    "live.entrypoint.started",
    "live.frame.presented",
    "event: live.session.ready",
    "event: live.session.ended reason=quit-after",
)
HOT_RESTART_EVENTS = (
    "event: live.runner.headless",
    "event: live.session.ready",
    "event: live.source.changed",
    "event: live.rebuild.started",
    "event: live.rebuild.finished",
    "event: live.bundle.rebuilt mode=full-bundle",
    "event: live.bundle.sent mode=full-bundle",
    "live.client.hot_restart.started",
    "live.entrypoint.restarted",
    "live.hot_restart.finished",
    "event: live.shutdown.ack client=desktop",
    "event: live.session.ended reason=quit-after",
)


@dataclass
class CommandResult:
    exit_code: int | None
    stdout: str
    stderr: str
    timed_out: bool


@dataclass
class Target:
    project: str
    path: Path
    kind: str
    scope: str


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def cli_binary(root: Path) -> Path:
    candidates = (
        root / "zig-out" / "bin" / "kira",
        root / "zig-out" / "bin" / "kirac",
    )
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    return candidates[0]


def read_manifest_kind(path: Path) -> str:
    text = path.read_text(encoding="utf-8")
    match = re.search(r'^\s*kind\s*=\s*"([^"]+)"', text, re.MULTILINE)
    return match.group(1) if match else "app"


def discover_project_roots(root: Path) -> list[Target]:
    parent = root.parent
    results: list[Target] = []
    seen: set[Path] = set()
    for current_root, dirs, files in os.walk(parent):
        current = Path(current_root)
        dirs[:] = [name for name in dirs if name not in IGNORED_DIRS]
        if current == root:
            dirs[:] = [name for name in dirs if name != "examples"]
            continue
        manifest_name = next((name for name in MANIFEST_NAMES if name in files), None)
        if manifest_name is None:
            continue
        if current in seen:
            continue
        if current.parent != parent:
            continue
        kind = read_manifest_kind(current / manifest_name)
        results.append(
            Target(
                project=current.name,
                path=current,
                kind="library" if kind == "library" else "executable",
                scope="root",
            )
        )
        seen.add(current)
    results.sort(key=lambda item: item.project.lower())
    return results


def discover_examples(projects: Iterable[Target]) -> list[Target]:
    examples: list[Target] = []
    seen: set[str] = set()
    for project in projects:
        for folder_name in ("examples", "Examples"):
            examples_root = project.path / folder_name
            if not examples_root.is_dir():
                continue
            for current_root, dirs, files in os.walk(examples_root):
                current = Path(current_root)
                dirs[:] = [name for name in dirs if name not in IGNORED_DIRS]
                manifest_name = next((name for name in MANIFEST_NAMES if name in files), None)
                if manifest_name is None:
                    continue
                real_key = os.path.realpath(current).lower()
                if real_key in seen:
                    continue
                seen.add(real_key)
                examples.append(
                    Target(
                        project=project.project,
                        path=current,
                        kind="example",
                        scope=current.relative_to(project.path).as_posix(),
                    )
                )
    examples.sort(key=lambda item: (item.project.lower(), item.scope.lower()))
    return examples


def run_command(argv: list[str], cwd: Path, timeout_s: float) -> CommandResult:
    proc = subprocess.Popen(
        argv,
        cwd=str(cwd),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        stdout, stderr = proc.communicate(timeout=timeout_s)
        return CommandResult(proc.returncode, stdout, stderr, False)
    except subprocess.TimeoutExpired:
        proc.kill()
        stdout, stderr = proc.communicate()
        return CommandResult(None, stdout, stderr, True)


def assert_clean_failure(result: CommandResult, expected_code: str) -> str | None:
    joined = f"{result.stdout}\n{result.stderr}"
    if expected_code not in joined:
        return f"missing expected diagnostic {expected_code}"
    for token in FORBIDDEN_OUTPUT:
        if token in joined:
            return f"forbidden output token `{token}` present"
    return None


def assert_clean_success(result: CommandResult, allow_timeout: bool) -> str | None:
    joined = f"{result.stdout}\n{result.stderr}"
    for token in FORBIDDEN_OUTPUT:
        if token in joined:
            return f"forbidden output token `{token}` present"
    if result.timed_out and allow_timeout:
        return None
    if result.timed_out:
        return "command timed out"
    if result.exit_code != 0:
        return f"exit code {result.exit_code}"
    return None


def assert_no_forbidden_live_output(result: CommandResult) -> str | None:
    joined = f"{result.stdout}\n{result.stderr}"
    for token in FORBIDDEN_LIVE_OUTPUT:
        if token in joined:
            return f"forbidden live output token `{token}` present"
    return None


def assert_events(result: CommandResult, required: Iterable[str]) -> str | None:
    joined = f"{result.stdout}\n{result.stderr}"
    missing = [event for event in required if event not in joined]
    if missing:
        return "missing live event(s): " + ", ".join(missing)
    return None


def assert_desktop_live_contract(result: CommandResult, target: Path) -> str | None:
    failure = assert_clean_success(result, allow_timeout=False)
    if failure is not None:
        return failure
    failure = assert_no_forbidden_live_output(result)
    if failure is not None:
        return failure
    failure = assert_events(result, DESKTOP_LIVE_READY_EVENTS)
    if failure is not None:
        return failure
    joined = f"{result.stdout}\n{result.stderr}"
    expected_root = target / ".kira-build" / "live"
    if f"runtime_cwd={target}" not in joined:
        return f"missing runtime cwd event for {target}"
    if f"output_root={expected_root}" not in joined:
        return f"missing live output root event for {expected_root}"
    if not expected_root.is_dir():
        return f"live output root was not created: {expected_root}"
    if "KIRA_APP_RENDERED_VISIBLE_CONTENT" not in joined:
        return "missing visible render sentinel KIRA_APP_RENDERED_VISIBLE_CONTENT"
    return None


def assert_hot_restart_contract(result: CommandResult) -> str | None:
    failure = assert_clean_success(result, allow_timeout=False)
    if failure is not None:
        return failure
    failure = assert_no_forbidden_live_output(result)
    if failure is not None:
        return failure
    failure = assert_events(result, HOT_RESTART_EVENTS)
    if failure is not None:
        return failure
    joined = f"{result.stdout}\n{result.stderr}"
    if "version 1" not in joined or "version 2" not in joined:
        return "hot restart did not expose both version 1 and version 2 output"
    runner_pids = re.findall(r"live\.runner\.pid=(\d+)", joined)
    launched_pids = re.findall(r"event: live\.runner\.launched pid=(\d+)", joined)
    if len(runner_pids) != 1:
        return f"expected one runner process identity, saw {len(runner_pids)}"
    if len(launched_pids) != 1:
        return f"expected one runner launch event, saw {len(launched_pids)}"
    if runner_pids[0] != launched_pids[0]:
        return f"runner pid mismatch: launched {launched_pids[0]}, client {runner_pids[0]}"
    return None


def format_status(result: CommandResult, allow_timeout: bool, expected_code: str | None, accepted_codes: tuple[str, ...] = ()) -> str:
    if expected_code is not None:
        return f"expected {expected_code}" if expected_code in (result.stdout + result.stderr) else "unexpected failure"
    for code in accepted_codes:
        if code in (result.stdout + result.stderr):
            return f"expected {code}"
    if result.timed_out and allow_timeout:
        return "smoke-timeout"
    if result.exit_code == 0:
        return "pass"
    return f"fail({result.exit_code})"


def run_matrix() -> int:
    root = repo_root()
    cli = cli_binary(root)
    if not cli.is_file():
        print(f"missing CLI binary: {cli}", file=sys.stderr)
        return 1

    projects = discover_project_roots(root)
    examples = discover_examples(projects)
    failures: list[str] = []

    print("Project | Target | Kind | check | build | run | live")
    print("--- | --- | --- | --- | --- | --- | ---")

    def execute_cell(target: Target, command: str) -> str:
        argv = [str(cli)]
        timeout_s = 120.0
        expected_code: str | None = None
        accepted_codes: tuple[str, ...] = ()
        allow_timeout = False

        if command == "check":
            argv += ["check", str(target.path)]
        elif command == "build":
            argv += ["build", str(target.path)]
        elif command == "run":
            argv += ["run", str(target.path)]
            if target.kind == "example":
                argv += ["--quit-after", "5s"]
            timeout_s = 20.0
            if target.kind == "library":
                expected_code = "KCL020"
                allow_timeout = False
        elif command == "live":
            argv += ["live", str(target.path)]
            if target.kind == "example":
                argv += ["--quit-after", "5s"]
                accepted_codes = ("KCL031", "KCL038")
            timeout_s = 60.0
            allow_timeout = False
            if target.kind == "library":
                expected_code = "KCL021"
        else:
            raise ValueError(command)

        result = run_command(argv, cwd=root, timeout_s=timeout_s)
        status = format_status(result, allow_timeout=allow_timeout, expected_code=expected_code, accepted_codes=accepted_codes)

        if expected_code is not None:
            failure = assert_clean_failure(result, expected_code)
        elif any(code in (result.stdout + result.stderr) for code in accepted_codes):
            failure = assert_clean_failure(result, next(code for code in accepted_codes if code in (result.stdout + result.stderr)))
            if failure is None and command == "live":
                failure = assert_no_forbidden_live_output(result)
        else:
            failure = assert_clean_success(result, allow_timeout=allow_timeout)
            if failure is None and command == "live":
                failure = assert_no_forbidden_live_output(result)
            if failure is None and command == "live" and target.kind == "example":
                failure = assert_events(result, (
                    "event: live.server.started",
                    "event: live.client.connected",
                    "live.bundle.graph.received",
                    "live.bundle.loaded",
                    "live.bundle.linked",
                    "live.entrypoint.started",
                    "live.frame.presented",
                    "event: live.session.ready",
                    "event: live.session.ended reason=quit-after",
                ))
        if failure is not None:
            failures.append(
                f"{target.project} {target.scope} {command}: {failure}\n"
                f"stdout:\n{result.stdout}\n"
                f"stderr:\n{result.stderr}\n"
            )
        return status

    for target in [*projects, *examples]:
        row = [
            target.project,
            "." if target.scope == "root" else target.scope,
            target.kind,
            execute_cell(target, "check"),
            execute_cell(target, "build"),
            execute_cell(target, "run"),
            execute_cell(target, "live"),
        ]
        print(" | ".join(row))

    failures.extend(run_live_contract_suite(root, cli))

    print("")
    print(f"Discovered project roots: {len(projects)}")
    for target in projects:
        print(f"- {target.project}: {target.path}")
    print(f"Discovered examples: {len(examples)}")
    for target in examples:
        print(f"- {target.project}: {target.scope}")

    if failures:
        print("")
        print("Failures:")
        for failure in failures:
            print(failure)
        return 1
    return 0


def run_live_contract_suite(root: Path, cli: Path) -> list[str]:
    failures: list[str] = []
    live_example = root.parent / "ui-foundation" / "Examples" / "basic-foundation-app"
    print("")
    print("Live contract checks")
    print("---")
    if not live_example.is_dir():
        failures.append(f"required live example is missing: {live_example}")
        print(f"missing required live example: {live_example}")
        return failures

    live_cases = (
        (
            "root desktop quit-after",
            [str(cli), "live", "desktop", str(live_example), "--quit-after", "5s"],
            root,
        ),
        (
            "example shorthand quit-after",
            [str(cli), "live", ".", "--quit-after", "5s"],
            live_example,
        ),
        (
            "example desktop legacy run-for",
            [str(cli), "live", "desktop", ".", "--run-for", "5s", "--kill-after"],
            live_example,
        ),
    )
    for name, argv, cwd in live_cases:
        result = run_command(argv, cwd=cwd, timeout_s=45.0)
        failure = assert_desktop_live_contract(result, live_example)
        status = "pass" if failure is None else "fail"
        print(f"{name}: {status}")
        if failure is not None:
            failures.append(
                f"{name}: {failure}\n"
                f"argv: {' '.join(argv)}\n"
                f"cwd: {cwd}\n"
                f"stdout:\n{result.stdout}\n"
                f"stderr:\n{result.stderr}\n"
            )

    invalid_platform = run_command([str(cli), "live", "not-a-platform", str(live_example)], cwd=root, timeout_s=10.0)
    platform_failure = assert_clean_failure(invalid_platform, "KCL041")
    print(f"invalid platform diagnostic: {'pass' if platform_failure is None else 'fail'}")
    if platform_failure is not None:
        failures.append(
            f"invalid platform diagnostic: {platform_failure}\n"
            f"stdout:\n{invalid_platform.stdout}\n"
            f"stderr:\n{invalid_platform.stderr}\n"
        )

    invalid_duration = run_command([str(cli), "live", "desktop", str(live_example), "--quit-after", "later"], cwd=root, timeout_s=10.0)
    joined_duration = f"{invalid_duration.stdout}\n{invalid_duration.stderr}"
    duration_failure = assert_clean_success(invalid_duration, allow_timeout=False)
    if invalid_duration.exit_code != 0 and "invalid duration" in joined_duration.lower():
        duration_failure = None
    print(f"invalid duration diagnostic: {'pass' if duration_failure is None else 'fail'}")
    if duration_failure is not None:
        failures.append(
            f"invalid duration diagnostic: {duration_failure}\n"
            f"stdout:\n{invalid_duration.stdout}\n"
            f"stderr:\n{invalid_duration.stderr}\n"
        )

    hot_restart_failure = run_hot_restart_test(root, cli)
    print(f"hot restart full-bundle reload: {'pass' if hot_restart_failure is None else 'fail'}")
    if hot_restart_failure is not None:
        failures.append(hot_restart_failure)

    ios_failure = run_ios_audit_test(root, cli, live_example)
    print(f"iOS live audit: {'pass' if ios_failure is None else 'fail'}")
    if ios_failure is not None:
        failures.append(ios_failure)

    return failures


def run_hot_restart_test(root: Path, cli: Path) -> str | None:
    tmp = Path(tempfile.mkdtemp(prefix="kira-live-reload-"))
    try:
        (tmp / "app").mkdir()
        (tmp / "kira.toml").write_text(
            "\n".join(
                (
                    "[package]",
                    'name = "live-reload-smoke"',
                    'version = "0.1.0"',
                    'kind = "app"',
                    'kira = "0.1.0"',
                    "",
                    "[defaults]",
                    'execution_mode = "hybrid"',
                    'build_target = "host"',
                    "",
                )
            ),
            encoding="utf-8",
        )
        entrypoint = tmp / "app" / "main.kira"
        entrypoint.write_text(
            '@Main\nfunction main() {\n    print("version 1");\n    return;\n}\n',
            encoding="utf-8",
        )
        proc = subprocess.Popen(
            [str(cli), "live", "desktop", str(tmp), "--headless", "--quit-after", "6s"],
            cwd=str(root),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        time.sleep(2.5)
        entrypoint.write_text(
            '@Main\nfunction main() {\n    print("version 2");\n    return;\n}\n',
            encoding="utf-8",
        )
        try:
            stdout, stderr = proc.communicate(timeout=20.0)
        except subprocess.TimeoutExpired:
            proc.kill()
            stdout, stderr = proc.communicate()
            return (
                "hot restart full-bundle reload: command timed out\n"
                f"stdout:\n{stdout}\n"
                f"stderr:\n{stderr}\n"
            )
        result = CommandResult(proc.returncode, stdout, stderr, False)
        failure = assert_hot_restart_contract(result)
        if failure is None:
            return None
        return (
            f"hot restart full-bundle reload: {failure}\n"
            f"stdout:\n{stdout}\n"
            f"stderr:\n{stderr}\n"
        )
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def run_ios_audit_test(root: Path, cli: Path, live_example: Path) -> str | None:
    result = run_command([str(cli), "live", "ios-simulator", str(live_example), "--quit-after", "1s"], cwd=root, timeout_s=30.0)
    joined = f"{result.stdout}\n{result.stderr}"
    failure = assert_no_forbidden_live_output(result)
    if failure is not None:
        return (
            f"iOS live audit: {failure}\n"
            f"stdout:\n{result.stdout}\n"
            f"stderr:\n{result.stderr}\n"
        )
    if "KCL046" in joined:
        required = ("event: live.ios.tools.detected", "event: live.ios.simulator.detected")
        failure = assert_events(result, required)
        if failure is None and result.exit_code == 0:
            failure = "iOS unsupported diagnostic exited successfully"
    elif "KTC020" in joined or "KTC021" in joined or "KTC022" in joined:
        failure = None if result.exit_code != 0 else "missing iOS toolchain diagnostic failure exit"
    elif result.exit_code == 0:
        failure = assert_events(result, (
            "event: live.server.started",
            "event: live.client.connected",
            "live.bundle.graph.received",
            "live.entrypoint.started",
        ))
    else:
        failure = "missing precise iOS live diagnostic or successful simulator handshake"
    if failure is None:
        return None
    return (
        f"iOS live audit: {failure}\n"
        f"stdout:\n{result.stdout}\n"
        f"stderr:\n{result.stderr}\n"
    )


if __name__ == "__main__":
    sys.exit(run_matrix())
