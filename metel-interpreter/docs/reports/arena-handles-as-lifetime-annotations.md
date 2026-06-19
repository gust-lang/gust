# Arena Handles as Lifetime Annotations

*Design exploration — June 2026*

*Note: this file lives in `metel-interpreter/docs/reports/` because the `docs/`
submodule was unavailable at time of writing. It belongs in `docs/reports/` once the
submodule is accessible.*

This report follows on from the substructural and separation types report and the
per-field multiplicities report. Those reports established a reference capability
vocabulary (`*iso`, `*val`, `*mut`, `*T`, `*tag`) and a scoped arena API of the form:

```metel
Arena::scoped(fun(arena: &mut Arena) {
    let p: *iso[arena] Counter = arena.alloc(Counter { value: 0 });
});
```

The annotation `[arena]` on the pointer type marks `p` as arena-backed and therefore
non-sendable: it is tied to the lifetime of the `arena` handle. This report examines
what `[arena]` actually means as a language construct, how it compares to Rust's
abstract lifetime parameters, and what it implies for annotation inference.

---

## 1. The region annotation refers to a real object

In Rust, lifetime parameters are phantom variables:

```rust
fn init<'a>(arena: &'a mut bumpalo::Bump, n: i64) -> &'a Counter {
    arena.alloc(Counter { value: n })
}
```

`'a` has no runtime presence and no corresponding source object. The programmer must
mentally bind it to `arena` by reading the constraints.

In metel, `[arena]` in a pointer type refers to the arena handle variable `arena` that
is already visible in the same signature or scope:

```metel
fun init(arena: &mut Arena, n: i64) -> *iso[arena] Counter {
    arena.alloc(Counter { value: n })
}
```

The annotation is self-explanatory: the returned pointer lives as long as `arena` does.
Every `[x]` in a type annotation refers to a variable `x` the programmer can locate,
inspect, and reason about in the surrounding code.

---

## 2. Allocation examples

**Single scoped allocation — RAII without an explicit destructor call:**

```metel
let r = Region::new();
let p: *iso[r] Counter = r.alloc(Counter { value: 0 });
p.inc();
p.inc();
println(p.value);   // 2
// r dropped here — p freed automatically
```

The region `r` is the lifetime of `p`. When `r` goes out of scope, every `*iso[r] T`
pointer is freed. No destructor call, no `free(p)` — the region drop handles it.

**Arena::scoped as sugar for the common bounded-scope pattern:**

```metel
Arena::scoped(fun(arena: &mut Arena) {
    let a: *iso[arena] Counter = arena.alloc(Counter { value: 0 });
    let b: *iso[arena] Counter = arena.alloc(Counter { value: 0 });
    a.inc() || b.inc();   // CSC: [arena] objects are distinct — disjointness from identity
    println(a.value + b.value);   // 2
});
```

`Arena::scoped` creates a region, runs the closure, then drops the region at the
closure boundary. It is equivalent to creating a region `r`, using it, and dropping it,
but with a lexically obvious lifetime scope.

**Two distinct regions — CSC `||` approved from the tags alone:**

```metel
let r1 = Region::new();
let r2 = Region::new();
let a: *iso[r1] Counter = r1.alloc(Counter { value: 0 });
let b: *iso[r2] Counter = r2.alloc(Counter { value: 0 });

a.inc() || b.inc();   // [r1] ∩ [r2] = ∅ statically — no sep{} annotation needed
println(a.value + b.value);
```

Region tags serve double duty: they prevent pointers from escaping their scope and they
give the CSC checker a static proof of disjointness. Two pointers with different region
tags cannot alias, so parallel composition is approved without a runtime check and
without a `sep{}` annotation at the call site.

**Freeze — consuming `*iso[r]` to produce a sendable `*val`:**

```metel
let r = Region::new();
let cfg: *iso[r] Config = r.alloc(Config { workers: 4, debug: false });

let shared: *val Config = freeze(cfg);   // cfg consumed; [r] ends empty
spawn { worker_a(shared) };
spawn { worker_b(shared) };
// shared is *val — no region tag, globally immutable, freely sendable
```

`freeze` satisfies the linear obligation on `cfg` and returns `*val Config` with no
region annotation. The memory is now globally immutable and no longer tied to `r`.
When `r` is eventually dropped, there is nothing to free — the `freeze` already
transferred ownership.

**Linked structures — shared region enables inter-referencing:**

Pointers to different regions cannot reference each other, since storing `*iso[r2]
Node` inside an `*iso[r1]` struct mixes regions:

```metel
let r1 = Region::new();
let r2 = Region::new();
let a: *iso[r1] Node = r1.alloc(Node { val: 1, next: null });
let b: *iso[r2] Node = r2.alloc(Node { val: 2, next: null });
// a.next = b;   // ERROR: *iso[r2] stored inside *iso[r1] — regions differ
```

Arena::scoped assigns all allocations the same region, so nodes can freely reference
each other:

```metel
Arena::scoped(fun(arena: &mut Arena) {
    let b: *iso[arena] Node = arena.alloc(Node { val: 2, next: null });
    let a: *iso[arena] Node = arena.alloc(Node { val: 1, next: b });
    //              ↑ same region                              ↑ [arena] = [arena] ✓
    process_list(a);
});
```

The shared `[arena]` tag is what makes `a.next = b` type-check.

---

## 3. Implicit region polymorphism in functions

A function that takes an `*iso[r] T` parameter is automatically region-polymorphic.
The `[r]` in the parameter type refers to that parameter, and at each call site the
compiler substitutes the actual region of the argument:

```metel
fun summarise(n: *iso[n] Node) -> i64 {
    n.val   // n freed when function returns
}

let r1 = Region::new();
let r2 = Region::new();
let a: *iso[r1] Node = r1.alloc(Node { val: 10 });
let b: *iso[r2] Node = r2.alloc(Node { val: 32 });

summarise(a) + summarise(b);
// first call:  [n] instantiated to [r1]
// second call: [n] instantiated to [r2]
```

No explicit region type parameter (`<R>`) is written anywhere. The region annotation in
the signature refers to the parameter name, and instantiation is implicit at each call
site — the same way type inference works for ordinary generics.

Functions that allocate into a caller-supplied region thread the handle through the
signature:

```metel
fun build_node(arena: &mut Arena, val: i64) -> *iso[arena] Node {
    arena.alloc(Node { val, next: null })
}

Arena::scoped(fun(arena: &mut Arena) {
    let n: *iso[arena] Node = build_node(arena, 42);
    // n: *iso[arena] Node — tied to this arena's scope
});
```

At the call site the compiler unifies `build_node`'s `arena` parameter with the
caller's `arena` variable, giving `n` the correct region tag without any annotation at
the call site.

---

## 4. Does this improve the annotation inference story?

Yes, in ways that matter.

### 4.1 Inference from the allocation site

When you write `arena.alloc(Counter { value: 0 })`, the compiler already knows the
region — it is `arena`, the receiver of the call. The return type is `*iso[arena]
Counter` without any annotation at the call site. Region information flows the same
way type information flows: determined by the constructor, propagated by assignment.

Rust's lifetime inference instead solves a constraint system over abstract variables.
The borrow checker generates inequality constraints (`'a: 'b`) and finds minimal
lifetimes that satisfy them all. This works, but the abstract variables make error
messages hard to understand ("lifetime 'a does not outlive 'b").

With arena handles the escape check reduces to variable liveness: "is `arena` still in
scope at this point?" The compiler already performs this check for every variable.
Region checking becomes a specialisation of ordinary liveness analysis rather than a
separate abstract constraint-solving pass.

### 4.2 Function signatures are self-documenting

In Rust, an explicit lifetime annotation introduces a phantom variable whose meaning is
established only by its appearance in the constraints:

```rust
fn parse<'src, 'arena>(
    src: &'src str,
    arena: &'arena mut Bump,
) -> Ast<'arena> { ... }
```

The reader must mentally verify that `'arena` connects `arena` to `Ast<'arena>` and
that `'src` is separate. In metel, the connection is explicit in the annotation itself:

```metel
fun parse(src: *val str, arena: &mut Arena) -> *iso[arena] Ast {
    arena.alloc(build_ast(src))
}
```

`[arena]` in the return type points at the parameter `arena`. There is nothing to
mentally bind.

### 4.3 Error messages name real objects

When a region escapes its scope in Rust the error references abstract lifetime
variables:

```
error[E0597]: `data` does not live long enough
  = note: borrowed value must be valid for the lifetime 'a
```

With arena handles the error names the actual arena variable:

```
error: *iso[arena] value escapes the scope of `arena`
  --> src/parser.rs:42:5
   |
   |     Arena::scoped(fun(arena: &mut Arena) {
   |                        ----- `arena` defined here
   ...
   |         result   // ERROR: result: *iso[arena] Ast cannot escape this block
```

The programmer sees which arena is involved without consulting the lifetime constraint
graph.

### 4.4 Struct definitions — the remaining hard case

Rust requires lifetime parameters at the struct definition site because the definition
must be self-contained:

```rust
struct Parser<'a> { input: &'a str, pos: usize }
```

With arena handles, the region does not exist at definition time. One option is a
region type parameter at the definition site, analogous to Rust but shorter:

```metel
struct Parser[R] { input: *iso[R] str, pos: i64 }
// usage:
let p: Parser[arena] = Parser { input: arena.alloc(source), pos: 0 };
```

A more ambitious option is for region annotations to appear only at binding sites, with
the struct definition carrying no region parameters:

```metel
struct Parser { input: *iso str, pos: i64 }   // region elided in definition

let p: Parser[arena] = Parser { input: arena.alloc(source), pos: 0 };
// all *iso fields inferred to carry [arena]
```

The binding annotation `Parser[arena]` propagates to all `*iso` fields inside the
struct, and field accesses within the scope inherit `[arena]` without annotation. This
would require region inference across field accesses, making struct definitions
significantly cleaner than Rust's `struct Foo<'a, 'b>`. It is the more speculative of
the two options but points toward a genuinely better story.

### 4.5 Summary

| Property | Rust `'a` lifetimes | Arena handles `[arena]` |
|---|---|---|
| Annotation refers to | Abstract phantom variable | Real variable in scope |
| Inference mechanism | Abstract constraint solving | Liveness of named variable |
| Error messages | Abstract relations between `'a`, `'b` | Named arena out of scope |
| Function signatures | Must introduce `<'a>` parameter | Refers to existing parameter |
| Struct definitions | `struct Foo<'a>` required | Region parameter or binding-site annotation (TBD) |
| CSC disjointness | Separate sep{} annotation | Derived from distinct region tags |

The inference algorithm is not fundamentally simpler — escape analysis is escape
analysis. But the surface the programmer sees is anchored to real objects rather than
phantom variables, which means common cases require fewer explicit annotations, and the
cases that do require them are easier to write and read.

---

## 5. Relationship to the reference capability vocabulary

The `[r]` region annotation interacts with the capability vocabulary as follows:

| Capability | With region `[r]` | Effect |
|---|---|---|
| `*iso T` | `*iso[r] T` | Sendability removed; tied to scope of `r` |
| `*val T` | `*val[r] T` | Sendability removed; unusual — `*val` is normally global |
| `*mut T` | `*mut[r] T` | Redundant — `*mut T` is already non-sendable |
| `*T` | `*[r] T` | Redundant — `*T` is already non-sendable |
| `*tag T` | `*tag[r] T` | Sendability removed; limits utility of `*tag` |

The annotation is meaningful only for the two sendable capabilities: `*iso` and `*val`.
For non-sendable capabilities the annotation is harmless but unnecessary; the compiler
could warn or elide it.

The normal path for making arena-allocated data sendable is `freeze`: consume the
`*iso[r] T` within the scope of `r` and produce a `*val T` with no region tag. The
`*val` is globally immutable, freely copyable across fibers, and not tied to any arena.

---

## References

- Tofte, M., & Talpin, J.-P. (1997). Region-based memory management. *Information and
  Computation*, 132(2).
- Grossman, D. et al. (2002). Region-based memory management in Cyclone. *PLDI 2002*.
- Fluet, M., & Morrisett, G. (2006). Monadic regions. *ICFP 2006*.
- Birkedal, L. et al. (2006). A unifying approach to region-based memory management.
  *POPL 2006*.
- Weiss, A. et al. (2019). Oxide: the essence of Rust. *arXiv:1903.00982*.
- Levy, A. et al. (2017). Multiprogramming a 64 kB computer safely and efficiently
  (Tock OS). *SOSP 2017*. (Region-based memory in embedded Rust.)
