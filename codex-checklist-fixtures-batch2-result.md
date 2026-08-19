# RFC checklist fixtures — batch 2 result

## RFC-0023 — Type Ascription vs Turbofish

Checklist audit: complete as-is (2 items).

- §1: reused `typechecking/builtins/stage8_04_type_ascription.mtl`.
- §2: reused `evaluator/generics/83_turbofish.mtl`.

## RFC-0030 — Module System Redesign

Checklist audit: added §12, rejecting obsolete `mod`, `use`, and `pub use`
declarations. The current grammar confirms that only `import` and `export` are
accepted.

- §1: reused `module_semantics/explicit_named_import_function_call`.
- §2: reused `module_semantics/facade_re_exports_item_and_consumer_can_use_it`.
- §3: reused `module_loading/facade_module_alongside_directory`.
- §4: reused `module_semantics/importing_private_item_is_t0009`.
- §5: added `parsing/rfc0030_import_after_declaration` (negative parse fixture).
- §6: reused `module_loading/accepts_root_self_super_std_and_child_roots_in_non_root_modules`.
- §7: reused the explicit/glob conflict fixtures in `module_semantics`.
- §8: reused `module_loading/rejects_circular_module_graph` and
  `module_loading/import_nonexistent_module_is_a_load_error`.
- §9: added `module_semantics/rfc0030_bare_export_loads_module`.
- §10: reused `module_loading/single_file_program_loads_without_modules`.
- §11: reused `module_semantics/std_core_builtins_available_in_each_module_without_import`.
- §12: added `parsing/rfc0030_legacy_mod_use_rejected` (negative parse fixture).

## RFC-0031 — Topological Per-Module Typechecking

Checklist audit: complete as-is (9 items).

- §§1–8: reused the existing module-semantics and module-loading fixtures whose
  sidecars now cite their matching sections.
- §9: added `module_semantics/rfc0031_reexport_private_item_is_t0009`.

## RFC-0032 — Field-Level Visibility

Checklist audit: added §8, requiring a warning for a `public` field on a private
struct. The built interpreter currently accepts that construct without a warning,
so §8 is deliberately uncited.

- §1: reused `module_semantics/mixed_visibility_struct_allows_public_field_access_across_modules`.
- §2: reused the existing private-field read, assignment, and declaring-module
  access fixtures.
- §3: reused `module_semantics/private_struct_field_construction_across_modules_is_t0009`.
- §§4–7: deliberately uncited. Struct patterns with `..`, linear declarations,
  and the related external-visibility behavior are not all available in the
  current grammar/implementation; these pre-existing checklist claims need human
  review rather than a misleading citation.
- §8: deliberately uncited; warning is not emitted by the current interpreter.

## RFC-0034 — Struct/Enum Aspect Bounds

Checklist audit: complete as-is (5 items).

- §1: reused `typechecking/generics/stage13_03_inline_and_where_merged`.
- §2: reused `typechecking/generics/stage14_neg_04_enum_construction_bound_violated`.
- §3: reused `typechecking/generics/stage14_10_impl_method_with_bounded_type_param`.
- §4: added `typechecking/generics/rfc0034_aspect_extend_and_match_bound`.
- §5: deliberately uncited. A focused generic match-arm probe fails with T0002
  (the receiver type for the aspect call cannot be inferred), so this claimed
  propagation is not currently demonstrated.

## Verification

`rfc.py check` is clean. It reports full coverage for RFC-0023 (2/2), RFC-0030
(12/12), and RFC-0031 (9/9); the intentional gaps are RFC-0032 (3/8) and
RFC-0034 (4/5), as recorded above.
