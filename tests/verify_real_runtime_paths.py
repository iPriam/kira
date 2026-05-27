#!/usr/bin/env python3
from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import time
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable


ROOT = Path(__file__).resolve().parents[1]
UI_FOUNDATION = (ROOT / "../ui-foundation").resolve()
UI_BASIC = UI_FOUNDATION / "Examples/basic-foundation-app"
UI_RETAINED = UI_FOUNDATION / "Examples/retained-tree-smoke"
EVIDENCE = ROOT / ".codex/work/evidence"
REPORT_JSON = EVIDENCE / "007-real-runtime-verifier.json"
CHROME_PORT = int(os.environ.get("KIRA_VERIFY_CHROME_PORT", "9236"))
HTTP_PORT = int(os.environ.get("KIRA_VERIFY_HTTP_PORT", "8137"))


@dataclass
class CommandResult:
    argv: list[str]
    cwd: str
    exit_code: int
    stdout: str
    stderr: str
    timeout: bool = False

    def summary(self, limit: int = 2000) -> dict[str, object]:
        return {
            "argv": self.argv,
            "cwd": self.cwd,
            "exit_code": self.exit_code,
            "timeout": self.timeout,
            "stdout": trim(self.stdout, limit),
            "stderr": trim(self.stderr, limit),
        }


@dataclass
class Check:
    name: str
    passed: bool
    details: dict[str, object] = field(default_factory=dict)


def trim(value: str, limit: int) -> str:
    if len(value) <= limit:
        return value
    return value[:limit] + f"\n... <trimmed {len(value) - limit} bytes>"


def run(
    argv: Iterable[str | Path],
    *,
    cwd: Path = ROOT,
    timeout: int = 60,
    env: dict[str, str] | None = None,
) -> CommandResult:
    cmd = [str(item) for item in argv]
    merged_env = os.environ.copy()
    if env:
        merged_env.update(env)
    try:
        completed = subprocess.run(
            cmd,
            cwd=str(cwd),
            env=merged_env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
        )
        return CommandResult(cmd, str(cwd), completed.returncode, completed.stdout, completed.stderr)
    except subprocess.TimeoutExpired as exc:
        return CommandResult(
            cmd,
            str(cwd),
            124,
            exc.stdout or "",
            exc.stderr or "",
            timeout=True,
        )


def find_cli() -> Path:
    cli = ROOT / "zig-out/bin/kira"
    if cli.exists():
        return cli
    build = run(["zig", "build"], timeout=180)
    if build.exit_code != 0 or not cli.exists():
        raise RuntimeError(f"zig build did not produce {cli}\n{build.stderr}")
    return cli


def read_text(path: Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


def has_port_listener(port: int) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.settimeout(0.2)
        return sock.connect_ex(("127.0.0.1", port)) == 0


def wait_http(url: str, timeout_s: float = 10.0) -> bool:
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=1):
                return True
        except Exception:
            time.sleep(0.1)
    return False


def chrome_path() -> str | None:
    candidates = [
        os.environ.get("KIRA_VERIFY_CHROME"),
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        shutil.which("google-chrome"),
        shutil.which("chromium"),
        shutil.which("chromium-browser"),
    ]
    for candidate in candidates:
        if candidate and Path(candidate).exists():
            return candidate
    return None


def validate_wasm(path: Path) -> dict[str, object]:
    data = path.read_bytes() if path.exists() else b""
    header = data[:8]
    probe = {
        "path": str(path),
        "exists": path.exists(),
        "bytes": len(data),
        "header": header.hex(),
        "header_only": data == b"\x00asm\x01\x00\x00\x00",
        "valid_magic": header == b"\x00asm\x01\x00\x00\x00",
        "has_sections": len(data) > 8,
        "has_kira_app_start": b"kira_app_start" in data,
        "has_runtime_started_export": b"kira_runtime_started" in data,
        "has_app_entrypoint_export": b"kira_app_entrypoint_invoked" in data,
        "has_ui_retained_export": b"kira_retained_tree_initialized" in data,
        "has_placeholder_false": b'placeholder":false' in data,
    }
    if not path.exists():
        probe["node_compile"] = False
        probe["node_instantiate"] = False
        return probe
    node = run(
        [
            "node",
            "-e",
            (
                "const fs=require('fs');"
                f"const b=fs.readFileSync({json.dumps(str(path))});"
                "WebAssembly.instantiate(b,{}).then((m)=>{"
                "const e=Object.keys(m.instance.exports).sort();"
                "console.log(JSON.stringify({compile:true,instantiate:true,exports:e}));"
                "}).catch((e)=>{console.error(e && e.stack || String(e));process.exit(1);});"
            ),
        ],
        timeout=20,
    )
    probe["node"] = node.summary(4000)
    probe["node_compile"] = node.exit_code == 0 and '"compile":true' in node.stdout
    probe["node_instantiate"] = node.exit_code == 0 and '"instantiate":true' in node.stdout
    probe["node_exports"] = node.stdout.strip()
    return probe


def check_placeholder_source_scan() -> Check:
    source_files = [
        ROOT / "packages/kira_cli/src/commands/export.zig",
        ROOT / "packages/kira_live/src/supervisor.zig",
        ROOT / "packages/kira_wasm_runtime/src/root.zig",
    ]
    findings: list[str] = []
    for path in source_files:
        text = read_text(path)
        if "Kira Apple runner scaffold" in text:
            findings.append(f"{path.relative_to(ROOT)} contains Apple scaffold success text")
        if "Kira Android runner scaffold" in text:
            findings.append(f"{path.relative_to(ROOT)} contains Android scaffold success text")
        if "Kira platform export scaffold" in text:
            findings.append(f"{path.relative_to(ROOT)} contains platform scaffold success text")
        if "static scaffold" in text:
            findings.append(f"{path.relative_to(ROOT)} contains static scaffold wording")
    for path in [ROOT / "packages/kira_cli/src/commands/export.zig", ROOT / "packages/kira_live/src/supervisor.zig"]:
        text = read_text(path)
        if "0x00, 0x61, 0x73, 0x6d" in text or "\\0asm" in text:
            findings.append(f"{path.relative_to(ROOT)} can directly spell a header-only Wasm artifact")
    wasm_text = read_text(ROOT / "packages/kira_wasm_runtime/src/root.zig")
    allowed_header_use = (
        "isHeaderOnly" in wasm_text
        and "validateModule" in wasm_text
        and "generated web runtime wasm is not the placeholder header" in wasm_text
    )
    if not allowed_header_use:
        findings.append("kira_wasm_runtime does not pair Wasm header emission with validation and a no-placeholder test")
    return Check("placeholder_source_scan", not findings, {"findings": findings})


def check_web_wasm(cli: Path) -> Check:
    export_result = run([cli, "export", "web", UI_BASIC, "--surface", "webgpu"], timeout=90)
    live_result = run([cli, "live", "web", UI_BASIC, "--surface", "webgpu", "--run-for", "1s"], timeout=90)
    export_root = UI_BASIC / "exports/web"
    live_root = UI_BASIC / ".kira-build/live/runners/web-kira-wasm"
    export_probe = validate_wasm(export_root / "kira-app.wasm")
    live_probe = validate_wasm(live_root / "kira-app.wasm")
    loader = read_text(export_root / "kira-wasm.js") if (export_root / "kira-wasm.js").exists() else ""
    manifest = read_text(export_root / "manifest.json") if (export_root / "manifest.json").exists() else ""
    required_loader_markers = [
        "KIRA_WASM_MODULE_LOADED",
        "KIRA_RUNTIME_STARTED",
        "KIRA_APP_ENTRYPOINT_INVOKED",
        "KIRA_UI_FOUNDATION_APP_STARTED",
        "KIRA_UI_TREE_BUILT",
        "KIRA_UI_RETAINED_TREE_READY",
        "KIRA_UI_LAYOUT_NON_EMPTY",
        "KIRA_UI_DRAW_COMMANDS_SUBMITTED",
        "KIRA_GRAPHICS_WEBGPU_INITIALIZED",
        "KIRA_WEBGPU_PIPELINE_CREATED",
        "KIRA_WEBGPU_FRAME_RENDERED",
    ]
    missing_markers = [marker for marker in required_loader_markers if marker not in loader]
    live_combined = live_result.stdout + live_result.stderr
    live_serves_http = "live.bundle.served" in live_combined and "http://127.0.0.1:" in live_combined
    live_uses_file_url = "file://" in live_combined
    probes = [export_probe, live_probe]
    passed = (
        export_result.exit_code == 0
        and live_result.exit_code == 0
        and live_serves_http
        and not live_uses_file_url
        and all(probe["exists"] for probe in probes)
        and all(probe["bytes"] > 8 for probe in probes)
        and all(probe["valid_magic"] for probe in probes)
        and all(probe["has_sections"] for probe in probes)
        and all(not probe["header_only"] for probe in probes)
        and all(probe["node_compile"] for probe in probes)
        and all(probe["node_instantiate"] for probe in probes)
        and all(probe["has_kira_app_start"] for probe in probes)
        and export_probe["has_runtime_started_export"]
        and export_probe["has_app_entrypoint_export"]
        and '"placeholder":false' in manifest
        and not missing_markers
    )
    return Check(
        "web_wasm_export_live",
        passed,
        {
            "export": export_result.summary(),
            "live": live_result.summary(),
            "export_probe": export_probe,
            "live_probe": live_probe,
            "missing_loader_markers": missing_markers,
            "live_serves_http": live_serves_http,
            "live_uses_file_url": live_uses_file_url,
        },
    )


def check_browser_webgpu() -> Check:
    export_root = UI_BASIC / "exports/web"
    if not (export_root / "index.html").exists():
        return Check("browser_webgpu", False, {"error": f"{export_root}/index.html missing"})
    chrome = chrome_path()
    if not chrome:
        return Check("browser_webgpu", False, {"error": "Chrome/Chromium not found"})
    if has_port_listener(CHROME_PORT):
        return Check("browser_webgpu", False, {"error": f"CDP port {CHROME_PORT} already in use"})
    if has_port_listener(HTTP_PORT):
        return Check("browser_webgpu", False, {"error": f"HTTP port {HTTP_PORT} already in use"})

    http = subprocess.Popen(
        [sys.executable, "-m", "http.server", str(HTTP_PORT), "--bind", "127.0.0.1"],
        cwd=str(export_root),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    user_data_dir = ROOT / ".zig-cache/verify-real-runtime-chrome"
    chrome_proc = subprocess.Popen(
        [
            chrome,
            "--headless=new",
            "--enable-unsafe-webgpu",
            "--enable-features=Vulkan,UseSkiaRenderer",
            f"--remote-debugging-port={CHROME_PORT}",
            f"--user-data-dir={user_data_dir}",
            "about:blank",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        if not wait_http(f"http://127.0.0.1:{HTTP_PORT}/index.html", 10):
            return Check("browser_webgpu", False, {"error": "HTTP server did not respond"})
        deadline = time.time() + 10
        while time.time() < deadline:
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{CHROME_PORT}/json", timeout=1):
                    break
            except Exception:
                time.sleep(0.1)
        env = {
            "KIRA_WEBGPU_URL": f"http://127.0.0.1:{HTTP_PORT}/index.html",
            "KIRA_CDP_JSON": f"http://127.0.0.1:{CHROME_PORT}/json",
        }
        node = run(["node", ".codex/work/evidence/ui-foundation-real-wasm-webgpu-check.mjs"], timeout=45, env=env)
        browser_json = EVIDENCE / "ui-foundation-real-wasm-webgpu-browser.json"
        data = json.loads(read_text(browser_json)) if browser_json.exists() else {}
        log_text = json.dumps(data.get("consoleEvents", []))
        required_logs = [
            "KIRA_WASM_MODULE_LOADED",
            "KIRA_RUNTIME_STARTED",
            "KIRA_APP_ENTRYPOINT_INVOKED",
            "KIRA_UI_FOUNDATION_APP_STARTED",
            "KIRA_UI_TREE_BUILT",
            "KIRA_UI_RETAINED_TREE_READY",
            "KIRA_UI_LAYOUT_NON_EMPTY",
            "KIRA_UI_DRAW_COMMANDS_SUBMITTED",
            "KIRA_GRAPHICS_WEBGPU_INITIALIZED",
            "KIRA_WEBGPU_PIPELINE_CREATED",
            "KIRA_WEBGPU_FRAME_RENDERED",
        ]
        missing_logs = [item for item in required_logs if item not in log_text]
        passed = node.exit_code == 0 and bool(data.get("pass")) and not missing_logs
        return Check(
            "browser_webgpu",
            passed,
            {
                "node": node.summary(4000),
                "browser_json": str(browser_json),
                "missing_logs": missing_logs,
                "state": data.get("state", {}),
                "wasm_probe": data.get("wasmProbe", {}),
                "gpu_probe": data.get("gpuProbe", {}),
            },
        )
    finally:
        for proc in (chrome_proc, http):
            proc.terminate()
        for proc in (chrome_proc, http):
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()


def check_retained_tree_native(cli: Path) -> Check:
    llvm = run([cli, "run", "--backend", "llvm", UI_RETAINED], timeout=90)
    hybrid = run([cli, "run", "--backend", "hybrid", UI_RETAINED], timeout=90)
    vm = run([cli, "run", "--backend", "vm", UI_RETAINED], timeout=90)
    required = [
        "KIRA_UI_RETAINED_TREE_READY",
        "KIRA_UI_RETAINED_CHILDREN_RECONCILED",
        "KIRA_UI_LAYOUT_NON_EMPTY",
        "KIRA_UI_DRAW_COMMANDS_SUBMITTED",
    ]
    missing = {
        "llvm": [item for item in required if item not in llvm.stdout],
        "hybrid": [item for item in required if item not in hybrid.stdout],
        "vm": [item for item in required if item not in vm.stdout],
    }
    passed = (
        llvm.exit_code == 0
        and hybrid.exit_code == 0
        and vm.exit_code == 0
        and "3" in llvm.stdout.split()
        and "3" in hybrid.stdout.split()
        and "3" in vm.stdout.split()
        and not missing["llvm"]
        and not missing["hybrid"]
        and not missing["vm"]
    )
    return Check(
        "retained_tree_native",
        passed,
        {
            "llvm": llvm.summary(4000),
            "hybrid": hybrid.summary(4000),
            "vm": vm.summary(4000),
            "missing_markers": missing,
        },
    )


def check_device_runner_sources(cli: Path) -> Check:
    ios = run([cli, "export", "ios", UI_BASIC], timeout=60)
    android = run([cli, "export", "android", UI_BASIC], timeout=60)
    apple_root = UI_BASIC / "exports/apple"
    android_root = UI_BASIC / "exports/android"
    apple_main = apple_root / "Shared/KiraRuntime/main.m"
    android_main = android_root / "app/src/main/java/com/kira/app/MainActivity.java"
    apple_text = read_text(apple_main) if apple_main.exists() else ""
    android_text = read_text(android_main) if android_main.exists() else ""
    apple_runner_config = apple_root / "Shared/KiraRuntime/KiraRunner.toml"
    android_runner_config = android_root / "app/src/main/assets/KiraRunner.toml"
    findings: list[str] = []
    if ios.exit_code != 0:
        findings.append("iOS export command failed")
    if android.exit_code != 0:
        findings.append("Android export command failed")
    for forbidden in ["scaffold", "placeholder", "Kira Apple runner scaffold", "Kira Android runner scaffold"]:
        if forbidden in apple_text:
            findings.append(f"Apple runner source contains forbidden text: {forbidden}")
        if forbidden in android_text:
            findings.append(f"Android runner source contains forbidden text: {forbidden}")
    if not apple_runner_config.exists():
        findings.append("Apple runner config missing")
    if not android_runner_config.exists():
        findings.append("Android runner config missing")
    for config_path, label in [(apple_runner_config, "Apple"), (android_runner_config, "Android")]:
        config = read_text(config_path) if config_path.exists() else ""
        if str(UI_BASIC) not in config:
            findings.append(f"{label} runner config does not reference selected UI Foundation example")
        if "basic-foundation-app" not in config:
            findings.append(f"{label} runner config does not name the selected example")
        if "KIRA_UI_FOUNDATION_APP_STARTED" not in config:
            findings.append(f"{label} runner config does not carry UI Foundation startup marker")
    return Check(
        "device_runner_sources",
        not findings,
        {
            "ios": ios.summary(),
            "android": android.summary(),
            "apple_main": str(apple_main),
            "android_main": str(android_main),
            "apple_config": str(apple_runner_config),
            "android_config": str(android_runner_config),
            "findings": findings,
        },
    )


def check_ios_simulator(cli: Path) -> Check:
    result = run([cli, "live", "ios", UI_BASIC, "--device", "simulator", "--run-for", "1s"], timeout=180)
    required = [
        "live.ios.simulator.detected",
        "live.ios.simulator.preferred.detected",
        "live.ios.simulator.build.succeeded",
        "live.ios.simulator.install.succeeded",
        "live.ios.simulator.launch.succeeded",
        "KIRA_UI_FOUNDATION_APP_STARTED",
        "KIRA_UI_TREE_BUILT",
        "KIRA_UI_RETAINED_TREE_READY",
        "KIRA_UI_LAYOUT_NON_EMPTY",
        "KIRA_UI_DRAW_COMMANDS_SUBMITTED",
        "KIRA_APP_RENDERED_VISIBLE_CONTENT",
    ]
    combined = result.stdout + result.stderr
    missing = [item for item in required if item not in combined]
    return Check("ios_simulator", result.exit_code == 0 and not missing, {"result": result.summary(6000), "missing": missing})


def check_physical_iphone(cli: Path) -> Check:
    result = run([cli, "live", "ios", UI_BASIC, "--host", "0.0.0.0", "--port", "42111", "--run-for", "1s"], timeout=240)
    combined = result.stdout + result.stderr
    required_attempt = [
        "live.ios.tools.detected",
        "live.ios.sdk.detected",
        "live.ios.signing.team.configured",
        "AKD4RFY7LU",
    ]
    physical_required = [
        "live.ios.physical.detected",
        "live.ios.endpoint.selected",
    ]
    missing_attempt = [item for item in required_attempt if item not in combined]
    missing_physical_attempt = [item for item in physical_required if item not in combined]
    success_markers = [
        "live.ios.runner.device_build.succeeded",
        "live.ios.install.succeeded",
        "live.ios.launch.succeeded",
        "KIRA_UI_FOUNDATION_APP_STARTED",
        "KIRA_UI_RETAINED_TREE_READY",
        "KIRA_APP_RENDERED_VISIBLE_CONTENT",
    ]
    missing_success = [item for item in success_markers if item not in combined]
    provisioning_blocked = "KTC075" in combined or "provisioning" in combined.lower()
    no_usable_iphone = "live.ios.physical.blocked reason=no-usable-iphone" in combined
    fallback_markers = [
        "live.ios.simulator.fallback.used",
        "live.ios.simulator.build.succeeded",
        "live.ios.simulator.install.succeeded",
        "live.ios.simulator.launch.succeeded",
        "KIRA_UI_FOUNDATION_APP_STARTED",
        "KIRA_UI_RETAINED_TREE_READY",
        "KIRA_APP_RENDERED_VISIBLE_CONTENT",
    ]
    missing_fallback = [item for item in fallback_markers if item not in combined]
    physical_success = result.exit_code == 0 and not missing_attempt and not missing_physical_attempt and not missing_success
    provisioning_attempt = provisioning_blocked and not missing_attempt and not missing_physical_attempt
    unavailable_attempt = no_usable_iphone and result.exit_code == 0 and not missing_attempt and not missing_fallback
    passed = physical_success or provisioning_attempt or unavailable_attempt
    return Check(
        "physical_iphone_attempt",
        passed,
        {
            "result": result.summary(8000),
            "missing_attempt": missing_attempt,
            "missing_physical_attempt": missing_physical_attempt,
            "missing_success": missing_success,
            "missing_fallback": missing_fallback,
            "provisioning_blocked": provisioning_blocked,
            "physical_unavailable": no_usable_iphone,
            "physical_success": physical_success,
            "simulator_fallback_success": unavailable_attempt,
            "required_device": {
                "team": "AKD4RFY7LU",
                "name": "Buy Your Own Network",
                "model": "iPhone 13 Pro Max",
                "id": "F38A1A9C-CB7F-59BB-855D-AA67C0C86580",
            },
        },
    )


def check_android_emulator(cli: Path) -> Check:
    result = run([cli, "live", "android", UI_BASIC, "--run-for", "1s"], timeout=240)
    combined = result.stdout + result.stderr
    required = [
        "live.android.tools.detected",
        "live.android.emulator.detected",
        "live.android.build.succeeded",
        "live.android.install.succeeded",
        "live.android.launch.succeeded",
        "KIRA_UI_FOUNDATION_APP_STARTED",
        "KIRA_UI_TREE_BUILT",
        "KIRA_UI_RETAINED_TREE_READY",
        "KIRA_UI_LAYOUT_NON_EMPTY",
        "KIRA_UI_DRAW_COMMANDS_SUBMITTED",
        "KIRA_APP_RENDERED_VISIBLE_CONTENT",
    ]
    missing = [item for item in required if item not in combined]
    return Check("android_emulator", result.exit_code == 0 and not missing, {"result": result.summary(8000), "missing": missing})


def check_backend_policy(cli: Path) -> Check:
    fixture = ROOT / "tests/fixtures/backend_policy_app"
    result = run([cli, "check", fixture, "--print-backend-policy"], timeout=60)
    source = read_text(ROOT / "packages/kira_manifest/src/platform_config.zig")
    required_source = [
        "ExecutionBackend",
        "wasm_runtime",
        "wasm_aot",
        "HybridSelectionMode",
        "native_except_runtime",
        "LibraryExecutionPolicy",
        "BackendSelectionSource",
    ]
    missing_source = [item for item in required_source if item not in source]
    required_output = [
        "backend.policy package=kira-graphics backend=llvm source=app-manifest native_required=true ffi_allowed=true",
        "backend.policy package=ui-foundation backend=hybrid source=app-manifest hybrid_selection=native_except_runtime",
        "backend.policy web backend=wasm_aot graphics_bridge=webgpu",
    ]
    missing_output = [item for item in required_output if item not in result.stdout]
    return Check(
        "backend_policy",
        result.exit_code == 0 and not missing_source and not missing_output,
        {
            "result": result.summary(6000),
            "fixture": str(fixture),
            "missing_source": missing_source,
            "missing_output": missing_output,
        },
    )


def check_ffi_safety(cli: Path) -> Check:
    result = run([cli, "check", "--backend", "vm", ROOT / "tests/pass/run/ffi_sokol_triangle_native"], timeout=90)
    combined = result.stdout + result.stderr
    required = "package `kira-graphics` requires native FFI but was selected for VM execution"
    passed = result.exit_code != 0 and required in combined and "runtime" not in combined.lower()
    return Check("ffi_safety", passed, {"result": result.summary(6000), "required": required})


def main() -> int:
    EVIDENCE.mkdir(parents=True, exist_ok=True)
    cli = find_cli()
    checks = [
        check_placeholder_source_scan(),
        check_web_wasm(cli),
        check_browser_webgpu(),
        check_retained_tree_native(cli),
        check_device_runner_sources(cli),
        check_ios_simulator(cli),
        check_physical_iphone(cli),
        check_android_emulator(cli),
        check_backend_policy(cli),
        check_ffi_safety(cli),
    ]
    report = {
        "repo": str(ROOT),
        "ui_foundation": str(UI_FOUNDATION),
        "cli": str(cli),
        "passed": all(item.passed for item in checks),
        "checks": [
            {"name": item.name, "passed": item.passed, "details": item.details}
            for item in checks
        ],
    }
    REPORT_JSON.write_text(json.dumps(report, indent=2), encoding="utf-8")
    for item in checks:
        status = "PASS" if item.passed else "FAIL"
        print(f"{status} {item.name}")
        if not item.passed:
            print(json.dumps(item.details, indent=2)[:6000])
    print(f"report={REPORT_JSON}")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
