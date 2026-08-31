# Evaluator Integration Benchmark Summary

| Fixture | Mean (ms) | Min (ms) | Max (ms) | Stddev (ms) | Evaluate Phase (ms) |
|---|---:|---:|---:|---:|---:|
| deep_type.mtl | 585.131 | 540.550 | 640.392 | 29.684 | 283.709 |
| id_chain.mtl | 564.164 | 516.359 | 692.741 | 40.197 | 139.533 |
| solve_storm.mtl | 948.669 | 888.955 | 1010.916 | 36.065 | 225.576 |

## deep_type.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 136.126 |
| typecheck | 165.294 |
| evaluate | 283.709 |
| total | 585.131 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 4.391 |
| inference | 59.330 |
| solve | 79.933 |
| scheme_env | 0.104 |
| construction | 10.681 |
| finalize | 0.497 |

Typechecker counters: `solve_calls=364`, `constraints_processed=429`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 139.448 | 5.577 |
| wrap | 90 | 133.870 | 133.781 |
| List::from | 90 | 0.089 | 0.089 |
| List::len | 1 | 0.001 | 0.001 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 139.448 |
| main | wrap | 90 | 133.870 |
| wrap | List::from | 90 | 0.089 |
| main | List::len | 1 | 0.001 |

Artifacts: `deep_type.mtl.profile.json`, `deep_type.mtl.callgraph.dot`

## id_chain.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 374.603 |
| typecheck | 50.027 |
| evaluate | 139.533 |
| total | 564.164 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 4.171 |
| inference | 20.821 |
| solve | 13.287 |
| scheme_env | 0.100 |
| construction | 4.644 |
| finalize | 0.086 |

Typechecker counters: `solve_calls=711`, `constraints_processed=817`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 17.425 | 1.588 |
| id | 400 | 15.838 | 15.838 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 17.425 |
| main | id | 400 | 15.838 |

Artifacts: `id_chain.mtl.profile.json`, `id_chain.mtl.callgraph.dot`

## solve_storm.mtl

### Phase Mean Timings

| Phase | Mean (ms) |
|---|---:|
| parse | 615.487 |
| typecheck | 107.605 |
| evaluate | 225.576 |
| total | 948.669 |

### Typechecker Sub-Phases

| Sub-phase | Mean (ms) |
|---|---:|
| registry | 4.242 |
| inference | 46.377 |
| solve | 40.993 |
| scheme_env | 0.093 |
| construction | 6.638 |
| finalize | 0.082 |

Typechecker counters: `solve_calls=858`, `constraints_processed=1595`

### Hottest Functions

| Function | Calls | Inclusive (ms) | Self (ms) |
|---|---:|---:|---:|
| main | 1 | 94.738 | 34.744 |
| map | 167 | 39.917 | 33.776 |
| filter | 83 | 19.667 | 16.720 |
| <closure> | 1255 | 8.560 | 8.560 |
| fold | 1 | 0.407 | 0.323 |
| List::new | 250 | 0.307 | 0.307 |
| List::push | 1250 | 0.261 | 0.261 |
| List::as_slice | 251 | 0.044 | 0.044 |
| List::from | 1 | 0.003 | 0.003 |

### Hottest Edges

| Caller | Callee | Calls | Inclusive (ms) |
|---|---|---:|---:|
| <entry> | main | 1 | 94.738 |
| main | map | 167 | 39.917 |
| main | filter | 83 | 19.667 |
| map | <closure> | 835 | 5.722 |
| filter | <closure> | 415 | 2.754 |
| main | fold | 1 | 0.407 |
| map | List::new | 167 | 0.213 |
| map | List::push | 835 | 0.176 |
| filter | List::new | 83 | 0.094 |
| filter | List::push | 415 | 0.085 |

Artifacts: `solve_storm.mtl.profile.json`, `solve_storm.mtl.callgraph.dot`
