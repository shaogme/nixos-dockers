#!/usr/bin/env python3
"""Run workspace test targets except Docker integration test files."""

import json
import subprocess
from pathlib import Path


def run(command: list[str]) -> None:
    print("+", " ".join(command), flush=True)
    subprocess.run(command, check=True)


metadata = json.loads(
    subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        text=True,
    )
)

for package in metadata["packages"]:
    if package["id"] not in metadata["workspace_members"]:
        continue

    selectors: list[str] = []
    has_doctests = False
    for target in package["targets"]:
        if Path(target["src_path"]).name == "docker.rs":
            continue

        kinds = target["kind"]
        if target["test"]:
            if "test" in kinds:
                selectors.extend(["--test", target["name"]])
            elif "bin" in kinds:
                selectors.extend(["--bin", target["name"]])
            elif "example" in kinds:
                selectors.extend(["--example", target["name"]])
            elif "bench" in kinds:
                selectors.extend(["--bench", target["name"]])
            elif "lib" in kinds or "proc-macro" in kinds:
                selectors.append("--lib")
                has_doctests = has_doctests or target["doctest"]

    if selectors:
        run(["cargo", "test", "--locked", "--package", package["name"], *selectors])
    if has_doctests:
        run(["cargo", "test", "--locked", "--package", package["name"], "--doc"])
