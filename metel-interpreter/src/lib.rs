//! Metel language interpreter library.
//! Exposes modules for use in tests and external code.

pub mod ast;
pub mod elaborator;
pub mod error;
pub mod evaluator;
pub mod module_loader;
pub mod module_paths;
pub mod name_resolver;
pub mod native_keys;
pub mod parser;
pub mod path_normalizer;
pub mod pipeline;
pub mod symbols;
pub mod typechecker;
pub mod typed_ast;
pub mod typeinference;
pub mod types;
