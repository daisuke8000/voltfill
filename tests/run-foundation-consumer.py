#!/usr/bin/env python3
"""Run the public-only consumer in an independent temporary Rust workspace."""
from pathlib import Path
import argparse
import json
import shutil
import subprocess
import tempfile
import tomllib

parser = argparse.ArgumentParser()
parser.add_argument("--pdfium-library", type=Path)
args = parser.parse_args()
repo = Path(__file__).resolve().parent.parent
feature = "pdfium" if args.pdfium_library else "hayro"
if args.pdfium_library and not args.pdfium_library.is_absolute():
    parser.error("--pdfium-library must be an explicit absolute file path")
with tempfile.TemporaryDirectory(prefix="voltfill-consumer-") as directory:
    root = Path(directory)
    shutil.copytree(repo / "tests/foundation-consumer", root, dirs_exist_ok=True)
    for name in ["Cargo.lock", "rust-toolchain.toml"]:
        shutil.copyfile(repo / name, root / name)
    manifest = root / "Cargo.toml"
    manifest.write_text(manifest.read_text().replace("../../crates/", str(repo / "crates") + "/"))
    subprocess.run(["cargo", "generate-lockfile", "--offline"], cwd=root, check=True)
    def packages(path):
        return {(p["name"], p["version"], p.get("source")): p for p in tomllib.loads(path.read_text())["package"] if "source" in p}
    locked = packages(repo / "Cargo.lock")
    assert all(key in locked and locked[key].get("checksum") == value.get("checksum") for key, value in packages(root / "Cargo.lock").items()), "consumer changed locked external dependency versions or checksums"
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--offline", "--locked", "--format-version", "1", "--features", feature], cwd=root))
    assert len(metadata["workspace_members"]) == 1
    assert all(p["name"] != "voltfill-core" for p in metadata["packages"])
    command = ["cargo", "run", "--offline", "--locked", "--features", feature, "--target-dir", str(root / "target")]
    if args.pdfium_library:
        command += ["--", str(args.pdfium_library)]
    subprocess.run(command, cwd=root, check=True)
