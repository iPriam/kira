#!/usr/bin/env python3
from __future__ import annotations

import os
import re
import subprocess
import sys
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
                accepted_codes = ("KCL031",)
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
        else:
            failure = assert_clean_success(result, allow_timeout=allow_timeout)
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


if __name__ == "__main__":
    sys.exit(run_matrix())
