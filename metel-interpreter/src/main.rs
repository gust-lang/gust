// See src/lib.rs for the rationale — this binary target re-declares the same
// modules as a separate crate root, so the same crate-level allow is needed
// here too.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless
)]

use std::process;

use clap::Parser;

mod ast;
mod coherence;
mod elaborator;
mod error;
mod evaluator;
mod module_loader;
mod module_paths;
mod name_resolver;
mod native_keys;
mod parser;
mod path_normalizer;
mod pipeline;
mod reference_resolver;
mod stdlib;
mod symbols;
mod typechecker;
mod typed_ast;
mod typeinference;
mod types;

use error::MetelError;

#[derive(Parser)]
#[command(name = "metel")]
#[command(version)]
#[command(about = "Metel interpreter")]
#[command(long_about = "A tree-walk interpreter for the metel programming language")]
struct Args {
    /// Path to the \.mtl file to execute
    #[arg(value_name = "FILE")]
    file: String,

    /// Print the AST and exit without executing
    #[arg(long)]
    debug_ast: bool,
}

fn main() {
    let args = Args::parse();

    if let Err(e) = run(&args.file, args.debug_ast) {
        eprintln!("{}", e);
        process::exit(1);
    }
}

fn run(filename: &str, debug_ast: bool) -> Result<(), MetelError> {
    if !debug_ast {
        pipeline::run_file(filename, &pipeline::RunOptions::default())?;
        return Ok(());
    }

    // 1. Load modules
    let graph = module_loader::load_root(filename)?;

    if debug_ast {
        for m in graph.modules.iter() {
            println!("=== {:?} ===\n{:#?}", m.module_path, m.program);
        }
        return Ok(());
    }
    Ok(())
}
