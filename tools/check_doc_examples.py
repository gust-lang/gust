#!/usr/bin/env python3
"""Extract `metel` code blocks from Markdown/MDX docs and run the runnable ones.

Scans README.md, tutorials, and the spec for fenced ```metel code blocks and runs
each block that looks like a complete program against a real `metel` binary, so
stale/broken examples fail loudly instead of sitting undiscovered for releases.

SCOPE: a block is checked only if it contains `fun main`. Everything else (a bare
signature illustrating a syntax shape, an elided `{ ... }`/`/* ... */` body, a
lexical-form literal) is skipped — there's nothing to run. A skipped block is not a
verified block; treat silence on a fragment as "not checked," not "confirmed
correct."

Two kinds of `fun main` block still aren't safe to run as a standalone file even
though they parse fine in isolation: one that only makes sense after an earlier
code block in the same doc (an aspect/struct/enum introduced a section above and
used without repeating the declaration), and one that's deliberately split across
multiple files via `// path/to/file.mtl` comments. Both are marked, in the doc
source, with an HTML comment on the line directly above the fence:

    <!-- doc-example: skip reason="depends on an earlier block in this doc" -->
    ```metel
    ...
    ```

A doc can also *intend* for an example not to compile or run — illustrating the
error a rule produces is often clearer than describing it in prose. Mark those with
`expect-fail` instead of `skip`; the checker still runs them, but treats a non-zero
exit as the pass and a zero exit as a regression (the doc's claim about what's
illegal no longer holds):

    <!-- doc-example: expect-fail reason="ascription failure — the whole point" -->
    ```metel
    ...
    ```

Usage:
    python3 check_doc_examples.py --binary path/to/metel FILE_OR_DIR [FILE_OR_DIR ...]

Each positional argument is a .md/.mdx file, or a directory searched recursively for
.md/.mdx files. Exits non-zero if any block fails (including an expect-fail block
that unexpectedly passes).
"""

import argparse
import re
import subprocess
import sys
import tempfile
from pathlib import Path

CODE_FENCE_RE = re.compile(r"```metel\n(.*?)```", re.DOTALL)
MAIN_RE = re.compile(r"\bfun\s+main\b")
MARKER_RE = re.compile(r'^<!--\s*doc-example:\s*(skip|expect-fail)\s*(?:reason="([^"]*)")?\s*-->$')


def find_doc_files(paths):
    """Resolve CLI arguments (files or directories) to a sorted list of .md/.mdx files."""
    files = []
    for raw in paths:
        p = Path(raw)
        if p.is_dir():
            files.extend(p.rglob("*.md"))
            files.extend(p.rglob("*.mdx"))
        elif p.is_file():
            files.append(p)
        else:
            sys.exit(f"error: path does not exist: {raw}")
    return sorted(set(files))


def dequote_block(code):
    """Strip a leading Markdown blockquote `> ` from every line, if the whole block is quoted.

    A ```metel fence nested inside a `>` blockquote (used for callouts) is captured
    with the quote marker still on every line, which is never valid Metel syntax. Only
    strips when *every* non-blank line is quoted, so an ordinary block that merely
    starts with `>` (unlikely, but not this function's business to assume) is untouched.
    """
    lines = code.split("\n")
    non_blank = [line for line in lines if line.strip() != ""]
    if not non_blank or not all(line.lstrip().startswith(">") for line in non_blank):
        return code
    stripped = []
    for line in lines:
        if line.startswith("> "):
            stripped.append(line[2:])
        elif line.startswith(">"):
            stripped.append(line[1:])
        else:
            stripped.append(line)  # a blank line inside the quote
    return "\n".join(stripped)


def find_marker(text, start_line):
    """Return (kind, reason) from a `doc-example:` comment on the line right above
    start_line, or (None, None) if there isn't one."""
    lines = text.split("\n")
    if start_line < 2:
        return None, None
    preceding = lines[start_line - 2].strip()
    m = MARKER_RE.match(preceding)
    if not m:
        return None, None
    return m.group(1), m.group(2)


def extract_blocks(doc_path):
    """Yield (start_line, code, marker_kind, marker_reason) for every ```metel block."""
    text = doc_path.read_text()
    for match in CODE_FENCE_RE.finditer(text):
        start_line = text.count("\n", 0, match.start()) + 1
        kind, reason = find_marker(text, start_line)
        yield start_line, dequote_block(match.group(1)), kind, reason


def run_block(binary, code):
    """Run one code block as a standalone program. Returns (ok, output)."""
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".mtl", delete=False
    ) as tmp:
        tmp.write(code)
        tmp_path = tmp.name
    try:
        result = subprocess.run(
            [binary, tmp_path],
            capture_output=True,
            text=True,
            timeout=30,
        )
        ok = result.returncode == 0
        output = result.stdout + result.stderr
        return ok, output
    except subprocess.TimeoutExpired:
        return False, "timed out after 30s"
    finally:
        Path(tmp_path).unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--binary", required=True, help="path to the metel binary")
    parser.add_argument("paths", nargs="+", help="files or directories to scan")
    args = parser.parse_args()

    binary = str(Path(args.binary).resolve())
    if not Path(binary).is_file():
        sys.exit(f"error: --binary not found: {binary}")

    doc_files = find_doc_files(args.paths)
    if not doc_files:
        sys.exit("error: no .md/.mdx files found under the given paths")

    passed = 0
    failed = 0
    skipped = 0
    failures = []

    for doc_path in doc_files:
        for start_line, code, marker_kind, marker_reason in extract_blocks(doc_path):
            if marker_kind == "skip":
                skipped += 1
                continue

            if marker_kind == "expect-fail":
                ok, output = run_block(binary, code)
                if ok:
                    failed += 1
                    reason = f" ({marker_reason})" if marker_reason else ""
                    failures.append((
                        doc_path, start_line,
                        f"expected this block to fail{reason}, but it ran cleanly — "
                        "the doc's claim about what's illegal may no longer hold",
                    ))
                else:
                    passed += 1
                continue

            if not MAIN_RE.search(code):
                skipped += 1
                continue

            ok, output = run_block(binary, code)
            if ok:
                passed += 1
            else:
                failed += 1
                failures.append((doc_path, start_line, output.strip()))

    for doc_path, start_line, output in failures:
        print(f"FAIL {doc_path}:{start_line}")
        for line in output.splitlines():
            print(f"    {line}")
        print()

    print(f"{passed} passed, {failed} failed, {skipped} skipped")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
