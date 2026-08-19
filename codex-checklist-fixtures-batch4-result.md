# RFC checklist fixtures — batch 4 result

## RFC-0054 — `List<T>`

Checklist audit: expanded from five to six items. Section 6 records the
independently testable guarantee that `List::from` copies its source rather than
aliasing it.

| Section | Fixture |
|---|---|
| 1 | reused `evaluator/builtins/38_builtins.mtl` |
| 2 | reused `evaluator/builtins/38_builtins.mtl` |
| 3 | reused `evaluator/builtins/38_builtins.mtl` |
| 4 | reused `evaluator/builtins/38_builtins.mtl` |
| 5 | reused `evaluator/builtins/38_builtins.mtl` |
| 6 | new `evaluator/builtins/86_list_from_copies_source.mtl` |

## RFC-0106 — optional braces for empty constructors

Checklist audit: complete as-is (three items).

| Section | Fixture |
|---|---|
| 1 | reused `evaluator/structs/89_empty_constructor_forms.mtl` |
| 2 | reused `evaluator/structs/89_empty_constructor_forms.mtl` |
| 3 | new `typechecking/structs/stage5_neg_42_non_empty_struct_requires_fields.mtl`; new `typechecking/enums/stage6_neg_13_non_empty_variant_requires_fields.mtl` |

RFC-0058 and RFC-0059 were correctly left untouched: both retain their `"*"`
whole-RFC `untestable` exemptions and have no normative sections.

## Claims left uncited

None.

## Verification

- `cargo test --release --workspace` passed with zero failures (817 integration
  tests).
- `METEL_CORE_ROOT=$(pwd) python3 docs/public/rfcs/tools/rfc.py check` reported
  `check: no problems found.`
- Coverage is full for RFC-0054 (6/6) and RFC-0106 (3/3); RFC-0058 and RFC-0059
  remain valid whole-RFC exemptions.
