#!/usr/bin/env python3
"""Extract `metel` code blocks from Markdown/MDX docs and run the runnable ones.

Scans README.md, tutorials, and the spec for fenced ```metel code blocks and runs
each block that looks like a complete program against a real `metel` binary, so
stale/broken examples fail loudly instead of sitting undiscovered for releases.

Four verification strategies are tried, in this order, per block:

1. **A multi-file example** — a single fence containing 2+ `// path/to/file.mtl`
   comments marking file boundaries. Split into real files at their marked paths in a
   temp directory (so `import`/`root::`/`super::` resolve for real) and run whichever
   segment contains `fun main`.
2. **A complete program** (contains `fun main`) — run directly.
3. **A fragment with no `main` at all** (a bare signature, a struct/aspect
   declaration with no runnable entry point) — first try synthesizing a `fun main`
   that calls every top-level function whose parameters are all recognized
   primitives (or none) with placeholder arguments, so the fragment is actually
   *executed*, not just typechecked. If nothing in the fragment is callable this
   unambiguously (a generic function, a parameter of some other type, or no free
   function at all — just a struct/enum/aspect declaration), or the synthesized
   call doesn't run cleanly, fall back to running the fragment as-is: if it fails
   with *exactly* `[R0001]` ("no main function defined"), that means it parsed and
   typechecked completely, so it counts as verified even though nothing executed.
4. **A bare expression** (a lexical-form literal like `1_000_000` or `'\\n'`, shown
   without a surrounding statement, often several to a block, one per line) — if 1-3
   don't apply or fail, try each non-blank line as the initializer of a throwaway
   `let` in a trivial `fun main`, or (for a line that's already a full statement) as
   that function's body directly. Content that isn't actually a bare expression (an
   elided body, a bare signature) fails this too and is correctly left unverified.

What's left unverified after all four: a block that only makes sense after an earlier
code block in the same doc (an aspect/struct/enum introduced a section above and used
here without repeating the declaration) has no general, safe way to reconstruct that
context — concatenating a doc's earlier blocks risks colliding duplicate declarations
across illustrative blocks that each redeclare the same example type. An elided body
(`{ ... }`, `/* ... */`) or a bare signature with no body is not valid syntax at all,
with or without a `main` wrapped around it. Two *separate* fences that together form
one multi-file example (as opposed to one fence with multiple `// path.mtl` markers)
aren't detected — that would need guessing that two adjacent fences are a matching
pair, which is fragile for the rare case it actually happens. A no-`main` fragment
whose only free functions are generic, take a non-primitive parameter (a struct, an
array, a function type), are declared `-> !` (a function that promises never to
return can't make a synthesized call "succeed" by definition), or don't exist at all
(a struct/enum/aspect declaration with no accompanying function) can't be exercised
by strategy 3's synthesized `main` either — it still falls back to the
parses-and-typechecks-only R0001 check.

Both of the above are marked, in the doc source, with a comment on the line directly
above the fence, so a human reading the doc sees why a block is exempt at its point of
use rather than needing to check this script:

    <!-- doc-example: skip reason="depends on an earlier block in this doc" -->
    ```metel
    ...
    ```

A doc can also *intend* for an example not to compile or run — illustrating the
error a rule produces is often clearer than describing it in prose, and a known,
tracked interpreter bug is sometimes clearer to mark in place than to work around.
Mark those with `expect-fail` instead of `skip`; the checker still runs the same four
strategies above, but treats "none of them verified it" as the pass and "one of them
did" as a regression (the doc's claim about what's illegal, or the bug being tracked,
may no longer hold):

    <!-- doc-example: expect-fail reason="ascription failure — the whole point" -->
    ```metel
    ...
    ```

**`.mdx` files need the JSX form instead** — `<!-- -->` is only a comment in plain
Markdown; MDX parses `<...>` as JSX and fails the website build on it (found the hard
way: `<!--` isn't valid JSX, so an HTML-comment marker in a tutorial's `.mdx` file
compiles fine here, in this Python-regex-based checker, and then breaks
`docusaurus build` outright). Use `{/* ... */}` in a `.mdx` file instead, otherwise
identical:

    {/* doc-example: skip reason="depends on an earlier block in this doc" */}
    ```metel
    ...
    ```

**A `reason=` string must never itself contain a literal `/*` or `*/`** — a comment
of *either* form above still ends up lowered to a JS-style comment somewhere in
Docusaurus's MDX compile, and a reason describing an elided body as "(`/* ... */`)"
closes and reopens that comment mid-string, breaking the build the same way (found
the hard way, same release). Describe the syntax in prose instead of quoting it.

A skipped block is not a verified block; treat silence on a fragment as "not
checked," not "confirmed correct."

**A standalone `.mtl` file counts as one block, the whole file** — for Metel code
that lives outside any doc at all, e.g. metel-website's landing-page showcase
snippets (`src/showcases/*.mtl`, imported as raw text into a React component
instead of living as inline template-literal strings). That JS-string form was
invisible to this script entirely; three of six landing-page snippets were broken
and nobody noticed until they were checked by hand. The `skip`/`expect-fail`
marker goes on the file's own first line instead, as a Metel `//` comment (the
rest of the file is still exactly what gets verified):

    // doc-example: skip reason="depends on a helper not defined in this file"
    fun uses_undefined_helper() { ... }

Usage:
    python3 check_doc_examples.py --binary path/to/metel FILE_OR_DIR [FILE_OR_DIR ...]

Each positional argument is a .md/.mdx/.mtl file, or a directory searched
recursively for .md/.mdx/.mtl files. Exits non-zero if any block fails (including
an expect-fail block that unexpectedly verifies as working).
"""

import argparse
import re
import subprocess
import sys
import tempfile
from collections import Counter
from pathlib import Path, PurePosixPath

CODE_FENCE_RE = re.compile(r"```metel\n(.*?)```", re.DOTALL)
MAIN_RE = re.compile(r"\bfun\s+main\b")
MARKER_RE = re.compile(
    r'^(?:<!--\s*doc-example:\s*(?P<kind1>skip|expect-fail)\s*(?:reason="(?P<reason1>[^"]*)")?\s*-->'
    r'|\{/\*\s*doc-example:\s*(?P<kind2>skip|expect-fail)\s*(?:reason="(?P<reason2>[^"]*)")?\s*\*/\})$'
)
# Same marker, spelled as a Metel line comment -- for a standalone .mtl file, which
# has no surrounding Markdown/MDX comment syntax to borrow.
MTL_MARKER_RE = re.compile(
    r'^//\s*doc-example:\s*(?P<kind>skip|expect-fail)\s*(?:reason="(?P<reason>[^"]*)")?\s*$'
)
FILE_MARKER_RE = re.compile(r"^//\s*(\S+\.mtl)\s*$", re.MULTILINE)
R0001_RE = re.compile(r"\[R0001\]")
# A top-level `fun` declaration -- no leading whitespace, so an indented method inside
# an `extend` block never matches. `params` deliberately excludes `(` and `<` so a
# function-type or generic-type parameter falls through to synthesize_call's own
# rejection rather than being mis-split by this simple, non-nesting-aware group. `ret`
# captures an optional `-> Type` so synthesize_call can reject `-> !` (see below).
FUN_SIG_RE = re.compile(
    r"^(?:public\s+)?fun\s+(?P<name>\w+)\s*(?P<generics><[^>]*>)?\s*\((?P<params>[^()]*)\)"
    r"\s*(?:->\s*(?P<ret>[^{;]+))?",
    re.MULTILINE,
)

UNVERIFIABLE = "unverifiable fragment"

# Primitive parameter types unambiguous enough to fill with a fixed placeholder value.
# Deliberately narrow (RFC issue #686): a struct, an array, a function type, or a
# generic type parameter has no single value that's an obviously-safe guess, so a
# function using any of those is left unsynthesized rather than risking a
# placeholder that trips an unrelated panic and misreports a correct example as broken.
PRIMITIVE_PLACEHOLDERS = {
    "i8": "1", "i16": "1", "i32": "1", "i64": "1",
    "u8": "1", "u16": "1", "u32": "1", "u64": "1",
    "f32": "1.0", "f64": "1.0",
    "boolean": "true",
    "String": '"example"',
    "Char": "'a'",
}


def find_doc_files(paths):
    """Resolve CLI arguments (files or directories) to a sorted list of .md/.mdx/.mtl files."""
    files = []
    for raw in paths:
        p = Path(raw)
        if p.is_dir():
            files.extend(p.rglob("*.md"))
            files.extend(p.rglob("*.mdx"))
            files.extend(p.rglob("*.mtl"))
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
    return m.group("kind1") or m.group("kind2"), m.group("reason1") or m.group("reason2")


def extract_blocks(doc_path):
    """Yield (start_line, code, marker_kind, marker_reason) for every checkable block
    in `doc_path` -- every ```metel fence in a .md/.mdx file, or the whole file
    itself for a standalone .mtl file (see MTL_MARKER_RE's doc)."""
    text = doc_path.read_text()
    if doc_path.suffix == ".mtl":
        lines = text.split("\n")
        m = MTL_MARKER_RE.match(lines[0]) if lines else None
        if m:
            yield 2, "\n".join(lines[1:]), m.group("kind"), m.group("reason")
        else:
            yield 1, text, None, None
        return
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


def split_multi_file_block(code):
    """Split a ```metel block into {relative_path: content} at its `// x.mtl` markers.

    Returns None if the block doesn't have at least 2 markers, or if any marked path
    would escape a temp directory (absolute, or containing `..`) -- defensive, since
    this is parsing arbitrary doc prose, not trusted input.
    """
    matches = list(FILE_MARKER_RE.finditer(code))
    if len(matches) < 2:
        return None
    files = {}
    for i, m in enumerate(matches):
        rel_path = m.group(1)
        if PurePosixPath(rel_path).is_absolute() or ".." in PurePosixPath(rel_path).parts:
            return None
        segment_start = m.end()
        segment_end = matches[i + 1].start() if i + 1 < len(matches) else len(code)
        files[rel_path] = code[segment_start:segment_end]
    return files


def run_multi_file_block(binary, code):
    """Run a multi-file example. Returns (ok, output) or (None, None) if it isn't one."""
    files = split_multi_file_block(code)
    if files is None:
        return None, None
    entry_points = [path for path, content in files.items() if MAIN_RE.search(content)]
    if len(entry_points) != 1:
        return None, None
    entry = entry_points[0]
    with tempfile.TemporaryDirectory() as tmpdir:
        for rel_path, content in files.items():
            full_path = Path(tmpdir, rel_path)
            full_path.parent.mkdir(parents=True, exist_ok=True)
            full_path.write_text(content)
        try:
            result = subprocess.run(
                [binary, str(Path(tmpdir, entry))],
                capture_output=True,
                text=True,
                timeout=30,
            )
            ok = result.returncode == 0
            output = result.stdout + result.stderr
            return ok, output
        except subprocess.TimeoutExpired:
            return False, "timed out after 30s"


def run_wrapped_line(binary, line):
    """Try running one line as a bare expression, then as a bare statement, in a
    trivial main. Returns (ok, output) from whichever form actually ran.

    A lexical-form block is often several independent one-line illustrations
    (`42`, `1_000_000`), not one multi-line expression -- wrapping the whole block
    as a single `let` initializer fails outright for anything but a one-line block.
    The `;` goes on its own line in the expression form -- several examples end with
    a same-line `//` comment (`255u8       // u8`), which would swallow anything
    appended after it on that same line.
    """
    wrapped_expr = f"fun main() {{\n    let _ = {line}\n    ;\n}}\n"
    ok, output = run_block(binary, wrapped_expr)
    if ok:
        return ok, output
    # Some lines (e.g. `let x = "${...}";`) are already a complete statement, not a
    # bare expression -- wrapping *that* as `let _ = let x = ...;` is itself invalid,
    # so try it directly as the block's body instead.
    wrapped_stmt = f"fun main() {{\n    {line}\n}}\n"
    return run_block(binary, wrapped_stmt)


def run_wrapped_expression(binary, code):
    """Try verifying `code` line by line, each as a standalone lexical-form example.

    Passes only if every non-blank line succeeds on its own -- a block mixing a
    genuinely bare expression with an elided body or bare signature on another line
    should not be misclassified as verified just because one line happens to work.
    """
    lines = [line for line in code.split("\n") if line.strip()]
    if not lines:
        return False, ""
    outputs = []
    for line in lines:
        ok, output = run_wrapped_line(binary, line)
        outputs.append(output)
        if not ok:
            return False, output
    return True, "\n".join(outputs)


def synthesize_call(name, generics, params, ret):
    """Return a placeholder-argument call expression for one matched top-level `fun`
    signature, or None if it can't be synthesized unambiguously.

    Bails (returns None) on a generic function (no principled choice of `T`), a
    parameter list this simple, non-nesting-aware regex can't have split cleanly
    (a function-type or generic-type parameter contains its own `(` or `<`), an
    array-typed parameter (`i64[]` isn't in PRIMITIVE_PLACEHOLDERS), a parameter
    with no type annotation at all (e.g. `self` on a method that slipped through --
    shouldn't happen given FUN_SIG_RE's no-leading-whitespace anchor, but this is
    parsing arbitrary doc prose, not trusted input, so fail closed), or a function
    declared `-> !` (the never type) -- a function that promises to never return is,
    by definition, never going to make a synthesized call "succeed"; that's not a
    bug the call could be catching, it's the function working as documented.
    """
    if generics:
        return None
    if ret is not None and ret.strip() == "!":
        return None
    params = params.strip()
    if not params:
        return f"{name}()"
    if "(" in params or "<" in params:
        return None
    args = []
    for raw_param in params.split(","):
        raw_param = raw_param.strip()
        if not raw_param:
            continue
        if ":" not in raw_param:
            return None
        _, _, type_part = raw_param.partition(":")
        placeholder = PRIMITIVE_PLACEHOLDERS.get(type_part.strip())
        if placeholder is None:
            return None
        args.append(placeholder)
    return f"{name}({', '.join(args)})"


def synthesize_main(code):
    """Build a `fun main() { ... }` that calls every top-level function in `code`
    that synthesize_call can fill unambiguously, so a no-main fragment is actually
    *executed* instead of merely typechecked. Returns None if nothing in the
    fragment is callable this way -- a struct/enum/aspect-only fragment, or one
    where every free function is generic or takes a non-primitive parameter.
    """
    calls = []
    for m in FUN_SIG_RE.finditer(code):
        call = synthesize_call(m.group("name"), m.group("generics"), m.group("params"), m.group("ret"))
        if call is not None:
            calls.append(f"    {call};")
    if not calls:
        return None
    return "fun main() {\n" + "\n".join(calls) + "\n}\n"


def classify_block(binary, code):
    """Try every applicable verification strategy for one block.

    Returns (ok, method, output). `method` names whichever strategy produced the
    result -- "unverifiable fragment" specifically means "no strategy applied or
    succeeded", the one case the caller should treat as a skip rather than a failure
    when `ok` is False.
    """
    multi_ok, multi_output = run_multi_file_block(binary, code)
    if multi_ok is not None:
        return multi_ok, "multi-file", multi_output

    if MAIN_RE.search(code):
        ok, output = run_block(binary, code)
        return ok, "full program", output

    synthesized = synthesize_main(code)
    if synthesized is not None:
        synth_ok, synth_output = run_block(binary, code + "\n" + synthesized)
        # A real main is now present, so this can't fail with R0001 -- any failure
        # here is the fragment's own code breaking under actual execution, which is
        # exactly what strategy 3 previously had no way to notice. Report it as a
        # real failure rather than quietly falling back to the weaker check below:
        # a wrong-precondition placeholder is a false positive this tool doesn't
        # yet have a way to silence per-parameter, but it's the same shape of
        # problem the existing `skip`/`expect-fail` markers already exist to
        # handle -- annotate the doc block, don't weaken the check.
        return synth_ok, "fragment, synthesized main", synth_output

    ok, output = run_block(binary, code)
    if ok or R0001_RE.search(output):
        return True, "fragment, no main", output

    wrapped_ok, wrapped_output = run_wrapped_expression(binary, code)
    if wrapped_ok:
        return True, "wrapped expression", wrapped_output
    return False, UNVERIFIABLE, output


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
        sys.exit("error: no .md/.mdx/.mtl files found under the given paths")

    passed_by_kind = Counter()
    skipped_by_kind = Counter()
    failed = 0
    failures = []

    for doc_path in doc_files:
        for start_line, code, marker_kind, marker_reason in extract_blocks(doc_path):
            if marker_kind == "skip":
                skipped_by_kind["annotated skip"] += 1
                continue

            ok, method, output = classify_block(binary, code)

            if marker_kind == "expect-fail":
                if ok:
                    failed += 1
                    reason = f" ({marker_reason})" if marker_reason else ""
                    failures.append((
                        doc_path, start_line,
                        f"expected this block to fail{reason}, but it verified as "
                        f"working ({method}) — the doc's claim about what's illegal, "
                        "or the bug being tracked, may no longer hold",
                    ))
                else:
                    passed_by_kind["expect-fail"] += 1
                continue

            if ok:
                passed_by_kind[method] += 1
            elif method == UNVERIFIABLE:
                skipped_by_kind[UNVERIFIABLE] += 1
            else:
                failed += 1
                failures.append((doc_path, start_line, output.strip()))

    for doc_path, start_line, output in failures:
        print(f"FAIL {doc_path}:{start_line}")
        for line in output.splitlines():
            print(f"    {line}")
        print()

    passed = sum(passed_by_kind.values())
    skipped = sum(skipped_by_kind.values())
    passed_detail = ", ".join(f"{n} {kind}" for kind, n in passed_by_kind.most_common())
    skipped_detail = ", ".join(f"{n} {kind}" for kind, n in skipped_by_kind.most_common())
    print(f"{passed} passed ({passed_detail}), {failed} failed, {skipped} skipped ({skipped_detail})")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
