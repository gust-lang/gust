#!/usr/bin/env python3
"""Generate typechecker/solve stress fixtures for ctx.solve() profiling.

Each fixture is a valid, evaluable Metel program whose `main` returns i64, so
`metel-bench` can run it and report typecheck_detail (solve_ns, inference_ns,
solve_calls, constraints_processed).
"""
import pathlib
import sys

OUT = pathlib.Path(sys.argv[1])
OUT.mkdir(parents=True, exist_ok=True)

STORM_N = 250   # F1: sequential generic method calls
DEEP_N  = 90    # F2: generic-type nesting depth
CHAIN_N = 400   # F3: generic-instantiation chain length


def write(name: str, body: str) -> None:
    (OUT / name).write_text(body)
    print(f"  wrote {name} ({len(body)} bytes, {body.count(chr(10))} lines)")


# ── F1: solve_storm — many generic method-call dispatches ────────────────────
L = [
    "// STRESS: N sequential generic List method calls. Each .map/.filter is a",
    "// generic instantiation + closure inference + a receiver-dispatch ctx.solve().",
    "fun main() -> i64 {",
    "    let a0 := List::from([1i64, 2i64, 3i64, 4i64, 5i64]);",
]
for i in range(1, STORM_N + 1):
    p = f"a{i - 1}"
    if i % 3 == 0:
        L.append(f"    let a{i} := {p}.filter((x: i64) -> boolean {{ x >= 0i64 }});")
    elif i % 3 == 1:
        L.append(f"    let a{i} := {p}.map((x: i64) -> i64 {{ (x + 1i64) % 100i64 }});")
    else:
        L.append(f"    let a{i} := {p}.map((x: i64) -> i64 {{ (x * 2i64) % 97i64 }});")
L.append(f"    a{STORM_N}.fold(0i64, (acc: i64, x: i64) -> i64 {{ acc + x }})")
L.append("}")
write("solve_storm.mtl", "\n".join(L) + "\n")


# ── F2: deep_type — generic-type nesting depth ──────────────────────────────
L = [
    "// STRESS: a generic type nested DEEP_N levels. Every Substitution::apply /",
    "// unify touching the deepest binding recurses over List<List<...>>.",
    "fun wrap<T>(x: T) -> List<T> { List::from([x]) }",
    "",
    "fun main() -> i64 {",
    "    let v0 := 0i64;",
]
for i in range(1, DEEP_N + 1):
    L.append(f"    let v{i} := wrap(v{i - 1});")
L.append(f"    (&v{DEEP_N}).len()")
L.append("}")
write("deep_type.mtl", "\n".join(L) + "\n")


# ── F3: id_chain — long generic-instantiation chain ─────────────────────────
L = [
    "// STRESS: a CHAIN_N-long chain of generic identity calls. CHAIN_N fresh",
    "// instantiation vars + constraints; the final sum forces one solve+apply",
    "// that has to resolve a spread of the chain.",
    "fun id<T>(x: T) -> T { x }",
    "",
    "fun main() -> i64 {",
    "    let x0 := 1i64;",
]
for i in range(1, CHAIN_N + 1):
    L.append(f"    let x{i} := id(x{i - 1});")
picks = list(range(0, CHAIN_N + 1, max(1, CHAIN_N // 40)))
L.append("    " + " + ".join(f"x{i}" for i in picks))
L.append("}")
write("id_chain.mtl", "\n".join(L) + "\n")

print("done")
