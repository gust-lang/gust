use std::fs;
use std::time::Instant;

use serde::Serialize;

use crate::elaborator;
use crate::error::MetelError;
use crate::evaluator::{self, EvaluationReport};
use crate::module_loader;
use crate::name_resolver;
use crate::parser;
use crate::path_normalizer;
use crate::typechecker::{self, StdPrelude, TypecheckPhaseTimings};

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub collect_evaluator_profile: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PhaseTimings {
    pub load_root_ns: u64,
    pub resolve_ns: u64,
    pub normalize_ns: u64,
    pub typecheck_ns: u64,
    pub elaborate_ns: u64,
    pub evaluate_ns: u64,
    pub total_ns: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RunReport {
    pub phase_timings: PhaseTimings,
    pub evaluation: EvaluationReport,
}

#[allow(dead_code)] // public API used by the benchmark binary
#[derive(Debug, Clone, Default, Serialize)]
pub struct EvaluatorFixturePhaseTimings {
    pub parse_ns: u64,
    pub typecheck_ns: u64,
    pub typecheck_detail: TypecheckPhaseTimings,
    pub evaluate_ns: u64,
    pub total_ns: u64,
}

#[allow(dead_code)] // public API used by the benchmark binary
#[derive(Debug, Clone, Default, Serialize)]
pub struct EvaluatorFixtureRunReport {
    pub phase_timings: EvaluatorFixturePhaseTimings,
    pub evaluation: EvaluationReport,
}

pub fn run_file(filename: &str, options: &RunOptions) -> Result<RunReport, MetelError> {
    let total_started = Instant::now();

    let started = Instant::now();
    let graph = module_loader::load_root(filename)?;
    let load_root_ns = elapsed_ns(started);

    let started = Instant::now();
    let names = name_resolver::resolve(&graph)?;
    let resolve_ns = elapsed_ns(started);

    let started = Instant::now();
    let normalized = path_normalizer::normalize(graph, &names)?;
    let normalize_ns = elapsed_ns(started);

    let started = Instant::now();
    let typed_graph = typechecker::check_graph(normalized, &names, StdPrelude::default())?;
    let typecheck_ns = elapsed_ns(started);

    let started = Instant::now();
    let elaborated = elaborator::elaborate(typed_graph, &names)?;
    let elaborate_ns = elapsed_ns(started);

    let started = Instant::now();
    let evaluation = evaluator::evaluate_graph_with_options(
        elaborated,
        evaluator::EvaluationOptions {
            collect_profile: options.collect_evaluator_profile,
        },
    )?;
    let evaluate_ns = elapsed_ns(started);

    Ok(RunReport {
        phase_timings: PhaseTimings {
            load_root_ns,
            resolve_ns,
            normalize_ns,
            typecheck_ns,
            elaborate_ns,
            evaluate_ns,
            total_ns: elapsed_ns(total_started),
        },
        evaluation,
    })
}

#[allow(dead_code)] // public API used by the benchmark binary
pub fn run_evaluator_fixture(
    filename: &str,
    options: &RunOptions,
) -> Result<EvaluatorFixtureRunReport, MetelError> {
    let total_started = Instant::now();

    let started = Instant::now();
    let source = fs::read_to_string(filename).map_err(|err| {
        MetelError::internal(format!(
            "failed to read evaluator fixture `{filename}`: {err}"
        ))
    })?;
    let program = parser::parse(&source, filename)?;
    let parse_ns = elapsed_ns(started);

    let started = Instant::now();
    let typecheck_report = typechecker::check_with_ctx_with_report(program)?;
    let typecheck_ns = elapsed_ns(started);

    let started = Instant::now();
    let evaluation = evaluator::evaluate_with_ctx_and_options(
        typecheck_report.decls,
        typecheck_report.type_ctx,
        evaluator::EvaluationOptions {
            collect_profile: options.collect_evaluator_profile,
        },
    )?;
    let evaluate_ns = elapsed_ns(started);

    Ok(EvaluatorFixtureRunReport {
        phase_timings: EvaluatorFixturePhaseTimings {
            parse_ns,
            typecheck_ns,
            typecheck_detail: typecheck_report.timings,
            evaluate_ns,
            total_ns: elapsed_ns(total_started),
        },
        evaluation,
    })
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u64::MAX as u128) as u64
}
