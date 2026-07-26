use std::fs;
use std::path::{Path, PathBuf};

use metel::move_check::place::Projection;
use metel::{coherence, module_loader, move_check, name_resolver, path_normalizer, typechecker};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = args
        .first()
        .map_or_else(|| PathBuf::from("tests/integration/sources"), PathBuf::from);
    let mut fixtures = Vec::new();
    collect_sources(&root, &mut fixtures);
    fixtures.sort();

    let mut newly_failing = Vec::new();
    let mut skipped_generic_bodies_user_total = 0usize;
    let mut skipped_generic_bodies_embedded_std_total = 0usize;
    let mut user_violation_count = 0usize;
    let mut user_violations_with_by_value_receiver_moves = 0usize;
    let mut embedded_std_violation_count = 0usize;
    let mut per_program_generic_skips = Vec::new();

    for (index, fixture) in fixtures.into_iter().enumerate() {
        if index > 0 && index % 50 == 0 {
            eprintln!("processed={index}");
        }
        let Ok(graph) = module_loader::load_root(&fixture) else {
            continue;
        };
        let Ok(names) = name_resolver::resolve(&graph) else {
            continue;
        };
        let Ok(normalized) = path_normalizer::normalize(graph, &names) else {
            continue;
        };
        if coherence::check(&normalized, &names).is_err() {
            continue;
        }
        let Ok(typed) = typechecker::check_graph(&normalized, &names, &typechecker::CorePrelude::default()) else {
            continue;
        };
        let report = move_check::collect_graph_violations(&typed);
        let mut user_violations = Vec::new();
        for violation in report.violations {
            if violation.use_span.filename.starts_with("<embedded std::") {
                embedded_std_violation_count += 1;
                continue;
            }
            assert_ne!(
                violation.use_span, violation.moved_span,
                "checker bug: move site reported as its own use for `{}` at {}:{}",
                violation.binding, violation.use_span.filename, violation.use_span.line
            );
            assert!(
                !is_projection_base_only_violation(&violation),
                "checker bug: projection base falsely reported for `{}`: use={:?} moved={:?}",
                violation.binding,
                violation.use_place,
                violation.moved_place
            );
            if violation.moved_by_value_receiver {
                user_violations_with_by_value_receiver_moves += 1;
            }
            user_violation_count += 1;
            user_violations.push(violation);
        }
        skipped_generic_bodies_user_total += report.skipped_generic_bodies_user;
        skipped_generic_bodies_embedded_std_total += report.skipped_generic_bodies_embedded_std;
        per_program_generic_skips.push((
            report.skipped_generic_bodies_user,
            report.skipped_generic_bodies_embedded_std,
        ));
        if !user_violations.is_empty() {
            let details: Vec<String> = user_violations
                .iter()
                .map(|violation| {
                    format!(
                        "{}:{}:{}:{}:{}->{}:{}:{}",
                        violation.use_span.filename,
                        violation.use_span.line,
                        violation.use_span.col,
                        format_place(&violation.use_place),
                        violation.binding,
                        format_place(&violation.moved_place),
                        violation.moved_span.line,
                        violation.moved_span.col
                    )
                })
                .collect();
            newly_failing.push((fixture, user_violations.len(), details));
        }
    }

    if !per_program_generic_skips.is_empty() {
        per_program_generic_skips.sort_unstable();
        let fixture_count = per_program_generic_skips.len();
        let typical = per_program_generic_skips[fixture_count / 2];
        let max_user = per_program_generic_skips
            .iter()
            .map(|(user, _)| *user)
            .max()
            .unwrap_or(0);
        let max_std = per_program_generic_skips
            .iter()
            .map(|(_, std)| *std)
            .max()
            .unwrap_or(0);
        println!(
            "typical_skipped_generic_bodies_per_program=user:{};embedded_std:{}",
            typical.0, typical.1
        );
        println!("max_skipped_generic_bodies_per_program=user:{max_user};embedded_std:{max_std}");
    }
    println!("fixtures_with_move_violations={}", newly_failing.len());
    println!("user_move_violations={user_violation_count}");
    println!(
        "user_move_violations_with_by_value_receiver_move_side={user_violations_with_by_value_receiver_moves}"
    );
    println!("embedded_std_move_violations={embedded_std_violation_count}");
    println!("skipped_generic_bodies_user_total={skipped_generic_bodies_user_total}");
    println!(
        "skipped_generic_bodies_embedded_std_total={skipped_generic_bodies_embedded_std_total}"
    );
    for (fixture, count, details) in newly_failing {
        println!("{}:{}:{}", fixture.display(), count, details.join("|"));
    }
}

fn is_projection_base_only_violation(violation: &move_check::MoveViolation) -> bool {
    let moved = &violation.moved_place;
    let used = &violation.use_place;
    moved.root() == used.root()
        && !moved.projections().is_empty()
        && violation.use_span == violation.moved_span
        && used.projections().len() + 1 == moved.projections().len()
        && used
            .projections()
            .iter()
            .zip(moved.projections().iter())
            .all(|(used, moved)| used == moved)
}

fn format_place(place: &move_check::place::Place) -> String {
    let mut rendered = place.root().to_string();
    for projection in place.projections() {
        match projection {
            Projection::Field(field) => {
                rendered.push('.');
                rendered.push_str(field);
            }
            Projection::TupleIndex(index) => {
                rendered.push('.');
                rendered.push_str(&index.to_string());
            }
            Projection::OpaqueIndex => rendered.push_str("[_]"),
        }
    }
    rendered
}

fn collect_sources(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_file() {
        if path.extension().is_some_and(|ext| ext == "mtl") {
            out.push(path.to_path_buf());
        }
        return;
    }

    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let child = entry.path();
        if child.is_dir() {
            let main = child.join("main.mtl");
            if main.is_file() {
                out.push(main);
            } else {
                collect_sources(&child, out);
            }
        } else if child.extension().is_some_and(|ext| ext == "mtl") {
            out.push(child);
        }
    }
}
