#!/usr/bin/env python3
"""Emit deep_type.mtl at a given nesting depth (arg 2) to path (arg 1)."""
import pathlib
import sys

out = pathlib.Path(sys.argv[1])
n = int(sys.argv[2])
L = [
    f"// deep_type depth={n}",
    "fun wrap<T>(x: T) -> List<T> { List::from([x]) }",
    "",
    "fun main() -> i64 {",
    "    let v0 := 0i64;",
]
for i in range(1, n + 1):
    L.append(f"    let v{i} := wrap(v{i - 1});")
L.append(f"    (&v{n}).len()")
L.append("}")
out.write_text("\n".join(L) + "\n")
