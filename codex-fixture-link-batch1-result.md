# RFC fixture-link batch 1

## RFC-0007 — Compiler-Compatible Primitive Type System

- §1 — reused `evaluator/types/82_sized_numeric_types.mtl`.
- §2 — reused `evaluator/builtins/81_char.mtl`.
- §3 — reused `evaluator/types/82_sized_numeric_types.mtl`.
- §4 — reused `evaluator/arithmetic/11_overflow_panics.mtl`.
- §5 — reused `evaluator/types/neg_04_array_negative_index.mtl`.
- §6 — reused `evaluator/literals/04_polymorphic_literals.mtl`.

## RFC-0010 — String Interpolation

- §§1, 2, 4, 5 — reused `evaluator/builtins/38_builtins.mtl`.
- §3 — new `evaluator/builtins/86_interpolation_evaluation_order.mtl`, which
  proves two placeholders execute once and left-to-right through a mutable counter.

## RFC-0019 — Return Context Type Propagation

- §§1, 2 — reused `typechecking/functions/stage7_01_return_type_propagation.mtl`.

## RFC-0021 — Type Ascription Syntax

- §§1, 3 — reused `typechecking/builtins/stage8_04_type_ascription.mtl`.
- §2 — reused `typechecking/builtins/stage8_neg_02_ascribe_type_mismatch.mtl`.
- §4 — new `typechecking/builtins/stage8_neg_08_chained_type_ascription.mtl`.

## RFC-0022 — Braceless if body syntax

- §§1, 3 — reused `evaluator/control_flow/47_braceless_if.mtl`.
- §4 — reused `evaluator/control_flow/neg_19_braceless_if_dangling_else.mtl`.
- §5 — reused `evaluator/control_flow/neg_20_braceless_if_mixed_arms.mtl`.
- §2 — deliberately uncited. The implementation requires a no-`else` braceless
  `if` to have `Unit` type, but it does allow that Unit-valued expression in a
  binding (for example, `let value = if (true) println("side effect");`). The
  checklist's statement-position-only claim therefore does not hold as written.
  This was verified directly with the release binary.
