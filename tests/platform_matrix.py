#!/usr/bin/env python3
from __future__ import annotations

import argparse
import sys
from pathlib import Path

import cli_matrix


def status_from_result(result: cli_matrix.CommandResult, required: tuple[str, ...], blocker_codes: tuple[str, ...]) -> str:
    joined = f"{result.stdout}\n{result.stderr}"
    if result.exit_code == 0 and all(token in joined for token in required):
        return "pass"
    for code in blocker_codes:
        if code in joined:
            first_detail = next((line.strip() for line in joined.splitlines() if code in line), code)
            return f"blocked {first_detail}"
    if result.timed_out:
        return "blocked timeout"
    return f"fail exit={result.exit_code}"


def run_desktop_live(root: Path, cli: Path, target: cli_matrix.Target) -> str:
    result = cli_matrix.run_command([str(cli), "live", "desktop", str(target.path), "--quit-after", "2s"], cwd=root, timeout_s=45.0)
    joined = f"{result.stdout}\n{result.stderr}"
    if result.exit_code == 0 and "event: live.session.ready" in joined and "live.frame.presented" in joined:
        return "pass"
    if "KCL038" in joined:
        headless = cli_matrix.run_command([str(cli), "live", "desktop", str(target.path), "--headless", "--quit-after", "2s"], cwd=root, timeout_s=45.0)
        headless_joined = f"{headless.stdout}\n{headless.stderr}"
        if headless.exit_code == 0 and "event: live.session.ready" in headless_joined and "live.entrypoint.started" in headless_joined:
            return "pass headless"
    return status_from_result(result, ("event: live.session.ready",), ("KCL031", "KCL038", "KCL039"))


def run_macos_live(root: Path, cli: Path, target: cli_matrix.Target) -> str:
    result = cli_matrix.run_command([str(cli), "live", "macos", str(target.path), "--headless", "--quit-after", "2s"], cwd=root, timeout_s=90.0)
    return status_from_result(
        result,
        ("event: live.server.started", "event: live.client.connected", "live.entrypoint.started", "event: live.session.ready"),
        ("KCL031", "KCL038", "KCL039", "KCL046", "KCL072", "KTC070"),
    )


def run_iphone_live(root: Path, cli: Path, target: cli_matrix.Target) -> str:
    result = cli_matrix.run_command(
        [str(cli), "live", "ios", str(target.path), "--host", "0.0.0.0", "--port", "42111", "--quit-after", "1s"],
        cwd=root,
        timeout_s=90.0,
    )
    return status_from_result(
        result,
        ("event: live.ios.physical.detected", "event: live.ios.endpoint.selected", "event: live.session.ready"),
        ("KTC075", "KTC074", "KTC070", "KCL031", "KCL046", "KCL074", "KCL081"),
    )


def run_android_live(root: Path, cli: Path, target: cli_matrix.Target) -> str:
    result = cli_matrix.run_command(
        [str(cli), "live", "android", str(target.path), "--quit-after", "1s"],
        cwd=root,
        timeout_s=60.0,
    )
    return status_from_result(
        result,
        ("event: live.android.tools.detected", "event: live.android.emulator.detected", "event: live.session.ready"),
        ("KTC050", "KCL054", "KCL031"),
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--skip-macos", action="store_true")
    parser.add_argument("--skip-iphone", action="store_true")
    args = parser.parse_args()

    root = cli_matrix.repo_root()
    cli = cli_matrix.cli_binary(root)
    projects = cli_matrix.discover_project_roots(root)
    examples = cli_matrix.discover_examples(projects)

    print("Discovered sibling projects")
    for project in projects:
        print(f"- {project.project}: {project.path}")
    print("\nDiscovered examples")
    for example in examples:
        print(f"- {example.project}: {example.scope}")

    print("\nProject | Example | desktop-fast live | macOS Xcode live | iPhone live | Android live")
    print("--- | --- | --- | --- | --- | ---")
    failures: list[str] = []
    for example in examples:
        desktop = run_desktop_live(root, cli, example)
        macos = "skipped by flag" if args.skip_macos else run_macos_live(root, cli, example)
        iphone = "skipped by flag" if args.skip_iphone else run_iphone_live(root, cli, example)
        android = run_android_live(root, cli, example)
        print(f"{example.project} | {example.scope} | {desktop} | {macos} | {iphone} | {android}")
        for label, status in (("desktop", desktop), ("macos", macos), ("iphone", iphone), ("android", android)):
            if status.startswith("fail"):
                failures.append(f"{example.project}/{example.scope} {label}: {status}")

    if failures:
        print("\nFailures")
        for failure in failures:
            print(f"- {failure}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
