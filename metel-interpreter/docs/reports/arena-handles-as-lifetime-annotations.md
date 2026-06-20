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
abstract lifetime parameters, how region relationships between multiple arenas are
expressed, and what the system implies for annotation inference.

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

`Arena::scoped` assigns all allocations the same region, so nodes can freely reference
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

No explicit region type parameter is written anywhere. The region annotation in the
signature refers to the parameter name, and instantiation is implicit at each call
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

This implicit form covers the common single-region case. When a function involves
multiple regions, or when a struct must hold pointers from a region it does not own,
explicit region parameters are needed.

---

## 4. Explicit region parameters and outlives relationships

### 4.1 The `[...]` region parameter clause

Region parameters are declared in a `[...]` clause following the type parameter list.
This keeps them visually distinct from value type parameters while using the same
bracket notation already used in pointer types and arena types:

```metel
// single region parameter
fun alloc_node<T>[R](arena: &mut Arena[R], val: T) -> *iso[R] T {
    arena.alloc(val)
}

// multiple region parameters
fun transfer<T>[Src, Dst](
    src: &mut Arena[Src],
    dst: &mut Arena[Dst],
    val: *iso[Src] T,
) -> *iso[Dst] T {
    dst.alloc(*val)
}
```

At call sites region parameters are inferred from the concrete arena handles passed —
they are never written explicitly, just as type parameters are not written when they
can be inferred.

### 4.2 Outlives as an aspect bound

When two regions must stand in an outlives relationship, the `Outlives` aspect expresses
the constraint in the `[...]` clause using the same bound syntax as type parameters:

```metel
aspect Outlives<R> {}

fun transfer<T>[Src, Dst: Outlives<Src>](
    src: &mut Arena[Src],
    dst: &mut Arena[Dst],
    val: *iso[Src] T,
) -> *iso[Dst] T {
    dst.alloc(*val)   // safe: Dst outlives Src — data copied into the longer-lived region
}
```

The compiler auto-generates `Outlives` impls from scope analysis: if `long` was created
before `short` in the program (and therefore lives longer), the compiler generates
`impl Outlives<short_region> for long_region`. Manual `impl` of `Outlives` is not
permitted — the compiler is the sole authority on region ordering.

Structs with multiple regions follow the same pattern:

```metel
struct Session[Req, Resp: Outlives<Req>] {
    request_data: *iso[Req] Bytes,
    response_buf: *iso[Resp] Bytes,
}

fun handle[Req, Resp: Outlives<Req>](
    req_arena:  &mut Arena[Req],
    resp_arena: &mut Arena[Resp],
    raw:        *val Bytes,
) -> *iso[Resp] Session[Req, Resp] {
    resp_arena.alloc(Session {
        request_data: req_arena.alloc(parse(raw)),
        response_buf: resp_arena.alloc(Bytes::empty()),
    })
}
```

At use sites:

```metel
let resp = Arena::new();
Arena::scoped(fun(req: &mut Arena) {
    // compiler sees: resp created before scoped block
    // → impl Outlives<req_region> for resp_region  ✓
    let session = handle(&mut req, &mut resp, raw);
    // Req = req's region, Resp = resp's region — inferred; Outlives check passes
    process(session);
});
// req freed; session is *iso[resp] — still valid
// resp freed when resp goes out of scope
```

Lexical nesting handles the common case automatically: if `resp` is in an outer scope
relative to `req`, the compiler derives the outlives relationship without any
annotation. Explicit `Outlives` bounds are needed only in function and struct signatures
where the regions arrive from outside and their relationship cannot be observed from
the local scope.

### 4.3 Sub-arenas with `Arena[R]`

A sub-arena is an arena whose region is bounded by a parent region. At declaration
sites `Arena[R]` names the bounding region abstractly; at use sites the bracket holds a
concrete handle:

```metel
// declaration: R is an abstract region parameter
fun make_sub[Parent, Child: Outlives<Parent>](parent: &mut Arena[Parent]) -> Arena[Child] {
    parent.sub()
}

// use: brackets hold a concrete arena handle
let outer = Arena::new();              // outer: Arena
let inner: Arena[outer] = outer.sub();   // inner's region is bounded by outer
// compiler generates: impl Outlives<inner_region> for outer_region  ✓
```

Allocating from `inner` produces `*iso[inner] T` pointers. These can reference data
from `outer` (because `outer` outlives `inner`) but not the other way around.

### 4.4 The `[...]` notation across all levels

`[...]` consistently means "region" throughout the syntax. The level of abstraction is
determined by context: concrete handle names at use sites, abstract parameters
(uppercase by convention) at declaration sites.

| Form | Level | Meaning |
|---|---|---|
| `*iso[R] T` | type annotation | pointer in abstract region R |
| `*iso[arena] T` | type annotation | pointer in concrete region `arena` |
| `Arena[R]` | type annotation | arena bounded by abstract region R |
| `Arena[outer]` | type annotation | arena bounded by concrete handle `outer` |
| `fun foo[R]` | declaration | declares abstract region parameter R |
| `struct Foo[R]` | declaration | struct parameterised over region R |

No separate syntax is introduced for each context. The same bracket notation works
across all three levels.

---

## 5. Does this improve the annotation inference story?

Yes, in ways that matter.

### 5.1 Inference from the allocation site

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

### 5.2 Function signatures are self-documenting

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
mentally bind. For the multi-region case the `[...]` clause makes the region parameters
explicit in one place:

```rust
// Rust — two phantom parameters, reader must infer their relationship
fn handle<'req, 'resp: 'req>(req: &'req mut Bump, resp: &'resp mut Bump) -> &'resp Session<'req>
```

```metel
// metel — region parameters and their relationship in one clause
fun handle[Req, Resp: Outlives<Req>](req: &mut Arena[Req], resp: &mut Arena[Resp]) -> *iso[Resp] Session[Req, Resp]
```

### 5.3 Error messages name real objects

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

### 5.4 Struct definitions

With explicit region parameters in a `[...]` clause, struct definitions are
self-contained without needing to mix region and type parameters into a single `<>`
list:

```metel
struct Parser[R] { input: *iso[R] str, pos: i64 }
// usage — R inferred from the arena at the construction site:
Arena::scoped(fun(arena: &mut Arena) {
    let p = Parser { input: arena.alloc(source), pos: 0 };
    // p: Parser[arena] — R inferred as arena's region
});
```

Compare Rust, where lifetime parameters must appear in the `<>` list alongside type
parameters and carry no information about which parameter they come from:

```rust
struct Parser<'a> { input: &'a str, pos: usize }
```

A more ambitious option — speculative — is for region annotations to appear only at
binding sites, with the struct definition carrying no region clause at all:

```metel
struct Parser { input: *iso str, pos: i64 }   // region elided in definition

let p: Parser[arena] = Parser { input: arena.alloc(source), pos: 0 };
// binding annotation propagates [arena] to all *iso fields
```

This would require region inference across field accesses. The `struct Foo[R]` form is
the practical starting point; binding-site-only annotation is a possible later
extension.

### 5.5 Summary

| Property | Rust `'a` lifetimes | Arena handles with `[...]` |
|---|---|---|
| Annotation refers to | Abstract phantom variable | Real variable in scope |
| Inference mechanism | Abstract constraint solving | Liveness of named variable |
| Error messages | Abstract relations between `'a`, `'b` | Named arena out of scope |
| Single-region functions | Must introduce `<'a>` parameter | Region named after parameter; implicit |
| Multi-region functions | `<'a, 'b: 'a>` in type param list | `[Src, Dst: Outlives<Src>]` in region clause |
| Struct definitions | `struct Foo<'a>` required | `struct Foo[R]` with region clause |
| Outlives relationship | `'a: 'b` special syntax | `Outlives<R>` aspect bound |
| CSC disjointness | Separate `sep{}` annotation | Derived from distinct region tags |

The inference algorithm is not fundamentally simpler — escape analysis is escape
analysis. But the surface the programmer sees is anchored to real objects rather than
phantom variables, which means common cases require fewer explicit annotations, and the
cases that do require them are easier to write and read.

---

## 6. Relationship to the reference capability vocabulary

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

## 7. Stdlib allocation types

The reference capability vocabulary provides the primitives; `Box<T>`, `Arc<T>`, and
`Rc<T>` are ordinary stdlib structs built on top with no compiler magic beyond the
`Drop` aspect and two special arenas.

### 7.1 The global and fiber-local heaps

Two arenas with permanent lifetimes serve as the backing store for standard allocation:

```metel
static      Heap:      Arena = Arena::heap();   // global — *iso[Heap] T is sendable
fiber_local LocalHeap: Arena = Arena::local();  // per-fiber — *iso[LocalHeap] T is not
```

`Heap` outlives all scopes and fibers, so the compiler treats `[Heap]` as a static
region and permits `*iso[Heap] T` to cross fiber boundaries. `LocalHeap` is tied to the
current fiber; its pointers cannot be sent.

### 7.2 Box, Arc, and Rc

```metel
aspect Drop { fun drop(self); }   // runtime calls drop(self) when an affine value is weakened

// ---- Box<T> — unique ownership, sendable ------------------------------------
struct Box<T> { ptr: *iso[Heap] T }

impl<T> Box<T> {
    fun new(val: T) -> Box<T>        { Box { ptr: Heap.alloc(val) } }
    fun get(&self) -> *T             { &*self.ptr }
    fun get_mut(&mut self) -> *mut T { &mut *self.ptr }
    fun into_inner(self) -> T        { *self.ptr }
    fun freeze(self) -> Arc<T>       { Arc { ptr: Heap.freeze(self.ptr) } }
}
impl<T> Drop for Box<T> {
    fun drop(self) { Heap.free(self.ptr); }        // T::drop (if any) + dealloc
}

// ---- Arc<T> — shared immutable, sendable, atomic refcount ------------------
struct Arc<T> { ptr: *val T }

impl<T> Arc<T> {
    fun new(val: T) -> Arc<T> { Box::new(val).freeze() }
    fun get(&self) -> *T      { &*self.ptr }
}
impl<T> Drop for Arc<T> {
    fun drop(self) { Heap.release(self.ptr); }     // atomic decrement; free at zero
}

// ---- Rc<T> — shared immutable, NOT sendable, non-atomic refcount -----------
struct Rc<T> { ptr: *val[LocalHeap] T }

impl<T> Rc<T> {
    fun new(val: T) -> Rc<T> {
        Rc { ptr: LocalHeap.freeze(LocalHeap.alloc(val)) }
    }
    fun get(&self) -> *T { &*self.ptr }
}
impl<T> Drop for Rc<T> {
    fun drop(self) { LocalHeap.release(self.ptr); } // non-atomic; always fiber-local
}
```

`Arc<T>` and `Rc<T>` are structurally identical. The arena determines sendability and
whether the refcount requires atomic operations:

| Type | Arena | Pointer | Sendable | Refcount |
|---|---|---|---|---|
| `Box<T>` | `Heap` | `*iso[Heap] T` | Yes | — |
| `Arc<T>` | `Heap` | `*val T` | Yes | atomic |
| `Rc<T>` | `LocalHeap` | `*val[LocalHeap] T` | No | non-atomic |

---

## 8. Memory safety: leaks and cycles

Two leak scenarios arise from the design above.

### 8.1 Leak 1 — raw `*iso[Heap] T` silently dropped

`Heap.alloc` returns `*iso[Heap] T`. That type is affine — it can be weakened without
calling `Heap.free`. If a caller discards a raw pointer without consuming it, the
allocation leaks:

```metel
fun leaky() {
    let p: *iso[Heap] Counter = Heap.alloc(Counter { value: 0 });
    // p weakened here — no Drop on *iso — Heap.free never called
}
```

Three coherent responses:

**Option A — implicit Drop on `*iso[Heap] T`**: the compiler treats every
`*iso[Heap] T` drop as a call to `Heap.free` (chaining into `T::drop` first). No user
action needed. For arena-backed `*iso[arena] T`, the implicit drop is a no-op — the
arena frees everything at region end. One compiler rule handles both cases.

**Option B — `Heap.alloc` returns `Box<T>` directly**: raw `*iso[Heap] T` is never
exposed outside the stdlib. `Heap.alloc` and `Box::new` are the same function. Users
can only produce `Box<T>`, `Arc<T>`, `Rc<T>` — all of which have `Drop`. This is the
same boundary Rust draws between raw pointers and `Box`.

**Option C — `*iso[Heap] T` is linear**: consuming the pointer is required; weakening
is a compile error. Forces explicit handling at every exit point. Too restrictive for
practical use but maximally leak-safe.

Option B is the practical default: the raw capability is an implementation detail of
the stdlib, not a user-visible type. Option A is a useful safety net for any unsafe
context where raw `*iso[Heap] T` appears.

### 8.2 Leak 2 — reference cycles in `Rc<T>` and `Arc<T>`

No refcount-based system detects cycles without additional machinery. If two `Rc<T>`
values reference each other, the count never reaches zero and the memory is never freed.

The standard fix is `Weak<T>` — a non-owning handle that does not increment the
refcount. In metel's capability vocabulary, `*tag T` (identity only, no read/write) maps
directly onto this role:

```metel
struct Weak<T>    { ptr: *tag[LocalHeap] T }  // non-owning; Rc-backed; not sendable
struct WeakArc<T> { ptr: *tag T }             // non-owning; Arc-backed; sendable

impl<T> Rc<T> {
    fun downgrade(&self) -> Weak<T> {
        Weak { ptr: tag_of(self.ptr) }   // identity only — no refcount increment
    }
}

impl<T> Weak<T> {
    fun upgrade(&self) -> Rc<T>? {
        LocalHeap.try_upgrade(self.ptr)  // Some(Rc) if still alive, else null
    }
}
```

Back-edges in graphs use `Weak<T>`; only forward-ownership edges use `Rc<T>`:

```metel
struct Node {
    val:    i64,
    parent: Weak<Node>,      // back-edge — does not prevent parent from being freed
    children: [Rc<Node>],   // forward-ownership edges
}
```

### 8.3 How other languages handle these problems

| Approach | Leak 1 | Leak 2 | Cost |
|---|---|---|---|
| Tracing GC (Java, Go, OCaml) | Solved | Solved | GC pauses; non-deterministic finalization |
| RAII + Drop (C++, Rust) | Solved via `Box`/`Drop` | Not solved; `Weak` is opt-in | None at runtime |
| ARC + weak/unowned (Swift) | Solved | Partially — programmer uses `weak` | None at runtime |
| RC + cycle collector (Python) | Solved | Solved via periodic scan | Scan overhead |
| Nim ORC | Solved | Solved via trial deletion | O(cycle size) per collection |
| Linear types (Idris 2, Linear Haskell) | Compile-time error | Structurally impossible | Expressivity |
| Explicit allocators (Zig) | Debug-mode detection | N/A | Programmer discipline |

**Nim ORC** is the most relevant point of comparison. ORC uses reference counting for
Leak 1 (the compiler inserts decrements at scope exit, equivalent to `Drop`) and adds a
lightweight cycle detector for Leak 2: when a refcount drops and the object might
participate in a cycle, ORC traces the local subgraph and collects any fully-internal
cycles. This is deterministic O(cycle size) work per collection rather than O(heap) as
in a full tracing GC.

Applying Nim's approach to metel's `*val` capability would mean the compiler inserts
refcount increments on every `*val T` copy and decrements on every drop — making `Rc<T>`
and `Arc<T>` thin ergonomic wrappers over language-level behaviour rather than
substantive abstractions. The `Drop` impl on each type would become unnecessary. The
constraint is that implicit RC cannot apply to `*iso T`, which must remain uniquely
owned; automatic reference counting on a unique pointer contradicts its semantics.

---

## 9. Advantages of region allocation

Region allocation is not merely a lifetime-annotation mechanism; it has concrete
performance and safety properties that per-object allocation does not.

**Allocation speed.** Arena allocation is a pointer bump: increment a counter and
return the old value. `Heap.alloc` (backed by `malloc`) maintains free lists, handles
fragmentation, and typically acquires a lock or uses per-thread bookkeeping. For
allocation-heavy workloads — parsing, tree building, request-scoped data — the
difference is 10–100×.

**O(1) bulk deallocation.** Freeing N individually-allocated objects requires N
`Heap.free` calls and N `Drop` invocations for the allocations themselves. Dropping an
arena resets one pointer. A compiler processing a 100,000-node AST and discarding it
pays one deallocation, not 100,000.

**No per-object overhead.** `Arc<T>` stores a refcount in every object and emits
atomic increment/decrement on every copy and drop. Arena objects carry no extra fields
and require no Drop implementation for the allocation itself — only for external
resources (file handles, sockets) the object may own.

**Cache locality.** Objects bump-allocated sequentially are physically adjacent.
Traversing a tree whose nodes all came from the same arena hits L1/L2 cache reliably.
Individual `Box<T>` allocations scatter objects across the heap.

**Structural sharing without lifetime complexity.** Every `*iso[arena] T` pointer in
the same arena shares one lifetime. Nodes can reference each other freely, including
cycles, because all are freed together when the arena drops — no cycle is a leak:

```metel
Arena::scoped(fun(a: &mut Arena) {
    let x = a.alloc(Node { val: 1, next: null });
    let y = a.alloc(Node { val: 2, next: x });
    x.next = y;   // cycle — irrelevant; both freed when arena drops
});
```

**Predictable latency.** Refcount cascades — where dropping one `Arc<T>` triggers
a chain of inner drops — can free thousands of objects in a single operation with
unpredictable timing. Dropping an arena is one bounded operation.

---

## 10. Coexistence with standard allocation

Region allocation and standard per-object allocation are not alternatives; they are
the same system at different lifetime granularities. `Heap` is simply an arena whose
region never ends.

**Borrows are region-agnostic.** Any function taking `&T` or `*T` works regardless of
where the `T` was allocated. The borrow carries no information about its source arena:

```metel
fun describe(node: &Node) -> i64 { node.val }

let h = Box::new(Node { val: 1 });
describe(h.get());                        // from Heap

Arena::scoped(fun(a: &mut Arena) {
    let n = a.alloc(Node { val: 2 });
    describe(n);                          // from scoped arena — same call, same function
});
```

**Region-polymorphic functions work with any arena, including `Heap`.** A function
declared with a `[R]` region parameter instantiates to `Heap`, `LocalHeap`, or any
scoped arena at the call site:

```metel
fun build_node[R](arena: &mut Arena[R], val: i64) -> *iso[R] Node {
    arena.alloc(Node { val, next: null })
}

let h = build_node(&mut Heap, 1);         // *iso[Heap] Node — equivalent to Box<Node>

Arena::scoped(fun(a: &mut Arena) {
    let n = build_node(a, 2);             // *iso[a] Node — arena-scoped
});
```

**Data can move between regions.** When arena-scoped data needs to outlive its arena,
it is either copied to the heap or frozen into a sendable `*val`:

```metel
Arena::scoped(fun(a: &mut Arena) {
    let temp: *iso[a] Config = a.alloc(Config { workers: 4 });

    let permanent = Box::new(*temp);      // copy to Heap — independent lifetime
    let shared: *val Config = freeze(temp); // freeze — globally immutable, sendable
    spawn { use_config(shared) };
});
```

**Most user code never sees a region annotation.** The `[R]` clause appears in stdlib
implementations and region-polymorphic library functions. Application code uses `Box<T>`,
`Arc<T>`, `Rc<T>`, and `Arena::scoped` — all of which hide the underlying `*iso[R] T`
behind a named type:

```metel
fun main() {
    let b = Box::new(Counter { value: 0 });      // no annotation visible
    let a = Arc::new(Config { workers: 4 });     // no annotation visible
    Arena::scoped(fun(arena: &mut Arena) {
        let node = arena.alloc(Node { val: 1 }); // type inferred
    });
}
```

The region system is additive: programs that never need regions use `Box`, `Arc`, and
`Rc` as in any other language. Programs that benefit from bulk allocation or shared
lifetimes opt into `Arena::scoped` and gain the performance and lifetime-simplicity
properties described in section 9.

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
- Boyapati, C. et al. (2003). Ownership types for safe region-based memory management
  in real-time Java. *PLDI 2003*.
- Ekblad, A., & Claessen, K. (2014). A splittable implicit heap memory manager.
  *Haskell Symposium 2014*. (Bulk allocation and region deallocation in Haskell.)
- Rądkiewicz, A. (2020). ORC — Nim's new garbage collector. *Nim blog*.
  (Trial-deletion cycle collection on top of reference counting.)
- Bernardy, J.-P. et al. (2018). Linear Haskell: practical linearity in a higher-order
  polymorphic language. *POPL 2018*. (Implicit RC via linear types.)
