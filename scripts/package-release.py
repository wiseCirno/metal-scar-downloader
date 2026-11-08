"""Validate release inputs, smoke-check the embedded API, and collect CI assets."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def version():
    value = json.loads((ROOT / "package.json").read_text(encoding="utf-8"))["version"]
    if not re.fullmatch(r"\d+\.\d+\.\d+", value):
        raise ValueError(f"Expected a release version, got {value!r}")
    cargo = tomllib.loads((ROOT / "src/Cargo.toml").read_text(encoding="utf-8"))
    lock = tomllib.loads((ROOT / "src/Cargo.lock").read_text(encoding="utf-8"))
    own = next(p for p in lock["package"] if p["name"] == cargo["package"]["name"])
    tauri = json.loads((ROOT / "src/tauri.conf.json").read_text(encoding="utf-8"))
    assert cargo["package"]["version"] == own["version"] == value, "Application versions differ"
    assert tauri["version"] == "../package.json", "Tauri must use the shared application version"
    if os.environ.get("GITHUB_REF_TYPE") == "tag":
        tag = os.environ["GITHUB_REF_NAME"]
        assert tag in (value, f"v{value}"), f"Tag {tag!r} does not match application version {value}"
    print(f"Application version: {value}")
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
            output.write(f"version={value}\n")
    return value


def check_runtime(target):
    runtime = ROOT / "src/resources/netease-api"
    executable = runtime / ("node.exe" if "windows" in target else "bin/node")
    # Resource serialization stores file bytes; the Rust unpacker restores Unix mode.
    if os.name != "nt":
        executable.chmod(0o700)
    subprocess.run([str(executable), "--version"], check=True)
    for path in runtime.rglob("*"):
        assert not path.is_symlink(), f"Runtime contains an unsupported symbolic link: {path}"
    with tempfile.TemporaryDirectory(prefix="metal-scar-ci-") as cache:
        env = dict(os.environ, TMPDIR=cache, TMP=cache, TEMP=cache)
        # Use files for stdio to work in environments that disallow named pipes.
        with tempfile.TemporaryFile(mode="w+") as request, tempfile.TemporaryFile(mode="w+") as response:
            request.write('{"id":1,"method":"health","query":{}}\n')
            request.seek(0)
            subprocess.run([str(executable), str(runtime / "bridge.cjs")], cwd=runtime,
                           env=env, stdin=request, stdout=response, check=True, timeout=60)
            response.seek(0)
            result = json.loads(response.read())
        expected = json.loads((ROOT / "runtime/netease-api/package.json")
                              .read_text(encoding="utf-8"))["dependencies"]["@neteasecloudmusicapienhanced/api"]
        assert result.get("id") == 1 and result.get("body", {}).get("code") == 200, result
        assert result["body"]["version"] == expected, result
    print(f"Embedded runtime health passed: {target}, API {expected}")


def package(target, kind, output):
    value = version()
    release = ROOT / "src/target" / target / "release"
    output.mkdir(parents=True, exist_ok=True)
    platforms = {
        "x86_64-pc-windows-msvc": ("windows", "x64"),
        "aarch64-apple-darwin": ("macos", "aarch64"),
        "x86_64-unknown-linux-gnu": ("linux", "x64"),
    }
    expected_kind, arch = platforms[target]
    assert kind == expected_kind, f"Target {target} requires format {expected_kind}, got {kind}"
    stem = f"metal-scar-downloader_{value}_{kind}_{arch}"
    assets = []
    if kind == "windows":
        source = release / "metal-scar-downloader.exe"
        assert source.stat().st_size > 50_000_000, "Embedded runtime missing"
        destination = output / f"{stem}.zip"
        with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED) as archive:
            archive.write(source, "铁痕电台下载器.exe")
        with zipfile.ZipFile(destination) as archive:
            assert archive.testzip() is None
        assets.append(destination)
    else:
        formats = [("dmg", "*.dmg")] if kind == "macos" else [("appimage", "*.AppImage"), ("deb", "*.deb"), ("rpm", "*.rpm")]
        for folder, pattern in formats:
            matches = list((release / "bundle" / folder).glob(pattern))
            assert len(matches) == 1, f"Expected one {pattern} in {release / 'bundle' / folder}, got {matches}"
            source = matches[0]
            assert source.stat().st_size > 50_000_000, f"Package unexpectedly small: {source}"
            destination = output / f"{stem}{source.suffix}"
            shutil.copy2(source, destination)
            assets.append(destination)
    for path in assets:
        print(f"Release asset: {path.name} ({path.stat().st_size} bytes)")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check-version", action="store_true")
    parser.add_argument("--check-runtime", action="store_true")
    parser.add_argument("--target")
    parser.add_argument("--format", choices=["windows", "macos", "linux"])
    parser.add_argument("--output-dir", type=Path, default=ROOT / "release-assets")
    args = parser.parse_args()
    if args.check_version:
        version()
    elif args.check_runtime and args.target:
        check_runtime(args.target)
    elif args.target and args.format:
        package(args.target, args.format, args.output_dir)
    else:
        parser.error("Use --check-version, --check-runtime --target, or --target and --format")
