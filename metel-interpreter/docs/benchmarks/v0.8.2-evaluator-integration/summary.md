# Evaluator Integration Benchmark Summary

| Fixture | Mean (ms) | Min (ms) | Max (ms) | Stddev (ms) | Evaluate Phase (ms) |
|---|---:|---:|---:|---:|---:|
| int_01_statistics.mtl | 80.738 | 80.290 | 81.972 | 0.498 | 1.562 |
| int_02_battle.mtl | 100.394 | 99.440 | 101.628 | 0.753 | 1.714 |
| int_03_aspects.mtl | 31.356 | 30.977 | 31.933 | 0.334 | 0.623 |
| int_03_generic_option_chain.mtl | 72.360 | 71.515 | 73.669 | 0.654 | 3.032 |
| int_04_generic_algorithms.mtl | 146.191 | 145.354 | 147.679 | 0.922 | 5.175 |
| int_04_pipeline.mtl | 21.525 | 21.122 | 23.020 | 0.525 | 0.474 |
| int_05_aspects_combined.mtl | 19.120 | 19.010 | 19.347 | 0.105 | 0.478 |
| int_05_generic_data_pipeline.mtl | 61.483 | 60.975 | 63.880 | 0.834 | 2.541 |
| int_06_display.mtl | 23.283 | 22.590 | 23.630 | 0.338 | 0.510 |
| int_07_pub_declarations.mtl | 3.582 | 3.466 | 3.757 | 0.089 | 0.098 |
| int_08_std_core_paths.mtl | 21.767 | 21.540 | 22.176 | 0.191 | 0.524 |
| int_09_numeric_pipeline.mtl | 10.383 | 10.300 | 10.548 | 0.075 | 0.308 |
| int_10_char_processing.mtl | 10.969 | 10.929 | 11.042 | 0.038 | 0.418 |
| int_11_generic_sized.mtl | 24.925 | 24.787 | 25.195 | 0.123 | 0.716 |

## int_01_statistics.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 71.247 |
| typecheck | 7.928 |
| evaluate | 1.562 |
| total | 80.738 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.054 |
| inference | 2.443 |
| solve | 4.657 |
| scheme_env | 0.008 |
| construction | 0.315 |
| finalize | 0.009 |

Typechecker counters: `solve_calls=197`, `constraints_processed=506`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 1.232 | 0.439 |
| sort_floats | 3 | 0.152 | 0.144 |
| compute_stats | 7 | 0.144 | 0.140 |
| median | 3 | 0.114 | 0.050 |
| z_score | 2 | 0.102 | 0.050 |
| map_floats | 2 | 0.086 | 0.069 |
| binary_search | 3 | 0.084 | 0.051 |
| count_if | 2 | 0.074 | 0.057 |
| filter_floats | 1 | 0.047 | 0.037 |
| std_dev | 2 | 0.043 | 0.019 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 1.232 |
| main | compute_stats | 5 | 0.119 |
| main | median | 3 | 0.114 |
| main | z_score | 2 | 0.102 |
| main | sort_floats | 1 | 0.088 |
| main | map_floats | 2 | 0.086 |
| main | binary_search | 3 | 0.084 |
| main | count_if | 2 | 0.074 |
| median | sort_floats | 2 | 0.063 |
| main | filter_floats | 1 | 0.047 |

Artifacts: `int_01_statistics.mtl.profile.json`, `int_01_statistics.mtl.callgraph.dot`

## int_02_battle.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 92.156 |
| typecheck | 6.524 |
| evaluate | 1.714 |
| total | 100.394 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.062 |
| inference | 1.900 |
| solve | 3.618 |
| scheme_env | 0.006 |
| construction | 0.328 |
| finalize | 0.008 |

Typechecker counters: `solve_calls=216`, `constraints_processed=430`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 1.232 | 0.452 |
| battle | 3 | 0.526 | 0.296 |
| resolve | 20 | 0.285 | 0.201 |
| take_damage | 13 | 0.064 | 0.064 |
| format_round | 1 | 0.044 | 0.026 |
| simulate_rounds | 1 | 0.037 | 0.020 |
| heal | 7 | 0.030 | 0.030 |
| is_alive | 18 | 0.026 | 0.026 |
| <closure> | 7 | 0.021 | 0.021 |
| add_defense | 6 | 0.019 | 0.019 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 1.232 |
| main | battle | 3 | 0.526 |
| battle | resolve | 12 | 0.184 |
| main | resolve | 7 | 0.086 |
| resolve | take_damage | 10 | 0.047 |
| main | format_round | 1 | 0.044 |
| main | simulate_rounds | 1 | 0.037 |
| resolve | heal | 5 | 0.021 |
| battle | is_alive | 12 | 0.018 |
| format_round | summary | 2 | 0.017 |

Artifacts: `int_02_battle.mtl.profile.json`, `int_02_battle.mtl.callgraph.dot`

## int_03_aspects.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 28.702 |
| typecheck | 2.030 |
| evaluate | 0.623 |
| total | 31.356 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.058 |
| inference | 0.620 |
| solve | 0.865 |
| scheme_env | 0.005 |
| construction | 0.192 |
| finalize | 0.006 |

Typechecker counters: `solve_calls=149`, `constraints_processed=229`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.336 | 0.184 |
| circle_area_from_str | 3 | 0.046 | 0.033 |
| area | 9 | 0.033 | 0.029 |
| println | 2 | 0.024 | 0.024 |
| describe | 4 | 0.015 | 0.013 |
| next | 5 | 0.014 | 0.013 |
| approx | 9 | 0.010 | 0.010 |
| from | 5 | 0.006 | 0.006 |
| perimeter | 2 | 0.006 | 0.005 |
| parse_radius | 3 | 0.004 | 0.004 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.336 |
| main | circle_area_from_str | 3 | 0.046 |
| main | area | 7 | 0.026 |
| main | println | 2 | 0.024 |
| main | describe | 4 | 0.015 |
| main | next | 5 | 0.014 |
| main | approx | 9 | 0.010 |
| circle_area_from_str | area | 2 | 0.007 |
| main | perimeter | 2 | 0.006 |
| circle_area_from_str | parse_radius | 3 | 0.004 |

Artifacts: `int_03_aspects.mtl.profile.json`, `int_03_aspects.mtl.callgraph.dot`

## int_03_generic_option_chain.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 63.037 |
| typecheck | 6.291 |
| evaluate | 3.032 |
| total | 72.360 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.060 |
| inference | 2.019 |
| solve | 3.460 |
| scheme_env | 0.011 |
| construction | 0.337 |
| finalize | 0.009 |

Typechecker counters: `solve_calls=194`, `constraints_processed=380`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 2.655 | 0.871 |
| find_first | 6 | 0.369 | 0.288 |
| table_get | 3 | 0.284 | 0.088 |
| map_array | 3 | 0.272 | 0.207 |
| option_map | 10 | 0.260 | 0.245 |
| option_or | 18 | 0.253 | 0.253 |
| <closure> | 68 | 0.240 | 0.190 |
| option_and_then | 5 | 0.158 | 0.100 |
| option_is_some | 11 | 0.140 | 0.140 |
| count_where | 1 | 0.074 | 0.061 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 2.655 |
| main | table_get | 3 | 0.284 |
| main | map_array | 3 | 0.272 |
| main | option_or | 18 | 0.253 |
| main | find_first | 3 | 0.249 |
| main | option_map | 6 | 0.161 |
| main | option_and_then | 5 | 0.158 |
| main | option_is_some | 11 | 0.140 |
| table_get | find_first | 3 | 0.120 |
| find_first | <closure> | 21 | 0.078 |

Artifacts: `int_03_generic_option_chain.mtl.profile.json`, `int_03_generic_option_chain.mtl.callgraph.dot`

## int_04_generic_algorithms.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 126.457 |
| typecheck | 14.560 |
| evaluate | 5.175 |
| total | 146.191 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.066 |
| inference | 4.732 |
| solve | 8.565 |
| scheme_env | 0.016 |
| construction | 0.445 |
| finalize | 0.014 |

Typechecker counters: `solve_calls=295`, `constraints_processed=658`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 4.789 | 1.458 |
| fold | 6 | 0.482 | 0.361 |
| map_arr | 5 | 0.480 | 0.374 |
| <closure> | 160 | 0.416 | 0.386 |
| rsum | 13 | 0.392 | 0.117 |
| zip_with | 3 | 0.285 | 0.257 |
| filter | 3 | 0.258 | 0.216 |
| find_first | 3 | 0.248 | 0.204 |
| any | 2 | 0.196 | 0.159 |
| bsearch | 11 | 0.192 | 0.107 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 4.789 |
| main | fold | 6 | 0.482 |
| main | map_arr | 5 | 0.480 |
| main | zip_with | 3 | 0.285 |
| rsum | rsum | 11 | 0.274 |
| main | filter | 3 | 0.258 |
| main | find_first | 3 | 0.248 |
| main | any | 2 | 0.196 |
| main | result_is_ok | 9 | 0.180 |
| main | result_unwrap_or | 7 | 0.152 |

Artifacts: `int_04_generic_algorithms.mtl.profile.json`, `int_04_generic_algorithms.mtl.callgraph.dot`

## int_04_pipeline.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 19.379 |
| typecheck | 1.672 |
| evaluate | 0.474 |
| total | 21.525 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.048 |
| inference | 0.508 |
| solve | 0.716 |
| scheme_env | 0.004 |
| construction | 0.164 |
| finalize | 0.005 |

Typechecker counters: `solve_calls=92`, `constraints_processed=174`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.284 | 0.170 |
| pipeline_step | 3 | 0.056 | 0.045 |
| next | 11 | 0.033 | 0.032 |
| summary | 3 | 0.011 | 0.009 |
| lex_token | 3 | 0.005 | 0.005 |
| from | 3 | 0.005 | 0.004 |
| parse_number | 2 | 0.003 | 0.003 |
| new | 3 | 0.003 | 0.003 |
| len | 1 | 0.003 | 0.003 |
| double | 2 | 0.002 | 0.002 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.284 |
| main | pipeline_step | 3 | 0.056 |
| main | next | 11 | 0.033 |
| main | summary | 3 | 0.011 |
| pipeline_step | lex_token | 3 | 0.005 |
| pipeline_step | parse_number | 2 | 0.003 |
| main | new | 3 | 0.003 |
| main | from | 2 | 0.003 |
| main | len | 1 | 0.003 |
| main | double | 2 | 0.002 |

Artifacts: `int_04_pipeline.mtl.profile.json`, `int_04_pipeline.mtl.callgraph.dot`

## int_05_aspects_combined.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 17.324 |
| typecheck | 1.318 |
| evaluate | 0.478 |
| total | 19.120 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.044 |
| inference | 0.368 |
| solve | 0.567 |
| scheme_env | 0.004 |
| construction | 0.132 |
| finalize | 0.004 |

Typechecker counters: `solve_calls=84`, `constraints_processed=171`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.288 | 0.192 |
| next | 28 | 0.033 | 0.033 |
| run_with_config | 2 | 0.023 | 0.018 |
| require_config | 2 | 0.018 | 0.015 |
| describe | 2 | 0.010 | 0.009 |
| load_config | 4 | 0.006 | 0.006 |
| run | 2 | 0.005 | 0.005 |
| from | 2 | 0.004 | 0.004 |
| new | 8 | 0.004 | 0.004 |
| string_concat | 15 | 0.002 | 0.002 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.288 |
| main | next | 28 | 0.033 |
| main | run_with_config | 2 | 0.023 |
| main | require_config | 2 | 0.018 |
| main | describe | 2 | 0.010 |
| main | run | 2 | 0.005 |
| main | new | 8 | 0.004 |
| require_config | load_config | 2 | 0.003 |
| run_with_config | load_config | 2 | 0.003 |
| main | from | 1 | 0.002 |

Artifacts: `int_05_aspects_combined.mtl.profile.json`, `int_05_aspects_combined.mtl.callgraph.dot`

## int_05_generic_data_pipeline.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 54.246 |
| typecheck | 4.696 |
| evaluate | 2.541 |
| total | 61.483 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.051 |
| inference | 1.383 |
| solve | 2.610 |
| scheme_env | 0.011 |
| construction | 0.273 |
| finalize | 0.009 |

Typechecker counters: `solve_calls=183`, `constraints_processed=390`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 2.271 | 0.691 |
| filter_array | 4 | 0.344 | 0.277 |
| map_array | 3 | 0.289 | 0.212 |
| <closure> | 105 | 0.220 | 0.200 |
| zip_with | 3 | 0.181 | 0.162 |
| any | 3 | 0.167 | 0.136 |
| all | 3 | 0.125 | 0.099 |
| take | 2 | 0.102 | 0.097 |
| maybe_map | 4 | 0.093 | 0.087 |
| maybe_get_or | 6 | 0.075 | 0.075 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 2.271 |
| main | filter_array | 4 | 0.344 |
| main | map_array | 3 | 0.289 |
| main | zip_with | 3 | 0.181 |
| main | any | 3 | 0.167 |
| main | all | 3 | 0.125 |
| main | take | 2 | 0.102 |
| main | maybe_map | 4 | 0.093 |
| main | maybe_get_or | 6 | 0.075 |
| map_array | <closure> | 25 | 0.072 |

Artifacts: `int_05_generic_data_pipeline.mtl.profile.json`, `int_05_generic_data_pipeline.mtl.callgraph.dot`

## int_06_display.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 21.040 |
| typecheck | 1.734 |
| evaluate | 0.510 |
| total | 23.283 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.055 |
| inference | 0.494 |
| solve | 0.716 |
| scheme_env | 0.004 |
| construction | 0.175 |
| finalize | 0.003 |

Typechecker counters: `solve_calls=123`, `constraints_processed=204`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.265 | 0.152 |
| render_score | 3 | 0.044 | 0.032 |
| println | 4 | 0.025 | 0.025 |
| format | 7 | 0.022 | 0.019 |
| next | 5 | 0.015 | 0.015 |
| parse_score | 3 | 0.004 | 0.004 |
| from | 4 | 0.004 | 0.004 |
| string_concat | 21 | 0.002 | 0.002 |
| f64::to_string | 10 | 0.002 | 0.002 |
| len | 1 | 0.002 | 0.002 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.265 |
| main | render_score | 3 | 0.044 |
| main | println | 4 | 0.025 |
| main | format | 5 | 0.016 |
| main | next | 5 | 0.015 |
| render_score | format | 2 | 0.006 |
| render_score | parse_score | 3 | 0.004 |
| main | len | 1 | 0.002 |
| main | from | 3 | 0.002 |
| main | f64::to_string | 8 | 0.002 |

Artifacts: `int_06_display.mtl.profile.json`, `int_06_display.mtl.callgraph.dot`

## int_07_pub_declarations.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 3.312 |
| typecheck | 0.172 |
| evaluate | 0.098 |
| total | 3.582 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.029 |
| inference | 0.031 |
| solve | 0.028 |
| scheme_env | 0.003 |
| construction | 0.032 |
| finalize | 0.002 |

Typechecker counters: `solve_calls=15`, `constraints_processed=35`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.022 | 0.018 |
| classify | 1 | 0.002 | 0.002 |
| distance | 1 | 0.002 | 0.002 |
| assert | 4 | 0.000 | 0.000 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.022 |
| main | classify | 1 | 0.002 |
| main | distance | 1 | 0.002 |
| main | assert | 4 | 0.000 |

Artifacts: `int_07_pub_declarations.mtl.profile.json`, `int_07_pub_declarations.mtl.callgraph.dot`

## int_08_std_core_paths.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 18.827 |
| typecheck | 2.415 |
| evaluate | 0.524 |
| total | 21.767 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.039 |
| inference | 0.694 |
| solve | 1.281 |
| scheme_env | 0.005 |
| construction | 0.172 |
| finalize | 0.006 |

Typechecker counters: `solve_calls=76`, `constraints_processed=217`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.279 | 0.150 |
| add_parsed | 3 | 0.054 | 0.043 |
| find_in | 5 | 0.037 | 0.036 |
| double_parsed | 2 | 0.021 | 0.017 |
| parse_positive | 9 | 0.017 | 0.016 |
| perhaps_to_result | 4 | 0.006 | 0.006 |
| map_some | 3 | 0.005 | 0.005 |
| from | 2 | 0.004 | 0.003 |
| Array::len | 21 | 0.001 | 0.001 |
| string_concat | 6 | 0.001 | 0.001 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.279 |
| main | add_parsed | 3 | 0.054 |
| main | find_in | 5 | 0.037 |
| main | double_parsed | 2 | 0.021 |
| add_parsed | parse_positive | 5 | 0.008 |
| main | parse_positive | 2 | 0.006 |
| main | perhaps_to_result | 4 | 0.006 |
| main | map_some | 3 | 0.005 |
| double_parsed | parse_positive | 2 | 0.004 |
| add_parsed | from | 2 | 0.004 |

Artifacts: `int_08_std_core_paths.mtl.profile.json`, `int_08_std_core_paths.mtl.callgraph.dot`

## int_09_numeric_pipeline.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 9.007 |
| typecheck | 1.068 |
| evaluate | 0.308 |
| total | 10.383 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.036 |
| inference | 0.322 |
| solve | 0.448 |
| scheme_env | 0.004 |
| construction | 0.105 |
| finalize | 0.004 |

Typechecker counters: `solve_calls=76`, `constraints_processed=135`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.128 | 0.093 |
| mean_f32 | 1 | 0.007 | 0.007 |
| sum_i32 | 1 | 0.007 | 0.007 |
| scale | 5 | 0.007 | 0.007 |
| bucket_of | 5 | 0.006 | 0.005 |
| List::push | 14 | 0.003 | 0.003 |
| List::get | 3 | 0.001 | 0.001 |
| assert | 21 | 0.001 | 0.001 |
| List::new | 3 | 0.001 | 0.001 |
| List::as_slice | 3 | 0.000 | 0.000 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.128 |
| main | mean_f32 | 1 | 0.007 |
| main | sum_i32 | 1 | 0.007 |
| main | scale | 5 | 0.007 |
| main | bucket_of | 5 | 0.006 |
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
| parse | 9.571 |
| typecheck | 0.980 |
| evaluate | 0.418 |
| total | 10.969 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.036 |
| inference | 0.299 |
| solve | 0.386 |
| scheme_env | 0.004 |
| construction | 0.101 |
| finalize | 0.004 |

Typechecker counters: `solve_calls=96`, `constraints_processed=158`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.296 | 0.151 |
| count_uppercase | 3 | 0.061 | 0.042 |
| to_upper | 7 | 0.035 | 0.026 |
| to_lower | 7 | 0.035 | 0.026 |
| is_uppercase | 24 | 0.029 | 0.028 |
| is_lowercase | 9 | 0.012 | 0.011 |
| List::push | 20 | 0.003 | 0.003 |
| u32::From<Char>::from | 60 | 0.003 | 0.003 |
| assert | 28 | 0.001 | 0.001 |
| Char::From<u32>::from | 16 | 0.001 | 0.001 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.296 |
| main | count_uppercase | 3 | 0.061 |
| main | to_upper | 7 | 0.035 |
| main | to_lower | 7 | 0.035 |
| count_uppercase | is_uppercase | 15 | 0.018 |
| to_upper | is_lowercase | 7 | 0.009 |
| to_lower | is_uppercase | 7 | 0.009 |
| main | List::push | 20 | 0.003 |
| main | is_lowercase | 2 | 0.003 |
| main | is_uppercase | 2 | 0.003 |

Artifacts: `int_10_char_processing.mtl.profile.json`, `int_10_char_processing.mtl.callgraph.dot`

## int_11_generic_sized.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 20.965 |
| typecheck | 3.244 |
| evaluate | 0.716 |
| total | 24.925 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 0.041 |
| inference | 0.869 |
| solve | 1.863 |
| scheme_env | 0.006 |
| construction | 0.198 |
| finalize | 0.004 |

Typechecker counters: `solve_calls=128`, `constraints_processed=283`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 0.451 | 0.166 |
| map_to_list | 4 | 0.160 | 0.142 |
| clamp | 8 | 0.081 | 0.081 |
| zip_add_i32 | 1 | 0.020 | 0.018 |
| all_positive_i32 | 2 | 0.016 | 0.015 |
| <closure> | 18 | 0.014 | 0.014 |
| List::get | 23 | 0.005 | 0.005 |
| List::push | 39 | 0.005 | 0.005 |
| List::new | 11 | 0.001 | 0.001 |
| assert | 33 | 0.001 | 0.001 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 0.451 |
| main | map_to_list | 4 | 0.160 |
| main | clamp | 8 | 0.081 |
| main | zip_add_i32 | 1 | 0.020 |
| main | all_positive_i32 | 2 | 0.016 |
| map_to_list | <closure> | 18 | 0.014 |
| map_to_list | List::push | 18 | 0.003 |
| main | List::get | 12 | 0.003 |
| main | List::push | 18 | 0.002 |
| main | assert | 33 | 0.001 |

Artifacts: `int_11_generic_sized.mtl.profile.json`, `int_11_generic_sized.mtl.callgraph.dot`
