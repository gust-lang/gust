# Per-Field Multiplicities in Linear Structs

*Design exploration — June 2026*

*Note: this file lives in `metel-interpreter/docs/reports/` because the `docs/`
submodule was unavailable at time of writing. It belongs in `docs/reports/` once the
submodule is accessible.*

This report follows on from the substructural and separation types design exploration.
That report proposed `phantom linear` fields as a way to embed linear capability tokens
inside a struct. This report examines the design question that proposal raises: if a
struct has both linear and non-linear fields, what are the access and consumption rules
for each?

The conclusion is that the natural answer — per-field multiplicities — points toward a
richer type system than a flat per-struct linearity, and that this design reconciles
the "two-struct" and "single-struct" approaches to capability tokens.

---

## 1. The starting observation

Consider a struct that holds both a plain address and a linear capability:

```metel
struct File {
    fd:   i64,
    _cap: phantom linear (),
}
```

`fd` is an ordinary integer — a freely-copyable descriptor. `_cap` is a phantom linear
field that carries no runtime data but imposes a use-exactly-once obligation on the
struct. The struct is linear because it contains a linear field.

Two questions arise immediately:

1. Should `fd` be freely readable without consuming the struct? Intuitively yes: reading
   an `i64` field has nothing to do with the linear obligation on `_cap`.

2. What happens to `fd` after `_cap` is consumed? The struct is "used up," but `fd`
   is an unrestricted value with nowhere to go.

These questions are symptoms of a single underlying design gap: the treatment of
linearity as a flat per-struct property rather than a per-field one.

---

## 2. Non-linear fields are freely accessible via borrow

The first question has a clean answer. Reading `fd` through a shared borrow (`&File`)
does not touch the linear field at all. The borrow suspends the linear obligation for
its duration and reinstates it when it expires. The `i64` is simply read through the
borrow with no multiplicity cost.

This is the same principle Clean uses for uniqueness attributes: passing a unique value
to a function expecting a non-unique parameter temporarily relaxes the uniqueness
constraint without consuming the value.

```metel
fun log_descriptor(f: &File) -> i64 {
    f.fd   // read-only borrow of fd — _cap is untouched, File is still unconsumed
}

fun main() {
    let f = File::open("/tmp/log.txt");

    let fd1 = log_descriptor(&f);   // borrow — f still alive, _cap still unconsumed
    let fd2 = log_descriptor(&f);   // fine again
    let fd3 = f.fd;                  // direct copy of i64 — no consumption

    f.close();                       // _cap consumed here
}
```

The key rule: a `&self` borrow can freely read any field whose multiplicity is ω
(unrestricted). It suspends, but does not consume, any field whose multiplicity is 1
(linear). The linear obligation is reinstated when the borrow expires.

The consequence: the `fd: i64` field in a linear struct is already freely copyable and
"hand-able" without consuming the struct. The user's intuition is correct. The struct's
linearity restricts its *lifetime* (it must be consumed), not the *readability* of its
non-linear fields.

---

## 3. What happens to non-linear fields on consumption

The second question is harder. When the struct is consumed — when all its linear fields
are satisfied — what becomes of the non-linear fields?

There are three coherent options.

### Option A: They are dropped

The struct is consumed atomically. Non-linear fields allow weakening, so they are
trivially discarded alongside the linear ones.

```metel
fun close(f: File) {
    sys_close(f.fd);
    // f consumed — _cap satisfied (phantom: disappears), fd dropped (i64 allows weakening)
}
```

This is the simplest rule and requires no special treatment. Its limitation: the caller
cannot recover `fd` after calling `close`, even though `fd` is an unrestricted value
that has no reason to disappear.

### Option B: They can be explicitly extracted (residual)

Consuming the struct's linear fields releases the non-linear fields as standalone
values. The non-linear fields form a "residual" that the consuming function may use or
discard at will.

```metel
fun close(f: File) -> i64 {
    sys_close(f.fd);
    f.fd   // extract fd — caller can log it, store it, pass it to a tracer
}

// Via destructuring:
fun close(f: File) -> i64 {
    let File { fd, _cap: _ } = f;   // _ consumes _cap (phantom); fd is freed as i64
    sys_close(fd);
    fd
}
```

Extraction is explicit: the compiler does not automatically inject the residual
anywhere; the programmer either uses the non-linear fields or ignores them (which is
always valid, since they allow weakening).

### Option C: The struct downgrades automatically

After consuming the linear field, the binding transitions to a residual struct
containing only the non-linear fields, without explicit destructuring. The type of `f`
changes at the point of linear-field consumption.

```metel
// Hypothetical — requires the checker to reason about partial consumption
fun main() {
    let f = File::open("/tmp/log.txt");   // f: File { fd: i64 (ω), _cap: () (1) }
    consume_cap(f);                       // _cap consumed — f transitions to { fd: i64 }
    let fd = f.fd;                        // still accessible: f is now just { fd: i64 }
}
```

This is the most expressive option but the most complex to implement, since the type
checker must track which linear fields have been consumed at each program point and
update the type of the binding accordingly. This is full *typestate* on the field level,
not just on the struct level. It also raises aliasing questions: if `p = &f` was taken
before the downgrade, what type does `p` have afterwards?

Option B is the most practical starting point: it is unambiguous, requires no
per-binding state tracking beyond normal move semantics, and is sufficient for the
common patterns.

---

## 4. Reconciliation with the two-struct design

Option B reveals that the two-struct design and the mixed-multiplicity struct design are
the same thing expressed at different levels of explicitness.

The two-struct design makes the split upfront:

```metel
struct FileHandle { fd: i64 }          // multiplicity ω — freely copyable
linear struct FileCap {}               // multiplicity 1 — must be consumed
```

The mixed-multiplicity struct makes the split at consumption:

```metel
struct File {
    fd:   i64,               // multiplicity ω
    _cap: phantom linear (), // multiplicity 1
}

fun close(f: File) -> i64 {
    sys_close(f.fd);
    f.fd   // residual extraction: fd survives, _cap is consumed
}
```

Under Option B, consuming `File` and extracting `fd` produces exactly what you would
have had if you had started with two structs and consumed `FileCap` while holding
`FileHandle` independently. The two approaches are equivalent in expressive power;
they differ only in whether the split is manifest in the type definitions or deferred
to the point of consumption.

The practical tradeoffs:

| | Two-struct | Mixed-multiplicity struct |
|---|---|---|
| Freely alias the handle | Yes — it has its own type | Yes — via `&File` borrows |
| Return handle after consuming cap | Yes — `FileHandle` lives on its own | Yes — via residual extraction (Option B) |
| Typestate in the type | Separate type param on each struct | One type param on one struct |
| Syntax weight | Two definitions, two values threaded | One definition, one value |

The two-struct design is preferable when the address and the capability have genuinely
different lifetimes and are passed to different parts of the program independently. The
mixed-multiplicity struct is preferable when they always travel together until the
terminal action.

---

## 5. The underlying model: per-field multiplicities

What the analysis above points toward is that "a struct is linear if it contains a
linear field" is too coarse. The natural model is that each field carries its own
multiplicity, and the struct's overall multiplicity is derived from the join of its
fields' multiplicities. Access rules and consumption rules follow from the individual
field multiplicities, not from a single inherited struct-level flag.

The multiplicity lattice for the uses considered here has three elements:

```
0 — erased (not present at runtime; must not appear in expressions)
1 — linear (exactly once; no weakening, no contraction)
ω — unrestricted (any number of times; weakening and contraction allowed)
```

Field access rules under this model:

| Field multiplicity | Via `&self` borrow | Via consuming `self` |
|---|---|---|
| ω | Free copy — no cost | Extracted or dropped (weakening allowed) |
| 1 | Borrow suspends obligation; reinstated on borrow expiry | Must be consumed (no weakening) |
| 0 | Not accessible as a value | Not accessible as a value |

A struct's overall multiplicity is the least upper bound of its fields' multiplicities:
a struct with any multiplicity-1 field is linear; a struct with all multiplicity-ω
fields is unrestricted; a struct with any multiplicity-0 field and no multiplicity-1
fields is affine.

This is the direction quantitative type theory takes (Atkey 2018; McBride 2016, "I Got
Plenty o' Nuttin'"; Brady 2021, Idris 2) and that Linear Haskell approaches with
multiplicity polymorphism. Multiplicities become first-class and can be applied
per-binding, per-field, and per-type-parameter. A generic struct can be parameterised
over the multiplicity of its capability field:

```metel
// Cap is a multiplicity parameter: linear () makes the struct linear,
// () makes it unrestricted
struct Guarded<T, Cap> {
    value: T,
    _cap:  phantom Cap,
}

// Guarded<File, linear ()>  — linear; must be consumed
// Guarded<i64,  ()>          — unrestricted; freely copyable
```

The same abstraction covers both guarded and unguarded cases without duplication.

---

## 6. Implications for metel

Implementing the full per-field multiplicity model requires:

1. **Multiplicity annotations on fields.** Fields carry a multiplicity (`ω` by default,
   `linear` for exactly-once, potentially `affine` for at-most-once). The `phantom
   linear` syntax from the previous report is a surface form of a multiplicity-1
   phantom field.

2. **Borrow rules aware of field multiplicities.** `&self` borrows suspend multiplicity-1
   fields and permit free access to multiplicity-ω fields. The type checker tracks
   which fields are suspended during each borrow.

3. **Consumption rules with residual extraction.** When a struct with mixed-multiplicity
   fields is consumed via a `self` receiver or destructuring, multiplicity-1 fields must
   be consumed and multiplicity-ω fields are released as standalone values (or dropped).

4. **Derived struct multiplicity.** The struct's own multiplicity is inferred from its
   fields; no separate `linear struct` declaration is needed if the field annotation
   conveys the intent.

5. **Multiplicity polymorphism (optional, later).** Allow type parameters to range over
   multiplicities so that `Guarded<T, Cap>` works for both linear and unrestricted `Cap`
   without duplicating the definition.

Steps 1–4 are sufficient for the file handle, protocol state machine, and capability
token patterns described in the substructural types report. Step 5 is an extension that
enables library-level abstraction over resource obligations.

---

## References

- Girard, J.-Y. (1987). Linear logic. *Theoretical Computer Science*, 50(1).
- Wadler, P. (1990). Linear types can change the world! *IFIP TC 2 Working Conference*.
- Barendsen, E., & Smetsers, S. (1996). Uniqueness typing for functional languages.
  *Mathematical Structures in Computer Science*, 6(6).
- Atkey, R. (2018). Syntax and semantics of quantitative type theory. *LICS 2018*.
- McBride, C. (2016). I got plenty o' nuttin'. *A List of Successes That Can Change the
  World*, LNCS 9600.
- Bernardy, J.-P. et al. (2018). Linear Haskell: practical linearity in a higher-order
  polymorphic language. *POPL 2018*.
- Brady, E. (2021). Idris 2: quantitative type theory in practice. *ECOOP 2021*.
- Marshall, D., & Orchard, D. (2024). Functional ownership through fractional
  uniqueness. *OOPSLA 2024*.
