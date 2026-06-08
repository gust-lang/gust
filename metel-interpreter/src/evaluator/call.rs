use std::cell::RefCell;
use std::rc::Rc;

use crate::ast::Span;
use crate::error::{MetelError, RuntimeErrorCode};

use super::{
    attach_stack, eval_block, pop_frame, profiler_enter, profiler_exit, push_frame, type_of,
    ClosureBody, RuntimeCallable, RuntimeRegistry, Signal, Value,
};

/// How the receiver is bound into the callee's environment.
/// `Value` → cloned (value/&self receivers); `Shared` → Rc shared (mut self / &mut self).
/// See ADR-0036 for the dispatch design.
pub(super) enum ReceiverBinding {
    Value(Value),
    Shared(Rc<RefCell<Value>>),
}

fn call_runtime_callable(
    callable: RuntimeCallable,
    args: Vec<Value>,
    span: &Span,
    runtime: &RuntimeRegistry,
) -> Result<Signal, MetelError> {
    match callable {
        RuntimeCallable::Intrinsic { label, fun } => {
            profiler_enter(&label);
            let result = fun(args, span).map(Signal::Value).map_err(attach_stack);
            profiler_exit();
            result
        }
        RuntimeCallable::Closure(rc) => {
            let closure = (*rc).clone();
            let fn_name = closure
                .name
                .clone()
                .unwrap_or_else(|| "<closure>".to_string());
            push_frame(fn_name, span.clone());
            let mut call_env = closure.captured.clone();
            call_env.push_scope();
            for (param, val) in closure.params.iter().zip(args.iter()) {
                call_env.define(&param.name, val.clone());
            }
            let result = match &closure.body {
                ClosureBody::Typed(b) => eval_block(b, &mut call_env, runtime),
                ClosureBody::Untyped(b) => {
                    let scheme_and_ctx = closure
                        .name
                        .as_deref()
                        .and_then(|name| closure.type_ctx.as_ref().map(|ctx| (name, ctx)))
                        .and_then(|(name, type_ctx)| {
                            type_ctx.scheme_env.get(name).map(|s| (s, type_ctx))
                        });
                    match scheme_and_ctx {
                        Some((scheme, type_ctx)) => {
                            let arg_types: Vec<_> = args.iter().map(type_of::value_to_type).collect();
                            let tb = crate::typechecker::construct_generic_body(
                                scheme, &closure.params, &arg_types, b, span, type_ctx
                            )?;
                            eval_block(&tb, &mut call_env, runtime)
                        }
                        None => Err(attach_stack(MetelError::panic(
                            crate::error::RuntimeErrorCode::R0002,
                            format!("generic closure `{}` has no type context — construction-at-call-time unavailable",
                                closure.name.as_deref().unwrap_or("<anonymous>")),
                            span,
                        ))),
                    }
                }
            };
            let result = result.map_err(attach_stack);
            pop_frame();
            let sig = result?;
            Ok(match sig {
                Signal::Return(v) => Signal::Value(v),
                other => other,
            })
        }
    }
}

/// Dispatch a function call to a callable runtime value.
/// Converts `Signal::Return` at the function boundary.
pub(super) fn call_function(
    func: Value,
    args: Vec<Value>,
    span: &Span,
    runtime: &RuntimeRegistry,
) -> Result<Signal, MetelError> {
    // Auto-deref: calling through a function pointer transparently unwraps one pointer layer.
    let func = match func {
        Value::Pointer(rc) | Value::MutPointer(rc) => rc.borrow().clone(),
        other => other,
    };
    match func {
        Value::Callable(callable) => call_runtime_callable(callable, args, span, runtime),

        Value::Unit => Err(attach_stack(MetelError::panic(
            RuntimeErrorCode::R0002,
            "call: target is Unit, not a function",
            span,
        ))),

        other => Err(attach_stack(MetelError::panic(
            RuntimeErrorCode::R0010,
            format!(
                "call: expected a closure or builtin, got {:?}",
                std::mem::discriminant(&other)
            ),
            span,
        ))),
    }
}

pub(super) fn call_method_function(
    func: RuntimeCallable,
    receiver: ReceiverBinding,
    mut args: Vec<Value>,
    span: &Span,
    runtime: &RuntimeRegistry,
) -> Result<Signal, MetelError> {
    match func {
        RuntimeCallable::Closure(rc) => {
            let closure = (*rc).clone();
            let fn_name = closure
                .name
                .clone()
                .unwrap_or_else(|| "<closure>".to_string());
            push_frame(fn_name, span.clone());
            let mut call_env = closure.captured.clone();
            call_env.push_scope();
            if let Some(param) = closure.params.first() {
                match receiver {
                    ReceiverBinding::Value(value) => call_env.define(&param.name, value),
                    ReceiverBinding::Shared(cell) => call_env.define_rc(&param.name, cell),
                }
            }
            for (param, val) in closure.params.iter().skip(1).zip(args.iter()) {
                call_env.define(&param.name, val.clone());
            }
            let result = match &closure.body {
                ClosureBody::Typed(b) => eval_block(b, &mut call_env, runtime),
                ClosureBody::Untyped(b) => {
                    let scheme_and_ctx = closure
                        .name
                        .as_deref()
                        .and_then(|name| closure.type_ctx.as_ref().map(|ctx| (name, ctx)))
                        .and_then(|(name, type_ctx)| {
                            type_ctx.scheme_env.get(name).map(|s| (s, type_ctx))
                        });
                    match scheme_and_ctx {
                        Some((scheme, type_ctx)) => {
                            let arg_types: Vec<_> = args.iter().map(type_of::value_to_type).collect();
                            let tb = crate::typechecker::construct_generic_body(
                                scheme, &closure.params, &arg_types, b, span, type_ctx
                            )?;
                            eval_block(&tb, &mut call_env, runtime)
                        }
                        None => Err(attach_stack(MetelError::panic(
                            crate::error::RuntimeErrorCode::R0002,
                            format!("generic method `{}` has no type context — construction-at-call-time unavailable",
                                closure.name.as_deref().unwrap_or("<anonymous>")),
                            span,
                        ))),
                    }
                }
            };
            let result = result.map_err(attach_stack);
            pop_frame();
            let sig = result?;
            Ok(match sig {
                Signal::Return(v) => Signal::Value(v),
                other => other,
            })
        }
        callable => {
            let receiver_value = match receiver {
                ReceiverBinding::Value(value) => value,
                ReceiverBinding::Shared(cell) => cell.borrow().clone(),
            };
            args.insert(0, receiver_value);
            call_runtime_callable(callable, args, span, runtime)
        }
    }
}
