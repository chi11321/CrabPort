#!/usr/bin/env python3
"""Turn a macOS `sample` report into a flat self-time profile.

`sample` prints a call tree with per-node sample counts; that is hard to read
for "what is burning CPU". This walks the tree and prints, per function, the
samples where it was the *leaf* (its own time) — i.e. the classic flat profile.

Usage:
    flat_profile.py <sample.txt> [top_n]

Lines in the call graph look like:

    +   5073 start  (in dyld) + 2840  [0x19aed4274]
    +     5069 CFRunLoopRunSpecific  (in CoreFoundation) + 588  [0x...]

Depth is encoded by the length of the marker/space prefix before the count, so
a line is a child of the previous line with a strictly shorter prefix.
"""

import re
import sys
from collections import Counter

# prefix (markers + spaces) | count | frame text
LINE = re.compile(r"^(?P<prefix> {4}[+!:| ]*?)(?P<count>\d+) (?P<frame>.*)$")

# `0x104db80c4` / `load address 0x… + 0x…` noise carries no signal.
ADDR = re.compile(r"\s*\[0x[0-9a-f]+\]\s*$")
LOAD = re.compile(r"load address 0x[0-9a-f]+ \+ 0x[0-9a-f]+")


def normalize(frame: str) -> str:
    frame = ADDR.sub("", frame).strip()
    frame = LOAD.sub("load address …", frame)
    return frame


def parse(path: str):
    """Yield (depth, samples, frame) for every node in the call graph."""
    in_graph = False
    with open(path, "r", errors="replace") as handle:
        for raw in handle:
            line = raw.rstrip("\n")
            if line.startswith("Call graph:"):
                in_graph = True
                continue
            if not in_graph:
                continue
            if line.startswith("Total number in stack"):
                break
            match = LINE.match(line)
            if not match:
                continue
            yield (
                len(match.group("prefix")),
                int(match.group("count")),
                normalize(match.group("frame")),
            )


def self_times(path: str) -> Counter:
    """Samples attributed to each frame excluding its children's."""
    self_samples: Counter = Counter()
    # (depth, samples, frame, children_samples)
    stack = []
    for depth, samples, frame in parse(path):
        while stack and stack[-1][0] >= depth:
            pop_depth, pop_samples, pop_frame, children = stack.pop()
            self_samples[pop_frame] += pop_samples - children
            if stack:
                stack[-1][3] += pop_samples
        stack.append([depth, samples, frame, 0])
    while stack:
        _depth, samples, frame, children = stack.pop()
        self_samples[frame] += samples - children
        if stack:
            stack[-1][3] += samples
    # A parent can legitimately have fewer self samples than its children
    # (truncated branches); never report negative time.
    return Counter({f: max(0, n) for f, n in self_samples.items() if n > 0})


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__.strip())
        return 2
    path = sys.argv[1]
    top_n = int(sys.argv[2]) if len(sys.argv) > 2 else 25

    times = self_times(path)
    total = sum(times.values())
    if total == 0:
        print("no samples found — is this a `sample` report?")
        return 1

    print(f"total samples: {total}")
    print(f"{'self%':>6}  {'samples':>8}  function")
    for frame, count in times.most_common(top_n):
        print(f"{100.0 * count / total:5.1f}%  {count:8d}  {frame}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
