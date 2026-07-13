# ADR-0044: Validation Strategy for Opaque Return Variables

## Status

Accepted

## Context

RFC-0037 introduces return-position `impl Aspect` types, which allow functions to return opaque types that implement an aspect interface. The caller sees an abstract type that satisfies the aspect bound, without knowing the concrete implementation.

A key requirement is preventing the caller from naming or observing the concrete type of the opaque return value while still allowing normal usage patterns like:
- Passing the value to functions that accept `impl Aspect` parameters
- Calling aspect methods on the value
- Storing the value in variables declared with `impl Aspect` types

However, the type system must prevent problematic usage patterns like:
- Explicitly naming the concrete type (`let x: ConcreteType = f();`)
- Casting to the concrete type
- Passing to non-generic parameters declared with the exact concrete type

## Decision

We chose a **centralized validation strategy** rather than scattered per-site guards or deeper `unify()`-engine changes. This approach:

1. **Centralized validation**: Add a single validation function `validate_opaque_return_bindings()` that checks all opaque return variables at the end of constraint solving, mirroring the existing `validate_literal_bindings()` pattern.

2. **Linked vs. unlinked discriminator**: During function inference, distinguish between:
   - **Linked case**: When the return-position `impl Aspect` is linked to a parameter (e.g., `fun transform(x: impl Display) -> impl Display { x }`)
   - **Unlinked case**: When the return-position `impl Aspect` is independent (e.g., `fun make_pair() -> impl Display { 42 }`)

3. **Per-quantified-var metadata**: Store opaque return information in `TypeScheme.opaque_returns` to track which quantified variables represent opaque returns and their concrete types.

4. **Call-site registration**: When instantiating opaque-returning functions at call sites, register the aspect bounds in `current_type_param_bounds` and mark the variables in `opaque_return_vars` for validation.

## Rationale

### Why Centralized Validation?

1. **Avoids scattered guards**: Per-site guards would be fragile and easy to miss edge cases
2. **Mirrors existing patterns**: Follows the proven `validate_literal_bindings()` approach
3. **Comprehensive coverage**: Validates all constraint solving outcomes in one place
4. **Easier maintenance**: Single point of modification for validation logic

### Why Not `unify()` Engine Changes?

1. **Minimally invasive**: Avoid changing core unification logic that affects other features
2. **Preserves existing behavior**: Let the standard constraint solving handle normal cases
3. **Separation of concerns**: Validation is a cross-cutting concern separate from unification

### Why Linked/Unlinked Discrimination?

1. **Correctness**: Only unlinked cases need opacity enforcement (linked cases can name the concrete type)
2. **Performance**: Avoid unnecessary validation for linked cases
3. **Semantic accuracy**: Matches the RFC's specification about independence

## Implementation

### Key Components

1. **TypeScheme.opaque_returns**: Per-quantified-var metadata tracking `(aspect_name, concrete_type)`
2. **InferContext.opaque_return_vars**: Set of type variables representing opaque returns at the current call site
3. **validate_opaque_return_bindings()**: Checks that opaque return variables are not bound to concrete types
4. **register_type_var_bound()**: Registers aspect bounds for call-site type variables

### Validation Logic

```rust
pub fn validate_opaque_return_bindings(
    &self,
    subst: &Substitution,
    span: &Span,
) -> Result<(), MetelError> {
    for &var in &self.opaque_return_vars {
        match subst.apply(&InferType::Var(var)) {
            InferType::Var(_) => {
                // Still unbound, which is allowed
            }
            InferType::Never => {
                // Bottom type, which is allowed
            }
            InferType::Concrete(_) => {
                // Bound to a concrete type - this violates opacity
                return Err(MetelError::type_error(
                    TypeErrorCode::T0018,
                    "cannot name the concrete type of an opaque `impl Aspect` return value; use `impl Aspect` or a generic bound instead".to_string(),
                    span,
                ));
            }
            _ => {
                // Bound to some other inference type - this should be fine
                // The key insight is that these variables should remain abstract
                // for method dispatch, but can be used in valid contexts
            }
        }
    }
    Ok(())
}
```

### Call-Site Registration

```rust
// When instantiating an opaque-returning function
for (orig_tv, (aspect, _)) in scheme.opaque_returns.iter() {
    if let Some(&fresh_tv) = renaming.get(&orig_tv) {
        ctx.register_type_var_bound(fresh_tv, aspect.clone());
        ctx.mark_opaque_return_var(fresh_tv);
    }
}
```

## Alternatives Considered

### Scattered Per-Site Guards

**Problem**: Would require adding checks at many sites:
- `let`/`mut` variable declarations
- `Expr::Ascribe` type annotations
- Function argument passing
- Struct field assignments
- Array element assignments

**Issue**: Easy to miss sites, inconsistent behavior, hard to maintain.

### Deeper `unify()` Engine Changes

**Problem**: Would require modifying core unification logic to understand opaque return semantics.

**Issue**: Risk of breaking other features, complex implementation, harder to reason about.

### No Validation at All

**Problem**: Would allow concrete type naming to leak through.

**Issue**: Violates the RFC's opacity guarantees, reduces type safety.

## Testing Strategy

1. **Positive test cases**: Verify valid usage patterns work
   - Basic opaque returns
   - Method calls on opaque returns
   - Passing to `impl Aspect` parameters
   - Linked vs. unlinked cases

2. **Negative test cases**: Verify invalid patterns are rejected
   - Explicit concrete type naming
   - Casting to concrete types
   - Passing to non-generic concrete parameters

3. **Cross-module testing**: Ensure opacity is preserved across module boundaries

## Future Considerations

1. **Performance optimization**: Only validate when necessary (skip for linked cases)
2. **Better error messages**: More specific diagnostics for different violation types
3. **Integration with other features**: Ensure compatibility with planned features like multiple aspect bounds

## Related Work

- **RFC-0037**: Return-Position `impl Aspect` - the original specification
- **ADR-0043**: Generic Type Arg Recovery from Field Values - related monomorphization patterns
- **Existing `validate_literal_bindings()`**: Template for the centralized validation approach