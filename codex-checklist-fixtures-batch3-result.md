# RFC checklist fixtures — batch 3

## RFC-0035 — `impl Aspect` anonymous parameters

Checklist expanded from four to six items. Items 5 and 6 capture the still-live,
independently testable rejections of `impl Aspect` in struct-field and local-binding
annotations; return-position bounds remain governed by RFC-0037.

| Section | Fixture |
|---|---|
| 1 | `typechecking/generics/stage12_03_impl_aspect_param.mtl` |
| 2 | `typechecking/generics/stage12_04_impl_aspect_independent.mtl` |
| 3 | `typechecking/generics/stage14_03_impl_aspect_plus_where.mtl` |
| 4 | `typechecking/generics/stage12_neg_02_impl_aspect_bound_not_satisfied.mtl` |
| 5 | `typechecking/generics/stage18_neg_10_impl_aspect_struct_field_array.mtl` |
| 6 | `typechecking/generics/stage18_neg_08_impl_aspect_local_let_array.mtl` |

## RFC-0041 — lambda syntax

Checklist expanded from five to six items by splitting the independently testable
named-function spelling and rejected legacy anonymous-`fun` spelling.

| Section | Fixture |
|---|---|
| 1 | `evaluator/closures/33_closure.mtl`, `evaluator/closures/unannotated_closure_return_type_inferred.mtl` |
| 2 | new `parsing/neg_lambda_without_arrow.mtl` |
| 3 | `evaluator/closures/unannotated_closure_return_type_inferred.mtl` |
| 4 | `evaluator/closures/33_closure.mtl`, `evaluator/closures/unannotated_closure_return_type_inferred.mtl` |
| 5 | `evaluator/closures/33_closure.mtl` |
| 6 | `parsing/neg_11_old_fun_closure_syntax_in_expression_position.mtl` |

## RFC-0042 — mutable bindings

Checklist complete as-is (four items). The prior correction removing the stale public
binding claim remains intact.

| Section | Fixture |
|---|---|
| 1 | `typechecking/functions/stage4_neg_05_compound_assign_to_let.mtl` |
| 2 | `evaluator/control_flow/16_for_loop.mtl` |
| 3 | `evaluator/control_flow/16_for_loop.mtl` |
| 4 | `evaluator/control_flow/17_for_in.mtl` |

## RFC-0045 — mutable address-of lvalue paths

Checklist complete as-is (four items). All four sections are exercised by
`evaluator/types/14_mut_field_pointer.mtl`.

## RFC-0053 — fixed-size arrays

Checklist expanded from seven to eight items by splitting the independently testable
field-type and nested-type cases. New focused fixtures cover repeat-expression
evaluation and nesting; existing fixtures cover the remaining supported cases.

| Section | Fixture |
|---|---|
| 1 | `evaluator/types/13_sized_array_extended.mtl` |
| 2 | new `evaluator/types/fixed_array_repeat_evaluates_once.mtl` |
| 3 | `typechecking/types/stage3_04_sized_arrays.mtl`, `stage3_neg_07_sized_array_n_mismatch.mtl`, `stage3_neg_08_sized_array_elem_mismatch.mtl` |
| 4 | deliberately uncited |
| 5 | `evaluator/structs/45_lvalue_paths.mtl` |
| 6 | new `evaluator/types/fixed_array_nested.mtl` |
| 7 | `evaluator/types/12_sized_array.mtl`, `evaluator/types/13_sized_array_extended.mtl`, `typechecking/types/stage3_neg_09_sized_array_pattern_undercount.mtl` |
| 8 | new `parsing/neg_sized_array_named_length.mtl` |

Section 4 is deliberately uncited. Its checklist says a dynamic `T[]` value does not
implicitly coerce to `[T; N]`; the built interpreter accepted that direction in a direct
verification program. The same check also showed indexing `[T; 0]` produces a runtime
bounds error rather than the RFC's claimed static literal-index rejection. These are
reported rather than papered over with a citation.

## Verification

- `cargo test --release --workspace` passed with zero failures (816 integration tests).
- `METEL_CORE_ROOT=$(pwd) python3 docs/public/rfcs/tools/rfc.py check` reported
  `check: no problems found.`
- Coverage is full for RFC-0035 (6/6), RFC-0041 (6/6), RFC-0042 (4/4), and RFC-0045
  (4/4); RFC-0053 is 7/8 for the deliberate Section 4 gap above.
