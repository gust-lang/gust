# Substructural and Separation Types in Metel

*Design exploration — June 2026*

*Note: this file lives in `metel-interpreter/docs/reports/` because the `docs/`
submodule was unavailable at time of writing. It belongs in `docs/reports/` once the
submodule is accessible.*

This report surveys ideas from substructural type theory, uniqueness types, the Capture
Separation Calculus, and reference capabilities, and examines how each could be
incorporated into metel's type system and planned concurrency model.

The primary reference is Federico Bruzzone's "A Friendly Tour of Substructural,
Uniqueness, Ownership, and Capabilities Types — and more!" (2026), written in the
context of the Eter language project. The material draws on the same body of literature
that article covers: Girard's linear logic, Wadler's linear types, Clean's uniqueness
types, Marshall and Orchard's fractional uniqueness, the Capture Separation Calculus
(Xu et al. 2024), and Pony's reference capabilities.

**Current metel position.** Linear arenas as the primary memory-management mechanism
have been set aside. Linear types and owning pointers remain under consideration. The
broader ownership model is open, with owning pointers as one component among several.
This report treats all proposals as design options, not decisions.

---

## Table of contents

1. Background: the substructural hierarchy
2. Affine structs — move semantics as the foundation
3. Owning pointers: `own T`
4. Typestate via consuming receivers
5. Isolated references: `iso T`
6. Linear capability tokens
7. Degrees of separation: the Capture Separation Calculus
8. Integration with metel's planned concurrency model
9. Summary

---

## 1. Background: the substructural hierarchy

Classical type systems implicitly apply three structural rules to every variable in
scope:

| Rule | Meaning |
|---|---|
| Exchange | The order of variable declarations does not affect semantics |
| Weakening | A variable may be declared and never used |
| Contraction | A variable may be used any number of times (implicit copy) |

Substructural type systems restrict some of these rules to give types resource-like
behaviour. Dropping different rules yields different disciplines:

| System | Exchange | Weakening | Contraction | Allowed use count |
|---|---|---|---|---|
| Ordered | ✗ | ✗ | ✗ | Exactly once, in declaration order |
| Linear | ✓ | ✗ | ✗ | Exactly once |
| Affine | ✓ | ✓ | ✗ | At most once |
| Relevant | ✓ | ✗ | ✓ | At least once |
| Normal | ✓ | ✓ | ✓ | Unrestricted |

Affine types (at-most-once) are the most practical choice for a systems language: a
value can be dropped — its destructor runs (weakening is allowed) — but cannot be
duplicated without an explicit copy (contraction is forbidden). Rust is the canonical
example. Linear types (exactly-once) are stronger: the compiler rejects not just
duplication but also silent dropping, which is the right model for values that require
an explicit terminal action.

The sections below work through each concept from least to most invasive: from affine
move semantics (which requires no new syntax beyond a `Copy` aspect) to linear
capability tokens and degrees of separation.

---

## 2. Affine structs — move semantics as the foundation

The simplest ownership addition: make struct types *affine by default*. An affine type
can be dropped but cannot be implicitly copied. To opt into duplication, a type
explicitly implements `Copy`.

This is the foundation of the whole ownership model. Without move semantics there is no
ownership to speak of — you cannot transfer a value safely if copying it is always free.

**Proposed behaviour.** Struct types are affine unless they implement `Copy`. Assigning
an affine value to a new binding *moves* it: the original binding becomes inaccessible.
Passing an affine value to a function by value moves it into the callee.

```metel
struct Buffer {
    data: [u8],
    len:  i64,
}

impl Buffer {
    fun drain(self) -> [u8] { self.data }   // consuming receiver — Buffer gone after this
}

fun compress(buf: Buffer) -> Buffer { buf }

fun main() {
    let buf = Buffer { data: [1u8, 2u8, 3u8], len: 3 };
    let compressed = compress(buf);   // buf MOVES into compress
    // buf.drain();                   // ERROR: buf was moved
    let data = compressed.drain();
}
```

Opt into contraction for a specific type with a `Copy` aspect:

```metel
aspect Copy {}

struct Point { x: f64, y: f64 }
impl Copy for Point {}

fun main() {
    let p = Point { x: 1.0, y: 2.0 };
    let q = p;   // COPY — p still accessible because Point: Copy
    let r = p;
}
```

**What needs to change.** The evaluator currently uses `Rc<RefCell<Value>>` throughout,
giving shared-reference semantics. Move semantics requires tracking binding liveness and
rejecting uses of moved bindings. Adding a liveness map to `InferContext` would let the
type checker enforce the rule before the evaluator sees the code.

**What already works.** Consuming (`self`) receivers already express the concept at the
API level — `Buffer.drain(self)` is correctly typed as a move. The runtime behaviour
(actually preventing re-use) requires the evaluator change.

---

## 3. Owning pointers: `own T`

An owning pointer `own T` is a heap-allocated value with a unique owner. It differs
from existing metel reference types:

- `*T` and `*mut T` are *borrows* — non-owning views of a value that lives elsewhere.
  They do not free their target when dropped.
- `own T` *is* the owner — dropping it frees the heap cell.

The uniqueness guarantee is inherent: at any moment, exactly one `own T` exists for a
given heap cell. This makes in-place mutation always safe — no aliasing hazard — and
enables recursive data structures without a garbage collector.

**Syntax.** `own T` as a type expression; `own expr` as an allocation expression.

```metel
enum List<T> {
    Cons { head: T, tail: own List<T> },   // own enables the recursive field
    Nil  {},
}

impl<T> List<T> {
    fun empty() -> own List<T> {
        own List::Nil {}
    }

    fun prepend(self: own List<T>, val: T) -> own List<T> {
        own List::Cons { head: val, tail: own self }
    }

    fun len(&self) -> i64 {
        match self {
            List::Cons { head: _, tail } => 1 + tail.len(),
            List::Nil {}                 => 0,
        }
    }
}

fun main() {
    let list = List::empty<i64>().prepend(3).prepend(2).prepend(1);
    assert(list.len() == 3);
    // list freed here — all nodes released deterministically, depth-first
}
```

The `own` field in `Cons` is what makes the recursive definition valid: it stores a
pointer to a heap node rather than embedding a `List<T>` inline (infinite size).

**In-place mutation.** Because the caller holds the only reference, it can destructure
and rebuild without allocating new nodes. This is Wadler's result from "Linear types
can change the world!" (1990): a uniquely-owned value can be updated in place without
breaking referential transparency because no other reference can observe the mutation.

```metel
fun map_inplace(list: own List<i64>, f: (i64) -> i64) -> own List<i64> {
    match list {
        List::Nil {}                  => own List::Nil {},
        List::Cons { mut head, tail } => own List::Cons {
            head: f(head),
            tail: map_inplace(tail, f),
        },
    }
}
```

**What needs to change.** A heap allocator path in the evaluator; `own T` as a new
`Type` and `InferType` variant; `own expr` in the grammar and parser; destructor
registration at binding time so the heap cell is freed when the binding goes out of
scope.

---

## 4. Typestate via consuming receivers

Typestate encodes a value's *protocol state* in its type and uses the type checker to
enforce that operations are called in the correct order. A `File<Closed>` cannot be
read; a `File<Open>` cannot be opened again. The compiler enforces the protocol
statically, with no runtime overhead.

**This is already expressible in metel.** No new features are required for simple
protocols.

### Simple form: distinct struct types per state

The consuming (`self`) receiver is the key. A method that transitions state takes
`self` by value — consuming the old type — and returns a value of a new, distinct type.
Once a `TcpConn` is passed to `disconnect`, the compiler knows that binding is gone.

```metel
struct TcpInit   { addr: String }
struct TcpConn   { fd: i64 }
struct TcpClosed {}

impl TcpInit {
    fun connect(self) -> TcpConn {
        TcpConn { fd: sys_connect(self.addr) }
    }
}

impl TcpConn {
    fun send(&mut self, data: [u8]) { sys_send(self.fd, data); }
    fun recv(&self) -> [u8]         { sys_recv(self.fd) }

    fun disconnect(self) -> TcpClosed {   // TcpConn consumed here
        sys_close(self.fd);
        TcpClosed {}
    }
}

// TcpClosed has no methods — a closed connection is statically inert

fun roundtrip(addr: String, msg: [u8]) -> [u8] {
    let mut conn = TcpInit { addr: addr }.connect();
    conn.send(msg);
    let reply = conn.recv();
    let _done = conn.disconnect();
    // conn.send(msg);   // ERROR: conn was moved into disconnect
    reply
}
```

### Generic form: phantom state parameter

For protocols with many states, a generic state parameter avoids proliferating struct
definitions. The parameter carries no runtime data — it is a type-level tag only.

**Proposed addition.** A `phantom` keyword for zero-size struct fields that carry a type
argument without occupying space at runtime. This is equivalent to Rust's `PhantomData`.

```metel
struct Socket<State> {
    fd:     i64,
    _state: phantom State,   // zero size; erased at runtime
}

struct Listening {}
struct Accepting {}
struct Closed    {}

impl Socket<Listening> {
    fun bind(addr: String) -> Socket<Listening> {
        Socket { fd: sys_bind(addr), _state: phantom }
    }

    fun accept(self) -> Socket<Accepting> {
        Socket { fd: self.fd, _state: phantom }
    }
}

impl Socket<Accepting> {
    fun read(&self) -> [u8]          { sys_read(self.fd) }
    fun write(&mut self, d: [u8])    { sys_write(self.fd, d); }

    fun close(self) -> Socket<Closed> {
        sys_close(self.fd);
        Socket { fd: -1, _state: phantom }
    }
}

// Socket<Closed> has no methods
```

The phantom parameter participates in type unification normally:
`Socket<Listening>` and `Socket<Accepting>` are distinct types.

**Typestate pairs naturally with affine and linear types.** Typestate enforces the
*sequence* of operations; affine or linear types enforce that the sequence is
*completed*. A linear socket cannot be silently dropped (no unclosed connection) and
cannot be duplicated (no double-close).

---

## 5. Isolated references: `iso T`

An isolated reference `iso T` is a qualifier meaning "I am the only live reference to
this value at this moment." This is distinct from `own T`, which implies heap
allocation. `iso T` is a claim about *aliasing* rather than *allocation*.

The key asymmetry from uniqueness type theory (Barendsen and Smetsers 1996, Clean;
Marshall and Orchard 2024, Granule): isolation can be *forgotten* at any time —
downgraded to a shared view — but cannot be *reconstructed* for a value that has
already been shared. This one-way direction is what gives the guarantee meaning.

```metel
struct Image { pixels: [u32], width: i64, height: i64 }

// iso: the sole live reference — safe to mutate, safe to transfer to another fiber
fun process_on_worker(img: iso Image) { /* can mutate img freely */ }

fun pixel_width(img: *Image) -> i64 { img.width }

fun main() {
    let img: iso Image = iso Image {
        pixels: [0u32; 1920 * 1080],
        width:  1920,
        height: 1080,
    };

    // Temporarily downgrade iso → * for a shared read borrow
    let w = pixel_width(&img);

    // Borrow expired — img is iso again; safe to transfer
    process_on_worker(img);
    // pixel_width(&img);   // ERROR: img was moved into process_on_worker
}
```

**Fractional ownership interpretation.** The `iso` qualifier corresponds to holding the
full ownership fraction (p = 1) from Marshall and Orchard's fractional uniqueness model.
A shared borrow `*T` is a partial fraction (p ∈ (0, 1]). The invariant is that active
fractions never sum above 1:

```
iso T   :  p = 1  — the only reference; exclusive write
*mut T  :  p = 1 temporarily — mutable borrow (full fraction, on loan)
*T      :  p ∈ (0, 1) — shared borrow; many can coexist; sum ≤ 1

Invariant: Σ active fractions ≤ 1
```

Downgrading `iso → *T` splits the full fraction into a read share. When all borrows
expire, the full fraction is reconstituted and `iso` status is restored.

**Relation to `own T`.** Both carry p = 1, but model different things:
- `own T`: you allocated the heap cell; you free it on drop.
- `iso T`: you hold the only reference; the allocation may live elsewhere.

An `own T` implies `iso T` — you both own and are the sole reference. An `iso T`
obtained without allocating (e.g., a unique slice of a larger array) cannot
independently free its target.

---

## 6. Linear capability tokens

The preceding sections restrict *values*. Linear capability tokens separate the
*address* of a resource from the *permission to use it*, making the permission a
first-class linear value that can be threaded through a program independently.

The idea originates in the calculus of capabilities (Crary, Walker, Morrisett 1999) and
alias types (Smith, Walker, Morrisett 2000). A pointer is a freely-copyable address; a
capability token is a linear value that authorises access to the address it names. Many
aliases to the address may exist; the capability is scarce.

**The critical distinction: affine vs. linear.**

- **Affine** (`own T`): the value *may* be dropped silently — its destructor runs. Good
  for memory: freeing at scope exit is a safe default.
- **Linear** (`linear struct`): the value *must* be explicitly consumed — the compiler
  rejects it falling out of scope unconsumed. Good for protocols where a silent drop is
  a bug: an unclosed connection, an uncommitted transaction, an unsent acknowledgement.

```metel
// FileHandle: the address — freely copyable, harmless without a cap
struct FileHandle { fd: i64 }

// FileCap: the permission — LINEAR
// no contraction: can't duplicate → no double-close
// no weakening:   can't drop silently → no resource leak
linear struct FileCap { fd: i64 }

fun open(path: String) -> (FileHandle, linear FileCap) {
    let fd = sys_open(path);
    (FileHandle { fd: fd }, FileCap { fd: fd })
}

// Every operation returns the cap — it stays alive through the chain
fun read(h: FileHandle, cap: linear FileCap) -> (String, linear FileCap) {
    (sys_read(h.fd), cap)
}

fun write(h: FileHandle, data: String, cap: linear FileCap) -> linear FileCap {
    sys_write(h.fd, data);
    cap
}

// The only valid way to consume the cap
fun close(h: FileHandle, cap: linear FileCap) {
    sys_close(h.fd);
    // cap consumed here — linearity satisfied
}

fun main() {
    let (h, cap) = open("/tmp/log.txt");

    let h2          = h;                      // COPY — handle is freely duplicable
    let (line, cap) = read(h, cap);
    let cap         = write(h2, line, cap);
    close(h2, cap);

    // Forgetting close is a compile error:
    // let (h3, cap3) = open("/tmp/other.txt");
    // }   ← ERROR: cap3 is linear — exits scope unconsumed
}
```

**Ordered protocol enforcement.** Capability tokens with distinct types at each protocol
stage enforce step ordering without a runtime state machine:

```metel
// HTTP response: write_status → write_header* → end_headers → write_body* → finish

linear struct StatusPhase {}
linear struct HeaderPhase {}
linear struct BodyPhase   {}

fun write_status(conn: FileHandle, code: i64,   _: linear StatusPhase) -> linear HeaderPhase { ... }
fun write_header(conn: FileHandle, k: String,
                 v: String,                      cap: linear HeaderPhase) -> linear HeaderPhase { ... }
fun end_headers (conn: FileHandle,              _: linear HeaderPhase)   -> linear BodyPhase   { ... }
fun write_body  (conn: FileHandle, data: [u8],  cap: linear BodyPhase)   -> linear BodyPhase   { ... }
fun finish      (conn: FileHandle,              _: linear BodyPhase)                           { ... }
```

Writing the body before the headers is a type error: `write_body` requires a
`linear BodyPhase`, which `end_headers` has not yet produced.

**What needs to change.** A `linear` keyword on struct declarations; a linearity
checker pass (separate from the affine move checker) that rejects unconsumed linear
bindings at scope exit.

---

## 7. Degrees of separation: the Capture Separation Calculus

The previous sections all restrict what can be expressed or require reorganising code
around unique ownership. The Capture Separation Calculus (CSC — Xu, Boruch-Gruszecki,
Odersky 2024) takes the opposite stance: aliases to mutable state are permitted freely,
and separation is enforced only at the point where parallelism is introduced.

This makes CSC far less invasive than ownership or linear types. Existing sequential
code requires no changes. The type checker only activates when the programmer writes
`e₁ || e₂`.

### 7.1 Capture sets

A capture set is the set of *root variables* a reference transitively reaches. It is
computed alongside the type during inference, stored as a side channel rather than
embedded in the `Type` enum.

**Inference rules:**

| Expression | Capture set |
|---|---|
| Struct literal `T { .. }` | `{}` — a root value, not a reference |
| `let x = T { .. }` | binds root variable `x`; `cap(x) = {x}` |
| `&y`, `&mut y` | `{y}` |
| `&y.field` | `cap(y)` — propagate through field access |
| `*p` where `p: *T` | `cap(p)` — propagate through deref |
| Closure capturing `x, y` | `cap(x) ∪ cap(y)` |
| `f(a, b)` returning `*T` | `cap(a) ∪ cap(b)` (conservative) |
| `(e₁, e₂)` | `cap(e₁) ∪ cap(e₂)` |

```metel
fun main() {
    let mut a = Counter { value: 0 };   // cap(a) = {a}
    let mut b = Counter { value: 0 };   // cap(b) = {b}

    let pa: *mut Counter = &mut a;      // cap(pa) = {a}
    let pb: *mut Counter = &mut b;      // cap(pb) = {b}

    // cap(pa) ∩ cap(pb) = {a} ∩ {b} = ∅ — disjoint, safe for parallel use
}
```

**Implementation.** Add one field to `InferContext` in `typeinference/mod.rs`:

```rust
capture_env: Vec<HashMap<String, HashSet<String>>>,
```

Maintained alongside `mono_env`: pushed and popped with `push_scope` / `pop_scope`,
written by an extended `bind_mono`, read by `lookup_capture`. No changes to the `Type`
or `InferType` enums are required.

### 7.2 Parallel composition: `||`

`e₁ || e₂` runs both expressions concurrently. The checker verifies that their
write-accessible capture sets are disjoint before allowing the expression.

**Safety rules:**

```
write_cap(e₁) ∩ write_cap(e₂) = ∅   — no write–write race
write_cap(e₁) ∩ read_cap(e₂)  = ∅   — no write–read race
write_cap(e₂) ∩ read_cap(e₁)  = ∅   — no read–write race
```

Reader ∥ reader is unconditionally safe: any number of concurrent reads to the same
variable are allowed. The existing `*T` vs `*mut T` distinction directly encodes
the reader/writer distinction — no new types are needed.

```metel
struct Counter { value: i64 }

impl Counter {
    fun inc(&mut self) { self.value += 1; }
    fun get(&self) -> i64 { self.value }
}

fun main() {
    let mut a = Counter { value: 0 };
    let mut b = Counter { value: 0 };

    // cap(a.inc()) = {a}, cap(b.inc()) = {b} — disjoint: OK
    a.inc() || b.inc();
    assert(a.value == 1 && b.value == 1);

    // reader || reader — always OK
    let _ = a.get() || a.get();

    // write–read race — ERROR: write_cap({a}) ∩ read_cap({a}) ≠ ∅
    // a.inc() || a.get();

    // write–write race — ERROR: write_cap({a}) ∩ write_cap({a}) ≠ ∅
    // a.inc() || a.inc();

    // Sequential — no check needed, not parallel
    let v = a.get();
    a.inc();
}
```

**New AST nodes** (`ast/mod.rs` and `typed_ast/mod.rs`):

```rust
// In Expr:
Parallel(Box<Expr>, Box<Expr>, Span),

// In TypedExpr:
Parallel { left: Box<TypedExpr>, right: Box<TypedExpr>, ty: Type, span: Span },
```

**Inference rule** added to `infer_expr` in `typechecker/inference.rs`:

```rust
Expr::Parallel(left, right, span) => {
    let left_ty  = infer_expr(left,  ctx, fun_generalizations)?;
    let right_ty = infer_expr(right, ctx, fun_generalizations)?;

    let lw = ctx.write_capture_of(left);
    let rw = ctx.write_capture_of(right);
    let lr = ctx.read_capture_of(left);
    let rr = ctx.read_capture_of(right);

    if let Some(v) = lw.intersection(&rw).next() {
        return Err(parallel_write_write_error(v, *span));
    }
    if let Some(v) = lw.intersection(&rr).next()
        .or_else(|| rw.intersection(&lr).next())
    {
        return Err(parallel_write_read_error(v, *span));
    }

    Ok(InferType::Tuple(vec![left_ty, right_ty]))
}
```

**Evaluator.** The evaluator can initially execute both sides sequentially. The safety
guarantee is already established at compile time; actual thread-level parallel execution
can be added later independently.

### 7.3 Separation annotations: `sep{}`

When `||` is *inside* a function body, the checker cannot derive capture-set
disjointness from the call-site arguments without inspecting the function internals.
`sep{}` annotations are the mechanism by which callers provide a proof at the call site
and the function body trusts it.

```metel
// sep{a} on b declares: at the call site, cap(arg for b) must be disjoint from cap(arg for a)
fun parallel_inc(a: *mut Counter, sep{a} b: *mut Counter) {
    a.inc() || b.inc();   // safe inside: sep{a} is the axiom
}

fun main() {
    let mut x = Counter { value: 0 };
    let mut y = Counter { value: 0 };

    parallel_inc(&mut x, &mut y);    // cap = {x} vs {y} — disjoint: OK
    // parallel_inc(&mut x, &mut x); // ERROR: cap = {x} on both — sep{a} violated
}
```

**Grammar addition:**

```rust
// In ast/mod.rs:
pub enum Param {
    Plain     { name: String, ty: TypeExpr, span: Span },
    Separated { sep_from: Vec<String>, name: String, ty: TypeExpr, span: Span },
}
```

**Call-site checking.** After unifying argument types in the `Call` / `MethodCall`
inference arm, for each `Separated` parameter, look up the capture sets of the
relevant arguments and verify disjointness.

**Inside the function body.** `sep{a}` seeds a `separation_facts: Vec<(String, String)>`
in `InferContext`. The `||` rule consults these facts and treats the declared
disjointness as an axiom rather than re-deriving it from the call-site arguments,
which are not in scope inside the body.

---

## 8. Integration with metel's planned concurrency model

Metel's planned concurrency system: lightweight fibers via `spawn { }`, M:N scheduled
(no async/await, no function colouring), typed channels `Chan<T>` as the primary
communication primitive with `ch <- val` (send) and `<- ch` (receive), and a `select`
expression for multiplexing.

The ownership and separation mechanisms from the preceding sections map onto two
distinct levels of parallelism in this model.

### 8.1 Two levels of parallelism

**Fiber-level (coarse).** Fibers communicate through channels. Safety comes from
ownership transfer: sending `own T` or `iso T` into a channel strips access from the
sender and grants it to the receiver. No shared mutable state crosses fiber boundaries.
The CSC machinery does not activate at this level.

**Intra-fiber (fine).** Once a fiber owns its data, it may split work across threads
using `||`. The two sides share access to the same allocations but have provably
disjoint write domains. This is where capture-set disjointness is checked.

```
Fiber boundary:   own T / iso T → Chan<T> → own T / iso T
                  ownership transferred; sender loses access

Intra-fiber:      own T → split_at_mut → (iso A, iso B)
                  A and B are disjoint; safe for ||
```

### 8.2 Fibers and channels: ownership transfer

`spawn { }` captures variables from the enclosing scope. For the spawn to be safe,
captured variables must satisfy one of the following:

| Captured type | Semantics | Parent access after spawn |
|---|---|---|
| `own T` | Moved into fiber | Lost — parent cannot use it |
| `iso T` | Unique reference moved into fiber | Lost — parent cannot use it |
| `T: Copy` | Copied into fiber | Retained (immutable copy in fiber) |
| `Chan<T>` endpoint | Endpoint moved into fiber | Lost — each end owned by one fiber |

```metel
fun producer_consumer() {
    let (tx, rx): (SendChan<own [i64]>, RecvChan<own [i64]>) = Chan::new();

    let producer = spawn {
        // tx captured by move; rx not captured — unavailable here
        let data: own [i64] = [1, 2, 3, 4, 5, 6, 7, 8];
        tx <- data;         // data MOVED into channel — producer loses it
        // data[0]          // ERROR: data was sent
    };

    let consumer = spawn {
        // rx captured by move
        let received: own [i64] = <- rx;   // consumer acquires ownership
        let sum = received.iter().fold(0i64, |a, x| a + x);
        assert(sum == 36);
    };

    producer.join();
    consumer.join();
}
```

Channel endpoints are directional: `SendChan<T>` and `RecvChan<T>` are distinct types.
A fiber holding only `RecvChan<T>` cannot send; a fiber holding only `SendChan<T>`
cannot receive. Each endpoint is an `own` value — one fiber owns each side.

`Chan<linear T>` directly satisfies the linear type's exactly-once rule: the send is
the single consumption. This is the "channels are the natural transport for linear
values" property: a `linear FileCap` can be sent through a channel and the compiler
confirms it was not used on the sending side after the send.

### 8.3 `||` within a fiber: fine-grained parallelism

After acquiring `own [i64]` from a channel, a fiber may want to process independent
halves concurrently. The `||` expression handles this without spawning new fibers:

```metel
fun parallel_sum(data: own [i64]) -> i64 {
    let mid = data.len() / 2;

    // Both sides read via *[i64] — reader || reader is always safe
    let (left_sum, right_sum) =
        sum_slice(&data, 0, mid) || sum_slice(&data, mid, data.len());

    left_sum + right_sum
}
```

For mutation, both sides must touch provably disjoint subsets. The key primitive is
`split_at_mut`, which consumes the `own [i64]` and produces two `iso` halves with
*distinct* root variables in the capture environment:

```metel
fun parallel_transform(data: own [i64]) -> own [i64] {
    let mid = data.len() / 2;

    // split_at_mut consumes own [i64], produces (iso [i64], iso [i64])
    // cap(left) = {left}, cap(right) = {right} — distinct root variables
    let (left, right) = data.split_at_mut(mid);

    // write_cap(left side) = {left}, write_cap(right side) = {right}
    // {left} ∩ {right} = ∅ — CSC check passes
    let (l, r) = transform_half(left) || transform_half(right);

    join_halves(l, r)   // reassemble own [i64] from the two iso halves
}
```

`split_at_mut` and `join_halves` are the splitting and recombination operations from
fractional uniqueness theory: the original unique reference is divided into two
fractions, used in parallel under the CSC disjointness check, then recombined.

### 8.4 `spawn` with CSC disjointness

A more liberal alternative to the ownership-only capture rule: `spawn` may capture
`*mut T` references provided the spawned fiber's write-capture set is provably disjoint
from the parent's write-capture set at the spawn point.

```metel
fun update_partitions(mut part_a: [i64], mut part_b: [i64]) {
    // Fiber's write-capture: {part_b}
    // Parent's write-capture after spawn: {part_a}
    // {part_a} ∩ {part_b} = ∅ — safe
    let fiber = spawn {
        for (let mut x in part_b) { x *= 2; }
    };

    for (let mut x in part_a) { x *= 2; }

    fiber.join();   // synchronisation point; both capture sets merged
}
```

The checker at `spawn { body }` computes the write-capture set of `body` and verifies
it is disjoint from the write-capture set of the code between `spawn` and the
corresponding `join`. The `join` is the synchronisation boundary: after it, all capture
sets merge and normal sequential rules resume.

For functions called from inside the spawned fiber, `sep{}` propagates the proof
through the call boundary:

```metel
fun transform_pipeline(input: *[i64], sep{input} out: *mut [i64]) {
    for (let i in 0..input.len()) {
        out[i] = transform(input[i]);
    }
}

fun main() {
    let src:     [i64] = [1, 2, 3, 4];
    let mut dst: [i64] = [0, 0, 0, 0];

    // cap(src) = {src}, cap(dst) = {dst} — disjoint: sep{input} satisfied
    transform_pipeline(&src, &mut dst);

    // cap(src) = cap(src) — same: ERROR — sep{input} violated
    // transform_pipeline(&src, &mut src);
}
```

### 8.5 `select`

`select` is sequentially exclusive — at most one arm fires — so arms do not race with
each other and require no CSC checking between them. Each arm receives an owned value
from a channel and handles it with normal sequential semantics:

```metel
select {
    msg = <- ch_work => { process(msg); },   // msg: own WorkItem
    _   = <- ch_done => { break; },
}
```

Any concern about what a fired arm does relative to concurrently running fibers is
handled by the fiber-level ownership rule or the spawn-point disjointness check, not
by `select` itself.

### 8.6 The full picture

```
spawn { } + Chan<T>                        —  fiber boundary (coarse)
  own / iso / Copy / Chan captured             no shared mutable state across fibers
  OR: write-capture sets disjoint at spawn     (liberal CSC rule)

       ↓ each fiber owns its data

split_at_mut / join_halves                 —  splitting within a fiber
  own T → (iso A, iso B)                      cap(A) and cap(B) disjoint by construction

       ↓ two iso halves, provably separate

e₁ || e₂                                  —  fine-grained parallelism
  write_cap(e₁) ∩ write_cap(e₂) = ∅          CSC disjointness check at compile time
  reader || reader always permitted

sep{} annotations                          —  proof propagation through call boundaries
  verified at call sites                       allows || inside library function bodies
```

---

## 9. Summary

The six mechanisms are not alternatives — they address different granularities of the
same resource-safety problem and compose naturally with each other.

| Mechanism | Primary guarantee | Notes |
|---|---|---|
| Affine structs | No duplication; safe drop | Foundation; needed by all the rest |
| `own T` | Unique heap owner; deterministic free | The "owning pointer" component |
| Typestate | Protocol step ordering enforced statically | Works today with consuming receivers |
| `iso T` | No live aliases; safe transfer | `own T` implies `iso T`; bridges to CSC |
| `linear struct` | Explicit terminal action required | Stronger than affine; for protocol caps |
| CSC (`\|\|`, `sep{}`) | Parallel access to disjoint state | Works on top of existing `*T`/`*mut T` |

The split between concurrency levels is clean:

- **Channels + ownership** for inter-fiber communication: values move across fiber
  boundaries without sharing.
- **`||` + `sep{}`** for intra-fiber parallelism: one fiber owns a dataset, splits it
  into disjoint parts, processes them in parallel.
- **`spawn` + CSC disjointness** as a more liberal alternative to the ownership-only
  spawn rule: allows shared mutable state across the spawn point when write domains are
  provably separate.

The ordering from least to most invasive: affine move semantics touches the evaluator
and borrow-liveness tracking in `InferContext`; `own T` adds a heap allocator path;
typestate and `iso T` are type-system additions with no runtime cost; `linear struct`
adds a linearity checker pass; CSC adds capture-set tracking to `InferContext` plus the
`Parallel` expression and `sep{}` grammar, all building on the existing `*T` / `*mut T`
reference types.

---

## References

- Girard, J.-Y. (1987). Linear logic. *Theoretical Computer Science*, 50(1), 1–101.
- Wadler, P. (1990). Linear types can change the world! *IFIP TC 2 Working Conference*.
- Barendsen, E., & Smetsers, S. (1996). Uniqueness typing for functional languages with
  graph rewriting semantics. *Mathematical Structures in Computer Science*, 6(6).
- Crary, K., Walker, D., & Morrisett, G. (1999). Typed memory management in a calculus
  of capabilities. *POPL 1999*.
- Smith, F., Walker, D., & Morrisett, J. G. (2000). Alias types. *ESOP 2000*.
- Haller, P., & Odersky, M. (2010). Capabilities for uniqueness and borrowing.
  *ECOOP 2010*.
- Bernardy, J.-P. et al. (2018). Linear Haskell: practical linearity in a higher-order
  polymorphic language. *POPL 2018*.
- Marshall, D., & Orchard, D. (2024). Functional ownership through fractional
  uniqueness. *OOPSLA 2024*.
- Bao, Y. et al. (2021). Reachability types: tracking aliasing and separation in
  higher-order functional programs. *OOPSLA 2021*.
- Xu, Y., Boruch-Gruszecki, A., & Odersky, M. (2024). Degrees of separation: a
  flexible type system for safe concurrency. *OOPSLA 2024*.
- Bruzzone, F. (2026). A friendly tour of substructural, uniqueness, ownership, and
  capabilities types — and more! *federicobruzzone.github.io*.
