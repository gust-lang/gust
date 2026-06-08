# Evaluator Integration Benchmark Summary

| Fixture | Mean (ms) | Min (ms) | Max (ms) | Stddev (ms) | Evaluate Phase (ms) |
|---|---:|---:|---:|---:|---:|
| int_01_statistics.mtl | 87.376 | 85.304 | 93.315 | 3.020 | 1.395 |
| int_02_battle.mtl | 104.877 | 104.477 | 105.609 | 0.407 | 1.482 |
| int_03_aspects.mtl | 32.383 | 32.130 | 32.602 | 0.189 | 0.536 |
| int_03_generic_option_chain.mtl | 76.467 | 75.569 | 77.155 | 0.708 | 2.645 |
| int_04_generic_algorithms.mtl | 160.724 | 155.996 | 163.849 | 3.308 | 4.861 |
| int_04_pipeline.mtl | 22.233 | 21.977 | 22.706 | 0.261 | 0.414 |
| int_05_aspects_combined.mtl | 20.485 | 19.934 | 21.458 | 0.538 | 0.449 |
| int_05_generic_data_pipeline.mtl | 66.298 | 64.578 | 67.589 | 1.258 | 2.374 |
| int_06_display.mtl | 24.108 | 23.892 | 24.509 | 0.218 | 0.452 |
| int_07_pub_declarations.mtl | 3.678 | 3.594 | 3.891 | 0.111 | 0.090 |
| int_08_std_core_paths.mtl | 23.496 | 22.919 | 24.190 | 0.467 | 0.510 |
| int_09_numeric_pipeline.mtl | 10.938 | 10.629 | 11.332 | 0.284 | 0.276 |
| int_10_char_processing.mtl | 11.545 | 11.266 | 11.635 | 0.141 | 0.374 |
| int_11_generic_sized.mtl | 27.107 | 26.700 | 27.748 | 0.400 | 0.648 |

## int_01_statistics.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 74.185 |
| typecheck | 11.796 |
| evaluate | 1.395 |
| total | 87.376 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.052 |
| inference | 2.357 |
| solve | 8.481 |
| scheme_env | 0.007 |
| construction | 0.350 |
| finalize | 0.008 |

Typechecker counters: `solve_calls=197`, `constraints_processed=506`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 1.129 | 0.386 |
| sort_floats | 3 | 0.141 | 0.133 |
| binary_search | 3 | 0.141 | 0.071 |
| compute_stats | 7 | 0.110 | 0.107 |
| z_score | 2 | 0.089 | 0.042 |
| median | 3 | 0.084 | 0.030 |
| map_floats | 2 | 0.076 | 0.059 |
| float_eq | 8 | 0.074 | 0.062 |
| count_if | 2 | 0.064 | 0.047 |
| filter_floats | 1 | 0.042 | 0.032 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 1.129 |
| main | binary_search | 3 | 0.141 |
| main | z_score | 2 | 0.089 |
| main | sort_floats | 1 | 0.087 |
| main | compute_stats | 5 | 0.087 |
| main | median | 3 | 0.084 |
| main | map_floats | 2 | 0.076 |
| binary_search | float_eq | 7 | 0.069 |
| main | count_if | 2 | 0.064 |
| median | sort_floats | 2 | 0.054 |

Artifacts: `int_01_statistics.mtl.profile.json`, `int_01_statistics.mtl.callgraph.dot`

## int_02_battle.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 94.491 |
| typecheck | 8.903 |
| evaluate | 1.482 |
| total | 104.877 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.062 |
| inference | 1.834 |
| solve | 6.099 |
| scheme_env | 0.007 |
| construction | 0.333 |
| finalize | 0.007 |

Typechecker counters: `solve_calls=216`, `constraints_processed=430`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 1.054 | 0.390 |
| battle | 3 | 0.432 | 0.226 |
| resolve | 20 | 0.259 | 0.184 |
| take_damage | 13 | 0.059 | 0.059 |
| format_round | 1 | 0.037 | 0.021 |
| simulate_rounds | 1 | 0.034 | 0.018 |
| heal | 7 | 0.027 | 0.027 |
| is_alive | 18 | 0.022 | 0.022 |
| <closure> | 7 | 0.019 | 0.019 |
| add_defense | 6 | 0.018 | 0.018 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 1.054 |
| main | battle | 3 | 0.432 |
| battle | resolve | 12 | 0.165 |
| main | resolve | 7 | 0.080 |
| resolve | take_damage | 10 | 0.042 |
| main | format_round | 1 | 0.037 |
| main | simulate_rounds | 1 | 0.034 |
| resolve | heal | 5 | 0.019 |
| main | take_damage | 3 | 0.016 |
| battle | is_alive | 12 | 0.015 |

Artifacts: `int_02_battle.mtl.profile.json`, `int_02_battle.mtl.callgraph.dot`

## int_03_aspects.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 29.141 |
| typecheck | 2.706 |
| evaluate | 0.536 |
| total | 32.383 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.058 |
| inference | 0.616 |
| solve | 1.577 |
| scheme_env | 0.005 |
| construction | 0.191 |
| finalize | 0.005 |

Typechecker counters: `solve_calls=149`, `constraints_processed=229`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.317 | 0.165 |
| circle_area_from_str | 3 | 0.047 | 0.035 |
| area | 9 | 0.031 | 0.028 |
| println | 2 | 0.026 | 0.026 |
| describe | 4 | 0.014 | 0.012 |
| next | 5 | 0.013 | 0.013 |
| approx | 9 | 0.009 | 0.009 |
| from | 5 | 0.006 | 0.006 |
| perimeter | 2 | 0.005 | 0.005 |
| parse_radius | 3 | 0.004 | 0.004 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.317 |
| main | circle_area_from_str | 3 | 0.047 |
| main | println | 2 | 0.026 |
| main | area | 7 | 0.024 |
| main | describe | 4 | 0.014 |
| main | next | 5 | 0.013 |
| main | approx | 9 | 0.009 |
| circle_area_from_str | area | 2 | 0.006 |
| main | perimeter | 2 | 0.005 |
| circle_area_from_str | parse_radius | 3 | 0.004 |

Artifacts: `int_03_aspects.mtl.profile.json`, `int_03_aspects.mtl.callgraph.dot`

## int_03_generic_option_chain.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 64.715 |
| typecheck | 9.107 |
| evaluate | 2.645 |
| total | 76.467 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.055 |
| inference | 1.962 |
| solve | 6.254 |
| scheme_env | 0.013 |
| construction | 0.367 |
| finalize | 0.007 |

Typechecker counters: `solve_calls=194`, `constraints_processed=380`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 2.279 | 0.701 |
| find_first | 6 | 0.299 | 0.264 |
| table_get | 3 | 0.280 | 0.080 |
| map_array | 3 | 0.241 | 0.181 |
| option_map | 10 | 0.234 | 0.220 |
| option_or | 18 | 0.214 | 0.214 |
| <closure> | 68 | 0.185 | 0.137 |
| option_and_then | 5 | 0.150 | 0.095 |
| option_is_some | 11 | 0.122 | 0.122 |
| count_where | 1 | 0.076 | 0.062 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 2.279 |
| main | table_get | 3 | 0.280 |
| main | map_array | 3 | 0.241 |
| main | option_or | 18 | 0.214 |
| main | find_first | 3 | 0.174 |
| main | option_and_then | 5 | 0.150 |
| main | option_map | 6 | 0.137 |
| table_get | find_first | 3 | 0.126 |
| main | option_is_some | 11 | 0.122 |
| main | count_where | 1 | 0.076 |

Artifacts: `int_03_generic_option_chain.mtl.profile.json`, `int_03_generic_option_chain.mtl.callgraph.dot`

## int_04_generic_algorithms.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 132.698 |
| typecheck | 23.165 |
| evaluate | 4.861 |
| total | 160.724 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.065 |
| inference | 4.610 |
| solve | 16.844 |
| scheme_env | 0.024 |
| construction | 0.527 |
| finalize | 0.013 |

Typechecker counters: `solve_calls=295`, `constraints_processed=658`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 4.272 | 1.241 |
| fold | 6 | 0.433 | 0.324 |
| map_arr | 5 | 0.420 | 0.320 |
| <closure> | 160 | 0.408 | 0.379 |
| rsum | 13 | 0.328 | 0.093 |
| find_first | 3 | 0.238 | 0.176 |
| filter | 3 | 0.231 | 0.189 |
| zip_with | 3 | 0.208 | 0.185 |
| result_is_ok | 9 | 0.200 | 0.200 |
| any | 2 | 0.175 | 0.141 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 4.272 |
| main | fold | 6 | 0.433 |
| main | map_arr | 5 | 0.420 |
| main | find_first | 3 | 0.238 |
| rsum | rsum | 11 | 0.233 |
| main | filter | 3 | 0.231 |
| main | zip_with | 3 | 0.208 |
| main | result_is_ok | 9 | 0.200 |
| main | any | 2 | 0.175 |
| main | result_unwrap_or | 7 | 0.133 |

Artifacts: `int_04_generic_algorithms.mtl.profile.json`, `int_04_generic_algorithms.mtl.callgraph.dot`

## int_04_pipeline.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 19.697 |
| typecheck | 2.123 |
| evaluate | 0.414 |
| total | 22.233 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.052 |
| inference | 0.479 |
| solve | 1.187 |
| scheme_env | 0.004 |
| construction | 0.183 |
| finalize | 0.004 |

Typechecker counters: `solve_calls=92`, `constraints_processed=174`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.261 | 0.138 |
| pipeline_step | 3 | 0.068 | 0.056 |
| next | 11 | 0.031 | 0.030 |
| summary | 3 | 0.010 | 0.008 |
| lex_token | 3 | 0.006 | 0.006 |
| from | 3 | 0.005 | 0.004 |
| parse_number | 2 | 0.004 | 0.004 |
| double | 2 | 0.003 | 0.003 |
| new | 3 | 0.002 | 0.002 |
| len | 1 | 0.002 | 0.002 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.261 |
| main | pipeline_step | 3 | 0.068 |
| main | next | 11 | 0.031 |
| main | summary | 3 | 0.010 |
| pipeline_step | lex_token | 3 | 0.006 |
| pipeline_step | parse_number | 2 | 0.004 |
| main | from | 2 | 0.003 |
| main | double | 2 | 0.003 |
| main | new | 3 | 0.002 |
| main | len | 1 | 0.002 |

Artifacts: `int_04_pipeline.mtl.profile.json`, `int_04_pipeline.mtl.callgraph.dot`

## int_05_aspects_combined.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 18.142 |
| typecheck | 1.895 |
| evaluate | 0.449 |
| total | 20.485 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.049 |
| inference | 0.363 |
| solve | 1.071 |
| scheme_env | 0.004 |
| construction | 0.191 |
| finalize | 0.004 |

Typechecker counters: `solve_calls=84`, `constraints_processed=171`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.249 | 0.159 |
| next | 28 | 0.030 | 0.030 |
| run_with_config | 2 | 0.021 | 0.016 |
| require_config | 2 | 0.016 | 0.013 |
| describe | 2 | 0.010 | 0.008 |
| run | 2 | 0.006 | 0.006 |
| load_config | 4 | 0.006 | 0.006 |
| new | 8 | 0.003 | 0.003 |
| from | 2 | 0.003 | 0.003 |
| string_concat | 15 | 0.002 | 0.002 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.249 |
| main | next | 28 | 0.030 |
| main | run_with_config | 2 | 0.021 |
| main | require_config | 2 | 0.016 |
| main | describe | 2 | 0.010 |
| main | run | 2 | 0.006 |
| main | new | 8 | 0.003 |
| run_with_config | load_config | 2 | 0.003 |
| require_config | load_config | 2 | 0.003 |
| run_with_config | from | 1 | 0.002 |

Artifacts: `int_05_aspects_combined.mtl.profile.json`, `int_05_aspects_combined.mtl.callgraph.dot`

## int_05_generic_data_pipeline.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 56.386 |
| typecheck | 7.538 |
| evaluate | 2.374 |
| total | 66.298 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.052 |
| inference | 1.413 |
| solve | 5.304 |
| scheme_env | 0.015 |
| construction | 0.296 |
| finalize | 0.008 |

Typechecker counters: `solve_calls=183`, `constraints_processed=390`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 2.060 | 0.593 |
| filter_array | 4 | 0.280 | 0.217 |
| map_array | 3 | 0.243 | 0.173 |
| <closure> | 105 | 0.216 | 0.197 |
| zip_with | 3 | 0.179 | 0.149 |
| any | 3 | 0.150 | 0.122 |
| maybe_map | 4 | 0.140 | 0.132 |
| all | 3 | 0.111 | 0.086 |
| maybe_get_or | 6 | 0.082 | 0.082 |
| take | 2 | 0.080 | 0.076 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 2.060 |
| main | filter_array | 4 | 0.280 |
| main | map_array | 3 | 0.243 |
| main | zip_with | 3 | 0.179 |
| main | any | 3 | 0.150 |
| main | maybe_map | 4 | 0.140 |
| main | all | 3 | 0.111 |
| main | maybe_get_or | 6 | 0.082 |
| main | take | 2 | 0.080 |
| map_array | <closure> | 25 | 0.065 |

Artifacts: `int_05_generic_data_pipeline.mtl.profile.json`, `int_05_generic_data_pipeline.mtl.callgraph.dot`

## int_06_display.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 21.337 |
| typecheck | 2.318 |
| evaluate | 0.452 |
| total | 24.108 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.055 |
| inference | 0.479 |
| solve | 1.366 |
| scheme_env | 0.004 |
| construction | 0.178 |
| finalize | 0.003 |

Typechecker counters: `solve_calls=123`, `constraints_processed=204`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.243 | 0.130 |
| render_score | 3 | 0.041 | 0.029 |
| println | 4 | 0.027 | 0.027 |
| format | 7 | 0.021 | 0.019 |
| next | 5 | 0.015 | 0.015 |
| parse_score | 3 | 0.005 | 0.004 |
| from | 4 | 0.004 | 0.004 |
| f64::to_string | 10 | 0.003 | 0.003 |
| string_concat | 21 | 0.002 | 0.002 |
| len | 1 | 0.002 | 0.002 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.243 |
| main | render_score | 3 | 0.041 |
| main | println | 4 | 0.027 |
| main | format | 5 | 0.016 |
| main | next | 5 | 0.015 |
| render_score | format | 2 | 0.006 |
| render_score | parse_score | 3 | 0.005 |
| main | f64::to_string | 8 | 0.003 |
| main | from | 3 | 0.002 |
| main | len | 1 | 0.002 |

Artifacts: `int_06_display.mtl.profile.json`, `int_06_display.mtl.callgraph.dot`

## int_07_pub_declarations.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 3.405 |
| typecheck | 0.183 |
| evaluate | 0.090 |
| total | 3.678 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.028 |
| inference | 0.031 |
| solve | 0.040 |
| scheme_env | 0.003 |
| construction | 0.038 |
| finalize | 0.002 |

Typechecker counters: `solve_calls=15`, `constraints_processed=35`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.018 | 0.014 |
| distance | 1 | 0.002 | 0.002 |
| classify | 1 | 0.001 | 0.001 |
| assert | 4 | 0.000 | 0.000 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.018 |
| main | distance | 1 | 0.002 |
| main | classify | 1 | 0.001 |
| main | assert | 4 | 0.000 |

Artifacts: `int_07_pub_declarations.mtl.profile.json`, `int_07_pub_declarations.mtl.callgraph.dot`

## int_08_std_core_paths.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 19.496 |
| typecheck | 3.490 |
| evaluate | 0.510 |
| total | 23.496 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.040 |
| inference | 0.695 |
| solve | 2.370 |
| scheme_env | 0.005 |
| construction | 0.180 |
| finalize | 0.005 |

Typechecker counters: `solve_calls=76`, `constraints_processed=217`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.309 | 0.173 |
| add_parsed | 3 | 0.057 | 0.045 |
| find_in | 5 | 0.038 | 0.036 |
| parse_positive | 9 | 0.021 | 0.020 |
| double_parsed | 2 | 0.020 | 0.016 |
| perhaps_to_result | 4 | 0.006 | 0.006 |
| map_some | 3 | 0.005 | 0.005 |
| from | 2 | 0.004 | 0.004 |
| string_concat | 6 | 0.001 | 0.001 |
| assert | 16 | 0.001 | 0.001 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.309 |
| main | add_parsed | 3 | 0.057 |
| main | find_in | 5 | 0.038 |
| main | double_parsed | 2 | 0.020 |
| add_parsed | parse_positive | 5 | 0.009 |
| main | parse_positive | 2 | 0.009 |
| main | perhaps_to_result | 4 | 0.006 |
| main | map_some | 3 | 0.005 |
| add_parsed | from | 2 | 0.004 |
| double_parsed | parse_positive | 2 | 0.004 |

Artifacts: `int_08_std_core_paths.mtl.profile.json`, `int_08_std_core_paths.mtl.callgraph.dot`

## int_09_numeric_pipeline.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 9.311 |
| typecheck | 1.351 |
| evaluate | 0.276 |
| total | 10.938 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.036 |
| inference | 0.321 |
| solve | 0.753 |
| scheme_env | 0.004 |
| construction | 0.108 |
| finalize | 0.004 |

Typechecker counters: `solve_calls=76`, `constraints_processed=135`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.118 | 0.086 |
| sum_i32 | 1 | 0.007 | 0.007 |
| scale | 5 | 0.007 | 0.006 |
| mean_f32 | 1 | 0.007 | 0.006 |
| bucket_of | 5 | 0.005 | 0.005 |
| List::push | 14 | 0.003 | 0.003 |
| List::get | 3 | 0.001 | 0.001 |
| assert | 21 | 0.001 | 0.001 |
| List::new | 3 | 0.001 | 0.001 |
| u8::From<f32>::from | 5 | 0.000 | 0.000 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.118 |
| main | sum_i32 | 1 | 0.007 |
| main | scale | 5 | 0.007 |
| main | mean_f32 | 1 | 0.007 |
| main | bucket_of | 5 | 0.005 |
| main | List::push | 14 | 0.003 |
| main | List::get | 3 | 0.001 |
| main | assert | 21 | 0.001 |
| main | List::new | 3 | 0.001 |
| bucket_of | u8::From<f32>::from | 5 | 0.000 |

Artifacts: `int_09_numeric_pipeline.mtl.profile.json`, `int_09_numeric_pipeline.mtl.callgraph.dot`

## int_10_char_processing.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 9.832 |
| typecheck | 1.339 |
| evaluate | 0.374 |
| total | 11.545 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.035 |
| inference | 0.308 |
| solve | 0.697 |
| scheme_env | 0.004 |
| construction | 0.160 |
| finalize | 0.004 |

Typechecker counters: `solve_calls=96`, `constraints_processed=158`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.250 | 0.121 |
| count_uppercase | 3 | 0.054 | 0.038 |
| to_lower | 7 | 0.031 | 0.023 |
| to_upper | 7 | 0.031 | 0.023 |
| is_uppercase | 24 | 0.027 | 0.026 |
| is_lowercase | 9 | 0.010 | 0.010 |
| List::push | 20 | 0.003 | 0.003 |
| u32::From<Char>::from | 60 | 0.003 | 0.003 |
| assert | 28 | 0.001 | 0.001 |
| Char::From<u32>::from | 16 | 0.001 | 0.001 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.250 |
| main | count_uppercase | 3 | 0.054 |
| main | to_lower | 7 | 0.031 |
| main | to_upper | 7 | 0.031 |
| count_uppercase | is_uppercase | 15 | 0.016 |
| to_lower | is_uppercase | 7 | 0.008 |
| to_upper | is_lowercase | 7 | 0.007 |
| main | is_lowercase | 2 | 0.003 |
| main | List::push | 20 | 0.003 |
| main | is_uppercase | 2 | 0.003 |

Artifacts: `int_10_char_processing.mtl.profile.json`, `int_10_char_processing.mtl.callgraph.dot`

## int_11_generic_sized.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 21.735 |
| typecheck | 4.724 |
| evaluate | 0.648 |
| total | 27.107 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.039 |
| inference | 0.846 |
| solve | 3.365 |
| scheme_env | 0.006 |
| construction | 0.214 |
| finalize | 0.004 |

Typechecker counters: `solve_calls=128`, `constraints_processed=283`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.458 | 0.159 |
| map_to_list | 4 | 0.177 | 0.158 |
| clamp | 8 | 0.079 | 0.079 |
| zip_add_i32 | 1 | 0.019 | 0.017 |
| all_positive_i32 | 2 | 0.016 | 0.014 |
| <closure> | 18 | 0.014 | 0.014 |
| List::push | 39 | 0.005 | 0.005 |
| List::get | 23 | 0.005 | 0.005 |
| List::new | 11 | 0.002 | 0.002 |
| assert | 33 | 0.001 | 0.001 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.458 |
| main | map_to_list | 4 | 0.177 |
| main | clamp | 8 | 0.079 |
| main | zip_add_i32 | 1 | 0.019 |
| main | all_positive_i32 | 2 | 0.016 |
| map_to_list | <closure> | 18 | 0.014 |
| map_to_list | List::push | 18 | 0.003 |
| main | List::get | 12 | 0.003 |
| main | List::push | 18 | 0.002 |
| main | assert | 33 | 0.001 |

Artifacts: `int_11_generic_sized.mtl.profile.json`, `int_11_generic_sized.mtl.callgraph.dot`
