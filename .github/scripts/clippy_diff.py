"""Fail on Clippy warnings in lines changed by a pull request."""

import json
import os
from pathlib import PurePosixPath
import re
import subprocess
import sys


HUNK = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@")


def changed_ranges(path):
    diff = subprocess.check_output(
        ["git", "--literal-pathspecs", "diff", "--no-ext-diff", "--no-renames",
         "--unified=0", "HEAD^1", "HEAD", "--", path],
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    ranges = []
    for line in diff.splitlines():
        match = HUNK.match(line)
        if match:
            start = int(match[1])
            count = 1 if match[2] is None else int(match[2])
            if count:
                ranges.append((start, start + count - 1))
    return ranges


def main():
    command = [
        "cargo", "+1.100.0-x86_64-pc-windows-msvc", "clippy",
        "--locked", "--workspace", "--all-targets", "--no-deps",
        "--message-format=json",
    ]
    ranges = {}
    violations = 0
    is_pr = os.environ.get("GITHUB_EVENT_NAME") == "pull_request"
    with subprocess.Popen(command, stdout=subprocess.PIPE, text=True, encoding="utf-8") as clippy:
        for line in clippy.stdout:
            event = json.loads(line)
            if event.get("reason") != "compiler-message":
                continue
            message = event["message"]
            if message.get("rendered"):
                print(message["rendered"], end="")
            if message.get("level") != "warning":
                continue
            for span in message["spans"]:
                if not span["is_primary"]:
                    continue
                path = PurePosixPath(span["file_name"].replace("\\", "/"))
                if not path.parts or path.parts[0] != "crates":
                    continue
                if not is_pr:
                    continue
                name = path.as_posix()
                if name not in ranges:
                    ranges[name] = changed_ranges(name)
                if any(start <= span["line_end"] and end >= span["line_start"]
                       for start, end in ranges[name]):
                    violations += 1
                    break
        status = clippy.wait()
    if violations:
        print(f"{violations} Clippy warning(s) on changed lines.", file=sys.stderr)
    return status or int(violations > 0)


if __name__ == "__main__":
    sys.exit(main())
