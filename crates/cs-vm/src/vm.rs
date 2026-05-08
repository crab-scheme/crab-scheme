//! Stack-based VM that interprets [`Bytecode`].

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use cs_core::{Procedure, Symbol, SymbolTable, Value};

use crate::opcode::{Bytecode, CompiledLambda, Inst};

thread_local! {
    /// Side-channel for multi-value returns within a VM tier. `values` (when
    /// passed >1 args) and `partition` write here; `call-with-values` reads.
    static VM_PENDING_VALUES: RefCell<Option<Vec<Value>>> = const { RefCell::new(None) };
    /// Side-channel for `raise` / `error`. Set by raise; read by
    /// with-exception-handler when a callee returns Err.
    static VM_PENDING_RAISE: RefCell<Option<Value>> = const { RefCell::new(None) };
}

fn take_pending_values() -> Option<Vec<Value>> {
    VM_PENDING_VALUES.with(|cell| cell.borrow_mut().take())
}

fn set_pending_values(vs: Vec<Value>) {
    VM_PENDING_VALUES.with(|cell| *cell.borrow_mut() = Some(vs));
}

fn take_pending_raise() -> Option<Value> {
    VM_PENDING_RAISE.with(|cell| cell.borrow_mut().take())
}

fn set_pending_raise(v: Value) {
    VM_PENDING_RAISE.with(|cell| *cell.borrow_mut() = Some(v));
}

#[derive(Debug, Clone)]
pub struct VmError {
    pub message: String,
}

impl VmError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
        }
    }
}

/// VM closure: a compiled lambda + the env at the point of construction.
#[derive(Debug)]
pub struct VmClosure {
    pub lambda_idx: usize,
    pub env: Rc<Env>,
    pub bc: Rc<Bytecode>,
}

impl Procedure for VmClosure {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("vm-closure")
    }
}

#[derive(Debug, Default)]
pub struct Env {
    pub bindings: RefCell<HashMap<Symbol, Value>>,
    pub parent: Option<Rc<Env>>,
}

impl Env {
    pub fn root() -> Rc<Self> {
        Rc::new(Self::default())
    }

    pub fn child(parent: Rc<Self>) -> Rc<Self> {
        Rc::new(Self {
            bindings: RefCell::new(HashMap::new()),
            parent: Some(parent),
        })
    }

    pub fn get(&self, name: Symbol) -> Option<Value> {
        if let Some(v) = self.bindings.borrow().get(&name) {
            return Some(v.clone());
        }
        if let Some(p) = &self.parent {
            return p.get(name);
        }
        None
    }

    pub fn set_existing(&self, name: Symbol, value: Value) -> bool {
        if self.bindings.borrow().contains_key(&name) {
            self.bindings.borrow_mut().insert(name, value);
            return true;
        }
        if let Some(p) = &self.parent {
            return p.set_existing(name, value);
        }
        false
    }

    pub fn define(&self, name: Symbol, value: Value) {
        self.bindings.borrow_mut().insert(name, value);
    }
}

struct Frame {
    insts: Vec<Inst>,
    ip: usize,
    env: Rc<Env>,
    /// Captured shared bytecode (so closures can resolve their lambda body).
    bc: Rc<Bytecode>,
}

pub fn run(bc: &Bytecode, top_env: Rc<Env>, syms: &mut SymbolTable) -> Result<Value, VmError> {
    let bc_rc = Rc::new(bc.clone());
    let mut stack: Vec<Value> = Vec::new();
    let mut frames: Vec<Frame> = vec![Frame {
        insts: bc.insts.clone(),
        ip: 0,
        env: top_env,
        bc: bc_rc.clone(),
    }];
    loop {
        let Some(frame) = frames.last_mut() else {
            return Err(VmError::new("vm stack underflow"));
        };
        if frame.ip >= frame.insts.len() {
            // End of frame: pop, keep top of stack as result.
            frames.pop();
            if frames.is_empty() {
                return stack
                    .pop()
                    .ok_or_else(|| VmError::new("empty stack at exit"));
            }
            continue;
        }
        let inst = frame.insts[frame.ip].clone();
        frame.ip += 1;
        match inst {
            Inst::Const(v) => stack.push(v),
            Inst::LoadVar(s) => {
                let v = frame
                    .env
                    .get(s)
                    .ok_or_else(|| VmError::new(format!("undefined variable: {}", syms.name(s))))?;
                stack.push(v);
            }
            Inst::SetVar(s) => {
                let v = stack
                    .pop()
                    .ok_or_else(|| VmError::new("stack underflow on Set"))?;
                if !frame.env.set_existing(s, v.clone()) {
                    let mut root = frame.env.clone();
                    while let Some(p) = root.parent.clone() {
                        root = p;
                    }
                    root.define(s, v);
                }
            }
            Inst::DefineGlobal(s) => {
                let v = stack
                    .pop()
                    .ok_or_else(|| VmError::new("stack underflow on Define"))?;
                let mut root = frame.env.clone();
                while let Some(p) = root.parent.clone() {
                    root = p;
                }
                root.define(s, v);
            }
            Inst::DefineLocal(s) => {
                let v = stack
                    .pop()
                    .ok_or_else(|| VmError::new("stack underflow on DefineLocal"))?;
                frame.env.define(s, v);
            }
            Inst::Pop => {
                stack
                    .pop()
                    .ok_or_else(|| VmError::new("stack underflow on Pop"))?;
            }
            Inst::JumpIfFalse(target) => {
                let v = stack
                    .pop()
                    .ok_or_else(|| VmError::new("stack underflow on JumpIfFalse"))?;
                if !v.is_truthy() {
                    frame.ip = target;
                }
            }
            Inst::Jump(target) => {
                frame.ip = target;
            }
            Inst::Call(n) | Inst::TailCall(n) => {
                let is_tail = matches!(inst, Inst::TailCall(_));
                if stack.len() < n + 1 {
                    return Err(VmError::new("stack underflow on Call"));
                }
                let args_start = stack.len() - n;
                let mut args: Vec<Value> = stack.drain(args_start..).collect();
                let mut func = stack
                    .pop()
                    .ok_or_else(|| VmError::new("missing function on Call"))?;
                // Native HO: (map proc list) — produce a list.
                if let Value::Procedure(p) = &func {
                    if p.as_any().downcast_ref::<VmMap>().is_some() {
                        if args.len() < 2 {
                            return Err(VmError::new("map: needs proc + list"));
                        }
                        let proc_val = args.remove(0);
                        let lists: Vec<Vec<Value>> = args
                            .iter()
                            .map(collect_proper_list)
                            .collect::<Result<_, _>>()?;
                        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
                        let mut out = Vec::with_capacity(n);
                        for i in 0..n {
                            let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
                            let r = vm_call_sync(&proc_val, &row, syms)?;
                            out.push(r);
                        }
                        let result = Value::list(out);
                        stack.push(result);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-map"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmForEach>().is_some() {
                        if args.len() < 2 {
                            return Err(VmError::new("for-each: needs proc + list"));
                        }
                        let proc_val = args.remove(0);
                        let lists: Vec<Vec<Value>> = args
                            .iter()
                            .map(collect_proper_list)
                            .collect::<Result<_, _>>()?;
                        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
                        for i in 0..n {
                            let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
                            vm_call_sync(&proc_val, &row, syms)?;
                        }
                        stack.push(Value::Unspecified);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-for-each"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmFilter>().is_some() {
                        if args.len() != 2 {
                            return Err(VmError::new("filter: needs pred + list"));
                        }
                        let pred = args.remove(0);
                        let items = collect_proper_list(&args[0])?;
                        let mut kept = Vec::new();
                        for item in items {
                            let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
                            if r.is_truthy() {
                                kept.push(item);
                            }
                        }
                        stack.push(Value::list(kept));
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-filter"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmFind>().is_some() {
                        if args.len() != 2 {
                            return Err(VmError::new("find: needs pred + list"));
                        }
                        let pred = args.remove(0);
                        let items = collect_proper_list(&args[0])?;
                        let mut found = Value::Boolean(false);
                        for item in items {
                            let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
                            if r.is_truthy() {
                                found = item;
                                break;
                            }
                        }
                        stack.push(found);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-find"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmAny>().is_some() {
                        if args.len() < 2 {
                            return Err(VmError::new("any: needs pred + list"));
                        }
                        let pred = args.remove(0);
                        let lists: Vec<Vec<Value>> = args
                            .iter()
                            .map(collect_proper_list)
                            .collect::<Result<_, _>>()?;
                        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
                        let mut result = Value::Boolean(false);
                        for i in 0..n {
                            let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
                            let r = vm_call_sync(&pred, &row, syms)?;
                            if r.is_truthy() {
                                result = r;
                                break;
                            }
                        }
                        stack.push(result);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-any"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmEvery>().is_some() {
                        if args.len() < 2 {
                            return Err(VmError::new("every: needs pred + list"));
                        }
                        let pred = args.remove(0);
                        let lists: Vec<Vec<Value>> = args
                            .iter()
                            .map(collect_proper_list)
                            .collect::<Result<_, _>>()?;
                        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
                        let mut result = Value::Boolean(true);
                        for i in 0..n {
                            let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
                            let r = vm_call_sync(&pred, &row, syms)?;
                            if !r.is_truthy() {
                                result = Value::Boolean(false);
                                break;
                            }
                            result = r;
                        }
                        stack.push(result);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-every"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmFoldLeft>().is_some() {
                        if args.len() < 3 {
                            return Err(VmError::new("fold-left: needs proc + init + list"));
                        }
                        let proc_val = args.remove(0);
                        let mut acc = args.remove(0);
                        let lists: Vec<Vec<Value>> = args
                            .iter()
                            .map(collect_proper_list)
                            .collect::<Result<_, _>>()?;
                        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
                        for i in 0..n {
                            let mut row: Vec<Value> = vec![acc.clone()];
                            for l in &lists {
                                row.push(l[i].clone());
                            }
                            acc = vm_call_sync(&proc_val, &row, syms)?;
                        }
                        stack.push(acc);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-fold-left"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmFoldRight>().is_some() {
                        if args.len() < 3 {
                            return Err(VmError::new("fold-right: needs proc + init + list"));
                        }
                        let proc_val = args.remove(0);
                        let init = args.remove(0);
                        let lists: Vec<Vec<Value>> = args
                            .iter()
                            .map(collect_proper_list)
                            .collect::<Result<_, _>>()?;
                        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
                        let mut acc = init;
                        for i in (0..n).rev() {
                            let mut row: Vec<Value> = Vec::with_capacity(lists.len() + 1);
                            for l in &lists {
                                row.push(l[i].clone());
                            }
                            row.push(acc);
                            acc = vm_call_sync(&proc_val, &row, syms)?;
                        }
                        stack.push(acc);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-fold-right"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmReduce>().is_some() {
                        if args.len() != 3 {
                            return Err(VmError::new("reduce: needs proc + default + list"));
                        }
                        let proc_val = args.remove(0);
                        let default = args.remove(0);
                        let items = collect_proper_list(&args[0])?;
                        let result = if items.is_empty() {
                            default
                        } else {
                            let mut acc = items[0].clone();
                            for item in &items[1..] {
                                acc = vm_call_sync(&proc_val, &[acc, item.clone()], syms)?;
                            }
                            acc
                        };
                        stack.push(result);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-reduce"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmCount>().is_some() {
                        if args.len() < 2 {
                            return Err(VmError::new("count: needs pred + list"));
                        }
                        let pred = args.remove(0);
                        let lists: Vec<Vec<Value>> = args
                            .iter()
                            .map(collect_proper_list)
                            .collect::<Result<_, _>>()?;
                        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
                        let mut total: i64 = 0;
                        for i in 0..n {
                            let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
                            let r = vm_call_sync(&pred, &row, syms)?;
                            if r.is_truthy() {
                                total += 1;
                            }
                        }
                        stack.push(Value::fixnum(total));
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-count"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmPartition>().is_some() {
                        if args.len() != 2 {
                            return Err(VmError::new("partition: needs pred + list"));
                        }
                        let pred = args.remove(0);
                        let items = collect_proper_list(&args[0])?;
                        let mut yes = Vec::new();
                        let mut no = Vec::new();
                        for item in items {
                            let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
                            if r.is_truthy() {
                                yes.push(item);
                            } else {
                                no.push(item);
                            }
                        }
                        set_pending_values(vec![Value::list(yes), Value::list(no)]);
                        stack.push(Value::Unspecified);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-partition"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmValues>().is_some() {
                        if args.len() == 1 {
                            stack.push(args.remove(0));
                        } else {
                            set_pending_values(args.clone());
                            stack.push(Value::Unspecified);
                        }
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-values"));
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmCallWithValues>().is_some() {
                        if args.len() != 2 {
                            return Err(VmError::new("call-with-values: 2 args"));
                        }
                        let producer = args.remove(0);
                        let consumer = args.remove(0);
                        let prev = take_pending_values();
                        let prod_result = vm_call_sync(&producer, &[], syms)?;
                        let values = if let Some(vs) = take_pending_values() {
                            vs
                        } else {
                            vec![prod_result]
                        };
                        if let Some(prev) = prev {
                            set_pending_values(prev);
                        }
                        let r = vm_call_sync(&consumer, &values, syms)?;
                        stack.push(r);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack.pop().ok_or_else(|| {
                                    VmError::new("empty stack at tail-call-with-values")
                                });
                            }
                        }
                        continue;
                    }
                    // Vector / string / hashtable / sort / unfold HO ops.
                    if is_pure_ho_marker(p.as_ref()) {
                        let r = ho_apply(&func, &args, syms)?;
                        stack.push(r);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack
                                    .pop()
                                    .ok_or_else(|| VmError::new("empty stack at tail-ho"));
                            }
                        }
                        continue;
                    }
                    // `raise` / `error` / `with-exception-handler`.
                    if p.as_any().downcast_ref::<VmRaise>().is_some() {
                        if args.len() != 1 {
                            return Err(VmError::new("raise: 1 arg"));
                        }
                        set_pending_raise(args.remove(0));
                        return Err(VmError::new("__raised__"));
                    }
                    if p.as_any().downcast_ref::<VmErrorFn>().is_some() {
                        if args.is_empty() {
                            return Err(VmError::new("error: needs at least 1 arg"));
                        }
                        let msg = match &args[0] {
                            Value::String(s) => s.borrow().clone(),
                            other => format!("{}", other),
                        };
                        let irritants: Vec<Value> = args.drain(1..).collect();
                        set_pending_raise(make_vm_condition(msg, irritants));
                        return Err(VmError::new("__raised__"));
                    }
                    if p.as_any()
                        .downcast_ref::<VmWithExceptionHandler>()
                        .is_some()
                    {
                        if args.len() != 2 {
                            return Err(VmError::new("with-exception-handler: 2 args"));
                        }
                        let handler = args.remove(0);
                        let thunk = args.remove(0);
                        let prev = take_pending_raise();
                        let res = vm_call_sync(&thunk, &[], syms);
                        let final_val = match res {
                            Ok(v) => {
                                if let Some(prev) = prev {
                                    set_pending_raise(prev);
                                }
                                v
                            }
                            Err(e) => {
                                if e.message == "__raised__" {
                                    let cond =
                                        take_pending_raise().unwrap_or(Value::Boolean(false));
                                    if let Some(prev) = prev {
                                        set_pending_raise(prev);
                                    }
                                    // If the handler itself raises, repropagate.
                                    match vm_call_sync(&handler, &[cond], syms) {
                                        Ok(v) => v,
                                        Err(e2) => {
                                            return Err(e2);
                                        }
                                    }
                                } else {
                                    if let Some(prev) = prev {
                                        set_pending_raise(prev);
                                    }
                                    return Err(e);
                                }
                            }
                        };
                        stack.push(final_val);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack.pop().ok_or_else(|| {
                                    VmError::new("empty stack at tail-with-exception-handler")
                                });
                            }
                        }
                        continue;
                    }
                    if p.as_any().downcast_ref::<VmDynamicWind>().is_some() {
                        if args.len() != 3 {
                            return Err(VmError::new("dynamic-wind: 3 args"));
                        }
                        let before = args.remove(0);
                        let thunk = args.remove(0);
                        let after = args.remove(0);
                        // Call before, thunk, after; even on error, after must
                        // run. Tail-position semantics get the thunk's result.
                        vm_call_sync(&before, &[], syms)?;
                        let res = vm_call_sync(&thunk, &[], syms);
                        let after_res = vm_call_sync(&after, &[], syms);
                        // Surface thunk error first; otherwise after error.
                        let v = match (res, after_res) {
                            (Ok(v), Ok(_)) => v,
                            (Err(e), _) => return Err(e),
                            (Ok(_), Err(e)) => return Err(e),
                        };
                        stack.push(v);
                        if is_tail {
                            frames.pop();
                            if frames.is_empty() {
                                return stack.pop().ok_or_else(|| {
                                    VmError::new("empty stack at tail-dynamic-wind")
                                });
                            }
                        }
                        continue;
                    }
                }
                // Handle (apply proc a1 a2 ... rest-list)
                if let Value::Procedure(p) = &func {
                    if p.as_any().downcast_ref::<VmApply>().is_some() {
                        if args.is_empty() {
                            return Err(VmError::new("apply: needs at least 2 arguments"));
                        }
                        // Last arg is the list to spread.
                        let list_arg = args.pop().unwrap();
                        let inner_proc = args.remove(0);
                        let mut spread = Vec::new();
                        let mut cur = list_arg;
                        loop {
                            match cur {
                                Value::Null => break,
                                Value::Pair(pair) => {
                                    spread.push(pair.car.borrow().clone());
                                    cur = pair.cdr.borrow().clone();
                                }
                                other => {
                                    return Err(VmError::new(format!(
                                        "apply: last arg must be a proper list, got {}",
                                        other.type_name()
                                    )));
                                }
                            }
                        }
                        // Replace args with: prefix + spread.
                        args.extend(spread);
                        func = inner_proc;
                        // After apply rewrite: if the new func is itself a HO
                        // marker or values/cwv, handle it directly via the
                        // shared helpers (the inline arms above already ran
                        // for the original `apply` proc, not the new one).
                        if let Value::Procedure(p2) = &func {
                            let any2 = p2.as_any();
                            if any2.downcast_ref::<VmMap>().is_some()
                                || any2.downcast_ref::<VmForEach>().is_some()
                                || any2.downcast_ref::<VmFilter>().is_some()
                                || any2.downcast_ref::<VmFind>().is_some()
                                || any2.downcast_ref::<VmAny>().is_some()
                                || any2.downcast_ref::<VmEvery>().is_some()
                                || any2.downcast_ref::<VmFoldLeft>().is_some()
                                || any2.downcast_ref::<VmFoldRight>().is_some()
                                || any2.downcast_ref::<VmReduce>().is_some()
                                || any2.downcast_ref::<VmCount>().is_some()
                                || any2.downcast_ref::<VmPartition>().is_some()
                                || is_pure_ho_marker(p2.as_ref())
                            {
                                let r = ho_apply(&func, &args, syms)?;
                                stack.push(r);
                                if is_tail {
                                    frames.pop();
                                    if frames.is_empty() {
                                        return stack.pop().ok_or_else(|| {
                                            VmError::new("empty stack at tail-apply-ho")
                                        });
                                    }
                                }
                                continue;
                            }
                            if any2.downcast_ref::<VmValues>().is_some() {
                                if args.len() == 1 {
                                    stack.push(args.remove(0));
                                } else {
                                    set_pending_values(args.clone());
                                    stack.push(Value::Unspecified);
                                }
                                if is_tail {
                                    frames.pop();
                                    if frames.is_empty() {
                                        return stack.pop().ok_or_else(|| {
                                            VmError::new("empty stack at tail-apply-values")
                                        });
                                    }
                                }
                                continue;
                            }
                            if any2.downcast_ref::<VmCallWithValues>().is_some() {
                                if args.len() != 2 {
                                    return Err(VmError::new("call-with-values: 2 args"));
                                }
                                let producer = args.remove(0);
                                let consumer = args.remove(0);
                                let prev = take_pending_values();
                                let prod_result = vm_call_sync(&producer, &[], syms)?;
                                let values = if let Some(vs) = take_pending_values() {
                                    vs
                                } else {
                                    vec![prod_result]
                                };
                                if let Some(prev) = prev {
                                    set_pending_values(prev);
                                }
                                let r = vm_call_sync(&consumer, &values, syms)?;
                                stack.push(r);
                                if is_tail {
                                    frames.pop();
                                    if frames.is_empty() {
                                        return stack.pop().ok_or_else(|| {
                                            VmError::new("empty stack at tail-apply-cwv")
                                        });
                                    }
                                }
                                continue;
                            }
                        }
                        // Fall through to closure/builtin dispatch below.
                    }
                }
                match &func {
                    Value::Procedure(p) => {
                        let any = p.as_any();
                        // Parameter: 0 args reads, 1 arg writes.
                        if let Some(param) = any.downcast_ref::<cs_core::Parameter>() {
                            let r = if args.is_empty() {
                                param.cell.borrow().clone()
                            } else if args.len() == 1 {
                                *param.cell.borrow_mut() = args.remove(0);
                                Value::Unspecified
                            } else {
                                return Err(VmError::new("parameter: 0 or 1 arg"));
                            };
                            stack.push(r);
                            if is_tail {
                                frames.pop();
                                if frames.is_empty() {
                                    return stack.pop().ok_or_else(|| {
                                        VmError::new("empty stack at tail-parameter")
                                    });
                                }
                            }
                            continue;
                        }
                        if let Some(closure) = any.downcast_ref::<VmClosure>() {
                            let lam = &closure.bc.lambdas[closure.lambda_idx];
                            if !lambda_arity_ok(lam, args.len()) {
                                return Err(VmError::new("arity mismatch"));
                            }
                            let new_env = Env::child(closure.env.clone());
                            for (name, v) in lam.params.iter().zip(args.iter()) {
                                new_env.define(*name, v.clone());
                            }
                            if let Some(rest_name) = lam.rest {
                                let rest = &args[lam.params.len()..];
                                new_env.define(rest_name, Value::list(rest.iter().cloned()));
                            }
                            if is_tail {
                                // Replace current frame instead of pushing.
                                let last = frames.last_mut().unwrap();
                                last.insts = lam.body.clone();
                                last.ip = 0;
                                last.env = new_env;
                                last.bc = closure.bc.clone();
                            } else {
                                frames.push(Frame {
                                    insts: lam.body.clone(),
                                    ip: 0,
                                    env: new_env,
                                    bc: closure.bc.clone(),
                                });
                            }
                        } else if let Some(b) = any.downcast_ref::<VmBuiltin>() {
                            let r = (b.f)(&args)
                                .map_err(|e| VmError::new(format!("{}: {}", b.name, e)))?;
                            stack.push(r);
                            if is_tail {
                                frames.pop();
                                if frames.is_empty() {
                                    return stack.pop().ok_or_else(|| {
                                        VmError::new("empty stack at tail-builtin")
                                    });
                                }
                            }
                        } else if let Some(b) = any.downcast_ref::<VmBuiltinSyms>() {
                            let r = (b.f)(&args, syms)
                                .map_err(|e| VmError::new(format!("{}: {}", b.name, e)))?;
                            stack.push(r);
                            if is_tail {
                                frames.pop();
                                if frames.is_empty() {
                                    return stack.pop().ok_or_else(|| {
                                        VmError::new("empty stack at tail-builtin")
                                    });
                                }
                            }
                        } else {
                            return Err(VmError::new(
                                "vm: unsupported procedure type (no cross-tier bridge)",
                            ));
                        }
                    }
                    other => {
                        return Err(VmError::new(format!(
                            "call to non-procedure ({})",
                            other.type_name()
                        )));
                    }
                }
            }
            Inst::MakeClosure(idx) => {
                let cl = VmClosure {
                    lambda_idx: idx,
                    env: frame.env.clone(),
                    bc: frame.bc.clone(),
                };
                let p: Rc<dyn Procedure> = Rc::new(cl);
                stack.push(Value::Procedure(p));
            }
            Inst::Return => {
                // Ends current frame; preserve top of stack as return.
                frames.pop();
                if frames.is_empty() {
                    return stack
                        .pop()
                        .ok_or_else(|| VmError::new("empty stack on Return"));
                }
            }
        }
    }
}

fn lambda_arity_ok(lam: &CompiledLambda, n: usize) -> bool {
    if lam.rest.is_some() {
        n >= lam.params.len()
    } else {
        n == lam.params.len()
    }
}

/// A simple builtin-procedure type for VM consumers. The VM dispatches it
/// when a `Call` finds a procedure whose underlying type is `VmBuiltin`.
/// Embedders constructing VM environments use [`make_vm_builtin`] to install.
pub type VmBuiltinFn = fn(&[Value]) -> Result<Value, String>;

/// Builtin requiring access to the symbol table (symbol↔string, gensym,
/// display/write that resolve symbol names).
pub type VmBuiltinSymsFn = fn(&[Value], &mut SymbolTable) -> Result<Value, String>;

#[derive(Debug)]
pub struct VmBuiltin {
    pub name: &'static str,
    pub f: VmBuiltinFn,
}

impl Procedure for VmBuiltin {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some(self.name)
    }
}

#[derive(Debug)]
pub struct VmBuiltinSyms {
    pub name: &'static str,
    pub f: VmBuiltinSymsFn,
}

impl Procedure for VmBuiltinSyms {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some(self.name)
    }
}

pub fn make_vm_builtin(name: &'static str, f: VmBuiltinFn) -> Value {
    let p: Rc<dyn Procedure> = Rc::new(VmBuiltin { name, f });
    Value::Procedure(p)
}

pub fn make_vm_builtin_syms(name: &'static str, f: VmBuiltinSymsFn) -> Value {
    let p: Rc<dyn Procedure> = Rc::new(VmBuiltinSyms { name, f });
    Value::Procedure(p)
}

/// Marker for the `apply` builtin. The VM call dispatch recognizes this
/// type and spreads the last arg (a list) before calling the inner procedure.
#[derive(Debug)]
pub struct VmApply;

impl Procedure for VmApply {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("apply")
    }
}

pub fn make_vm_apply() -> Value {
    let p: Rc<dyn Procedure> = Rc::new(VmApply);
    Value::Procedure(p)
}

/// Marker types for native HO builtins that iterate (map/for-each/filter/find).
#[derive(Debug)]
pub struct VmMap;
#[derive(Debug)]
pub struct VmForEach;
#[derive(Debug)]
pub struct VmFilter;
#[derive(Debug)]
pub struct VmFind;
#[derive(Debug)]
pub struct VmAny;
#[derive(Debug)]
pub struct VmEvery;

impl Procedure for VmMap {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("map")
    }
}
impl Procedure for VmForEach {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("for-each")
    }
}
impl Procedure for VmFilter {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("filter")
    }
}
impl Procedure for VmFind {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("find")
    }
}
impl Procedure for VmAny {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("any")
    }
}
impl Procedure for VmEvery {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("every")
    }
}

pub fn make_vm_map() -> Value {
    Value::Procedure(Rc::new(VmMap) as Rc<dyn Procedure>)
}
pub fn make_vm_for_each() -> Value {
    Value::Procedure(Rc::new(VmForEach) as Rc<dyn Procedure>)
}
pub fn make_vm_filter() -> Value {
    Value::Procedure(Rc::new(VmFilter) as Rc<dyn Procedure>)
}
pub fn make_vm_find() -> Value {
    Value::Procedure(Rc::new(VmFind) as Rc<dyn Procedure>)
}
pub fn make_vm_any() -> Value {
    Value::Procedure(Rc::new(VmAny) as Rc<dyn Procedure>)
}
pub fn make_vm_every() -> Value {
    Value::Procedure(Rc::new(VmEvery) as Rc<dyn Procedure>)
}

/// Additional native HO marker types.
#[derive(Debug)]
pub struct VmFoldLeft;
#[derive(Debug)]
pub struct VmFoldRight;
#[derive(Debug)]
pub struct VmReduce;
#[derive(Debug)]
pub struct VmCount;
#[derive(Debug)]
pub struct VmPartition;
#[derive(Debug)]
pub struct VmValues;
#[derive(Debug)]
pub struct VmCallWithValues;

impl Procedure for VmFoldLeft {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("fold-left")
    }
}
impl Procedure for VmFoldRight {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("fold-right")
    }
}
impl Procedure for VmReduce {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("reduce")
    }
}
impl Procedure for VmCount {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("count")
    }
}
impl Procedure for VmPartition {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("partition")
    }
}
impl Procedure for VmValues {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("values")
    }
}
impl Procedure for VmCallWithValues {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn name(&self) -> Option<&str> {
        Some("call-with-values")
    }
}

pub fn make_vm_fold_left() -> Value {
    Value::Procedure(Rc::new(VmFoldLeft) as Rc<dyn Procedure>)
}
pub fn make_vm_fold_right() -> Value {
    Value::Procedure(Rc::new(VmFoldRight) as Rc<dyn Procedure>)
}
pub fn make_vm_reduce() -> Value {
    Value::Procedure(Rc::new(VmReduce) as Rc<dyn Procedure>)
}
pub fn make_vm_count() -> Value {
    Value::Procedure(Rc::new(VmCount) as Rc<dyn Procedure>)
}
pub fn make_vm_partition() -> Value {
    Value::Procedure(Rc::new(VmPartition) as Rc<dyn Procedure>)
}
pub fn make_vm_values() -> Value {
    Value::Procedure(Rc::new(VmValues) as Rc<dyn Procedure>)
}
pub fn make_vm_call_with_values() -> Value {
    Value::Procedure(Rc::new(VmCallWithValues) as Rc<dyn Procedure>)
}

/// Vector / string / hashtable HO markers.
#[derive(Debug)]
pub struct VmVectorMap;
#[derive(Debug)]
pub struct VmVectorForEach;
#[derive(Debug)]
pub struct VmVectorFold;
#[derive(Debug)]
pub struct VmVectorFilter;
#[derive(Debug)]
pub struct VmStringMap;
#[derive(Debug)]
pub struct VmStringForEach;
#[derive(Debug)]
pub struct VmHashtableWalk;
#[derive(Debug)]
pub struct VmHashtableForEach;
#[derive(Debug)]
pub struct VmHashtableFold;
#[derive(Debug)]
pub struct VmHashtableUpdate;
#[derive(Debug)]
pub struct VmUnfold;
#[derive(Debug)]
pub struct VmListSort;
#[derive(Debug)]
pub struct VmVectorSort;
#[derive(Debug)]
pub struct VmVectorSortBang;

macro_rules! impl_proc_named {
    ($t:ty, $name:expr) => {
        impl Procedure for $t {
            fn as_any(&self) -> &dyn Any {
                self
            }
            fn name(&self) -> Option<&str> {
                Some($name)
            }
        }
    };
}
impl_proc_named!(VmVectorMap, "vector-map");
impl_proc_named!(VmVectorForEach, "vector-for-each");
impl_proc_named!(VmVectorFold, "vector-fold");
impl_proc_named!(VmVectorFilter, "vector-filter");
impl_proc_named!(VmStringMap, "string-map");
impl_proc_named!(VmStringForEach, "string-for-each");
impl_proc_named!(VmHashtableWalk, "hashtable-walk");
impl_proc_named!(VmHashtableForEach, "hashtable-for-each");
impl_proc_named!(VmHashtableFold, "hashtable-fold");
impl_proc_named!(VmHashtableUpdate, "hashtable-update!");
impl_proc_named!(VmUnfold, "unfold");
impl_proc_named!(VmListSort, "list-sort");
impl_proc_named!(VmVectorSort, "vector-sort");
impl_proc_named!(VmVectorSortBang, "vector-sort!");

#[derive(Debug)]
pub struct VmTabulate;
#[derive(Debug)]
pub struct VmRemove;
#[derive(Debug)]
pub struct VmForce;
impl_proc_named!(VmTabulate, "tabulate");
impl_proc_named!(VmRemove, "remove");
impl_proc_named!(VmForce, "force");
pub fn make_vm_tabulate() -> Value {
    Value::Procedure(Rc::new(VmTabulate) as Rc<dyn Procedure>)
}
pub fn make_vm_remove() -> Value {
    Value::Procedure(Rc::new(VmRemove) as Rc<dyn Procedure>)
}
pub fn make_vm_force() -> Value {
    Value::Procedure(Rc::new(VmForce) as Rc<dyn Procedure>)
}

pub fn make_vm_vector_map() -> Value {
    Value::Procedure(Rc::new(VmVectorMap) as Rc<dyn Procedure>)
}
pub fn make_vm_vector_for_each() -> Value {
    Value::Procedure(Rc::new(VmVectorForEach) as Rc<dyn Procedure>)
}
pub fn make_vm_vector_fold() -> Value {
    Value::Procedure(Rc::new(VmVectorFold) as Rc<dyn Procedure>)
}
pub fn make_vm_vector_filter() -> Value {
    Value::Procedure(Rc::new(VmVectorFilter) as Rc<dyn Procedure>)
}
pub fn make_vm_string_map() -> Value {
    Value::Procedure(Rc::new(VmStringMap) as Rc<dyn Procedure>)
}
pub fn make_vm_string_for_each() -> Value {
    Value::Procedure(Rc::new(VmStringForEach) as Rc<dyn Procedure>)
}
pub fn make_vm_hashtable_walk() -> Value {
    Value::Procedure(Rc::new(VmHashtableWalk) as Rc<dyn Procedure>)
}
pub fn make_vm_hashtable_for_each() -> Value {
    Value::Procedure(Rc::new(VmHashtableForEach) as Rc<dyn Procedure>)
}
pub fn make_vm_hashtable_fold() -> Value {
    Value::Procedure(Rc::new(VmHashtableFold) as Rc<dyn Procedure>)
}
pub fn make_vm_hashtable_update() -> Value {
    Value::Procedure(Rc::new(VmHashtableUpdate) as Rc<dyn Procedure>)
}
pub fn make_vm_unfold() -> Value {
    Value::Procedure(Rc::new(VmUnfold) as Rc<dyn Procedure>)
}
pub fn make_vm_list_sort() -> Value {
    Value::Procedure(Rc::new(VmListSort) as Rc<dyn Procedure>)
}
pub fn make_vm_vector_sort() -> Value {
    Value::Procedure(Rc::new(VmVectorSort) as Rc<dyn Procedure>)
}
pub fn make_vm_vector_sort_bang() -> Value {
    Value::Procedure(Rc::new(VmVectorSortBang) as Rc<dyn Procedure>)
}

/// Exception support markers.
#[derive(Debug)]
pub struct VmRaise;
#[derive(Debug)]
pub struct VmErrorFn;
#[derive(Debug)]
pub struct VmWithExceptionHandler;
#[derive(Debug)]
pub struct VmGuardCallcc;
#[derive(Debug)]
pub struct VmDynamicWind;

impl_proc_named!(VmRaise, "raise");
impl_proc_named!(VmErrorFn, "error");
impl_proc_named!(VmWithExceptionHandler, "with-exception-handler");
impl_proc_named!(VmGuardCallcc, "call/cc");
impl_proc_named!(VmDynamicWind, "dynamic-wind");

pub fn make_vm_raise() -> Value {
    Value::Procedure(Rc::new(VmRaise) as Rc<dyn Procedure>)
}
pub fn make_vm_error_fn() -> Value {
    Value::Procedure(Rc::new(VmErrorFn) as Rc<dyn Procedure>)
}
pub fn make_vm_with_exception_handler() -> Value {
    Value::Procedure(Rc::new(VmWithExceptionHandler) as Rc<dyn Procedure>)
}
pub fn make_vm_dynamic_wind() -> Value {
    Value::Procedure(Rc::new(VmDynamicWind) as Rc<dyn Procedure>)
}

/// Build a "condition" value matching the tree-walker's `make_condition`:
/// a list `(string("error") string(msg) irritants...)`.
fn make_vm_condition(msg: String, irritants: Vec<Value>) -> Value {
    let mut items = vec![Value::string("error"), Value::string(msg)];
    items.extend(irritants);
    Value::list(items)
}

/// Synchronously call a VM procedure and return its result. Used by HO native
/// builtins (map/for-each/filter) to invoke the procedure once per element.
/// For closures, this runs a sub-VM to completion on the closure body.
pub fn vm_call_sync(
    func: &Value,
    args: &[Value],
    syms: &mut SymbolTable,
) -> Result<Value, VmError> {
    match func {
        Value::Procedure(p) => {
            let any = p.as_any();
            if let Some(b) = any.downcast_ref::<VmBuiltin>() {
                return (b.f)(args).map_err(|e| VmError::new(format!("{}: {}", b.name, e)));
            }
            if let Some(b) = any.downcast_ref::<VmBuiltinSyms>() {
                return (b.f)(args, syms).map_err(|e| VmError::new(format!("{}: {}", b.name, e)));
            }
            if let Some(c) = any.downcast_ref::<VmClosure>() {
                let lam = &c.bc.lambdas[c.lambda_idx];
                if !lambda_arity_ok(lam, args.len()) {
                    return Err(VmError::new("arity mismatch"));
                }
                let new_env = Env::child(c.env.clone());
                for (name, v) in lam.params.iter().zip(args.iter()) {
                    new_env.define(*name, v.clone());
                }
                if let Some(rest_name) = lam.rest {
                    let rest_args = &args[lam.params.len()..];
                    new_env.define(rest_name, Value::list(rest_args.iter().cloned()));
                }
                // Wrap lambda body in a fresh Bytecode and recursively run.
                let sub_bc = Bytecode {
                    insts: lam.body.clone(),
                    lambdas: c.bc.lambdas.clone(),
                };
                return run(&sub_bc, new_env, syms);
            }
            if any.downcast_ref::<VmApply>().is_some() {
                if args.is_empty() {
                    return Err(VmError::new("apply: 0 args"));
                }
                let inner = args[0].clone();
                let mut spread: Vec<Value> = args[1..args.len().saturating_sub(1)].to_vec();
                if args.len() >= 2 {
                    let last = args[args.len() - 1].clone();
                    let mut cur = last;
                    loop {
                        match cur {
                            Value::Null => break,
                            Value::Pair(p) => {
                                spread.push(p.car.borrow().clone());
                                cur = p.cdr.borrow().clone();
                            }
                            other => {
                                return Err(VmError::new(format!(
                                    "apply: non-list tail ({})",
                                    other.type_name()
                                )));
                            }
                        }
                    }
                }
                return vm_call_sync(&inner, &spread, syms);
            }
            if any.downcast_ref::<VmValues>().is_some() {
                if args.len() == 1 {
                    return Ok(args[0].clone());
                }
                set_pending_values(args.to_vec());
                return Ok(Value::Unspecified);
            }
            if any.downcast_ref::<VmCallWithValues>().is_some() {
                if args.len() != 2 {
                    return Err(VmError::new("call-with-values: 2 args"));
                }
                let prev = take_pending_values();
                let prod_result = vm_call_sync(&args[0], &[], syms)?;
                let values = if let Some(vs) = take_pending_values() {
                    vs
                } else {
                    vec![prod_result]
                };
                if let Some(prev) = prev {
                    set_pending_values(prev);
                }
                return vm_call_sync(&args[1], &values, syms);
            }
            // Recursively dispatch HO markers when they're called as the
            // procedure target of vm_call_sync (e.g. (apply map proc lst)).
            if any.downcast_ref::<VmMap>().is_some()
                || any.downcast_ref::<VmForEach>().is_some()
                || any.downcast_ref::<VmFilter>().is_some()
                || any.downcast_ref::<VmFind>().is_some()
                || any.downcast_ref::<VmAny>().is_some()
                || any.downcast_ref::<VmEvery>().is_some()
                || any.downcast_ref::<VmFoldLeft>().is_some()
                || any.downcast_ref::<VmFoldRight>().is_some()
                || any.downcast_ref::<VmReduce>().is_some()
                || any.downcast_ref::<VmCount>().is_some()
                || any.downcast_ref::<VmPartition>().is_some()
                || is_pure_ho_marker(p.as_ref())
            {
                return ho_apply(func, args, syms);
            }
            Err(VmError::new("unsupported procedure type in vm_call_sync"))
        }
        _ => Err(VmError::new("not a procedure")),
    }
}

/// Dispatch a HO marker procedure (map/filter/fold/...) when invoked via
/// vm_call_sync (e.g. nested through `apply`). Mirrors the inline arms in
/// `run`'s Call dispatch but without push/pop'ing the VM stack.
fn ho_apply(func: &Value, args: &[Value], syms: &mut SymbolTable) -> Result<Value, VmError> {
    let p = match func {
        Value::Procedure(p) => p.clone(),
        _ => return Err(VmError::new("ho_apply: not a procedure")),
    };
    let any = p.as_any();
    let mut args = args.to_vec();
    if any.downcast_ref::<VmMap>().is_some() {
        if args.len() < 2 {
            return Err(VmError::new("map: needs proc + list"));
        }
        let proc_val = args.remove(0);
        let lists: Vec<Vec<Value>> = args
            .iter()
            .map(collect_proper_list)
            .collect::<Result<_, _>>()?;
        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
            out.push(vm_call_sync(&proc_val, &row, syms)?);
        }
        return Ok(Value::list(out));
    }
    if any.downcast_ref::<VmForEach>().is_some() {
        if args.len() < 2 {
            return Err(VmError::new("for-each: needs proc + list"));
        }
        let proc_val = args.remove(0);
        let lists: Vec<Vec<Value>> = args
            .iter()
            .map(collect_proper_list)
            .collect::<Result<_, _>>()?;
        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
        for i in 0..n {
            let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
            vm_call_sync(&proc_val, &row, syms)?;
        }
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmFilter>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("filter: needs pred + list"));
        }
        let pred = args.remove(0);
        let items = collect_proper_list(&args[0])?;
        let mut kept = Vec::new();
        for item in items {
            let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
            if r.is_truthy() {
                kept.push(item);
            }
        }
        return Ok(Value::list(kept));
    }
    if any.downcast_ref::<VmFind>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("find: needs pred + list"));
        }
        let pred = args.remove(0);
        let items = collect_proper_list(&args[0])?;
        for item in items {
            let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
            if r.is_truthy() {
                return Ok(item);
            }
        }
        return Ok(Value::Boolean(false));
    }
    if any.downcast_ref::<VmAny>().is_some() {
        if args.len() < 2 {
            return Err(VmError::new("any: needs pred + list"));
        }
        let pred = args.remove(0);
        let lists: Vec<Vec<Value>> = args
            .iter()
            .map(collect_proper_list)
            .collect::<Result<_, _>>()?;
        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
        for i in 0..n {
            let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
            let r = vm_call_sync(&pred, &row, syms)?;
            if r.is_truthy() {
                return Ok(r);
            }
        }
        return Ok(Value::Boolean(false));
    }
    if any.downcast_ref::<VmEvery>().is_some() {
        if args.len() < 2 {
            return Err(VmError::new("every: needs pred + list"));
        }
        let pred = args.remove(0);
        let lists: Vec<Vec<Value>> = args
            .iter()
            .map(collect_proper_list)
            .collect::<Result<_, _>>()?;
        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
        let mut last_truthy = Value::Boolean(true);
        for i in 0..n {
            let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
            let r = vm_call_sync(&pred, &row, syms)?;
            if !r.is_truthy() {
                return Ok(Value::Boolean(false));
            }
            last_truthy = r;
        }
        return Ok(last_truthy);
    }
    if any.downcast_ref::<VmFoldLeft>().is_some() {
        if args.len() < 3 {
            return Err(VmError::new("fold-left: needs proc + init + list"));
        }
        let proc_val = args.remove(0);
        let mut acc = args.remove(0);
        let lists: Vec<Vec<Value>> = args
            .iter()
            .map(collect_proper_list)
            .collect::<Result<_, _>>()?;
        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
        for i in 0..n {
            let mut row: Vec<Value> = vec![acc.clone()];
            for l in &lists {
                row.push(l[i].clone());
            }
            acc = vm_call_sync(&proc_val, &row, syms)?;
        }
        return Ok(acc);
    }
    if any.downcast_ref::<VmFoldRight>().is_some() {
        if args.len() < 3 {
            return Err(VmError::new("fold-right: needs proc + init + list"));
        }
        let proc_val = args.remove(0);
        let init = args.remove(0);
        let lists: Vec<Vec<Value>> = args
            .iter()
            .map(collect_proper_list)
            .collect::<Result<_, _>>()?;
        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
        let mut acc = init;
        for i in (0..n).rev() {
            let mut row: Vec<Value> = Vec::with_capacity(lists.len() + 1);
            for l in &lists {
                row.push(l[i].clone());
            }
            row.push(acc);
            acc = vm_call_sync(&proc_val, &row, syms)?;
        }
        return Ok(acc);
    }
    if any.downcast_ref::<VmReduce>().is_some() {
        if args.len() != 3 {
            return Err(VmError::new("reduce: needs proc + default + list"));
        }
        let proc_val = args.remove(0);
        let default = args.remove(0);
        let items = collect_proper_list(&args[0])?;
        if items.is_empty() {
            return Ok(default);
        }
        let mut acc = items[0].clone();
        for item in &items[1..] {
            acc = vm_call_sync(&proc_val, &[acc, item.clone()], syms)?;
        }
        return Ok(acc);
    }
    if any.downcast_ref::<VmCount>().is_some() {
        if args.len() < 2 {
            return Err(VmError::new("count: needs pred + list"));
        }
        let pred = args.remove(0);
        let lists: Vec<Vec<Value>> = args
            .iter()
            .map(collect_proper_list)
            .collect::<Result<_, _>>()?;
        let n = lists.iter().map(|l| l.len()).min().unwrap_or(0);
        let mut total: i64 = 0;
        for i in 0..n {
            let row: Vec<Value> = lists.iter().map(|l| l[i].clone()).collect();
            let r = vm_call_sync(&pred, &row, syms)?;
            if r.is_truthy() {
                total += 1;
            }
        }
        return Ok(Value::fixnum(total));
    }
    if any.downcast_ref::<VmPartition>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("partition: needs pred + list"));
        }
        let pred = args.remove(0);
        let items = collect_proper_list(&args[0])?;
        let mut yes = Vec::new();
        let mut no = Vec::new();
        for item in items {
            let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
            if r.is_truthy() {
                yes.push(item);
            } else {
                no.push(item);
            }
        }
        set_pending_values(vec![Value::list(yes), Value::list(no)]);
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmVectorMap>().is_some() {
        if args.len() < 2 {
            return Err(VmError::new("vector-map: needs proc + vector"));
        }
        let proc_val = args.remove(0);
        let vectors: Vec<Vec<Value>> = args
            .iter()
            .map(|v| match v {
                Value::Vector(vec) => Ok(vec.borrow().clone()),
                other => Err(VmError::new(format!(
                    "vector-map: expected vector, got {}",
                    other.type_name()
                ))),
            })
            .collect::<Result<_, _>>()?;
        let n = vectors.iter().map(|v| v.len()).min().unwrap_or(0);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let row: Vec<Value> = vectors.iter().map(|v| v[i].clone()).collect();
            out.push(vm_call_sync(&proc_val, &row, syms)?);
        }
        return Ok(Value::Vector(Rc::new(RefCell::new(out))));
    }
    if any.downcast_ref::<VmVectorForEach>().is_some() {
        if args.len() < 2 {
            return Err(VmError::new("vector-for-each: needs proc + vector"));
        }
        let proc_val = args.remove(0);
        let vectors: Vec<Vec<Value>> = args
            .iter()
            .map(|v| match v {
                Value::Vector(vec) => Ok(vec.borrow().clone()),
                other => Err(VmError::new(format!(
                    "vector-for-each: expected vector, got {}",
                    other.type_name()
                ))),
            })
            .collect::<Result<_, _>>()?;
        let n = vectors.iter().map(|v| v.len()).min().unwrap_or(0);
        for i in 0..n {
            let row: Vec<Value> = vectors.iter().map(|v| v[i].clone()).collect();
            vm_call_sync(&proc_val, &row, syms)?;
        }
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmVectorFold>().is_some() {
        if args.len() != 3 {
            return Err(VmError::new("vector-fold: needs proc + init + vector"));
        }
        let proc_val = args.remove(0);
        let mut acc = args.remove(0);
        let items = match &args[0] {
            Value::Vector(v) => v.borrow().clone(),
            other => {
                return Err(VmError::new(format!(
                    "vector-fold: expected vector, got {}",
                    other.type_name()
                )));
            }
        };
        for item in items {
            acc = vm_call_sync(&proc_val, &[acc, item], syms)?;
        }
        return Ok(acc);
    }
    if any.downcast_ref::<VmVectorFilter>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("vector-filter: needs pred + vector"));
        }
        let pred = args.remove(0);
        let items = match &args[0] {
            Value::Vector(v) => v.borrow().clone(),
            other => {
                return Err(VmError::new(format!(
                    "vector-filter: expected vector, got {}",
                    other.type_name()
                )));
            }
        };
        let mut out = Vec::new();
        for item in items {
            let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
            if r.is_truthy() {
                out.push(item);
            }
        }
        return Ok(Value::Vector(Rc::new(RefCell::new(out))));
    }
    if any.downcast_ref::<VmStringMap>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("string-map: needs proc + string"));
        }
        let proc_val = args.remove(0);
        let chars: Vec<char> = match &args[0] {
            Value::String(s) => s.borrow().chars().collect(),
            other => {
                return Err(VmError::new(format!(
                    "string-map: expected string, got {}",
                    other.type_name()
                )));
            }
        };
        let mut out = String::with_capacity(chars.len());
        for c in chars {
            let r = vm_call_sync(&proc_val, &[Value::Character(c)], syms)?;
            match r {
                Value::Character(c) => out.push(c),
                other => {
                    return Err(VmError::new(format!(
                        "string-map: proc must return char, got {}",
                        other.type_name()
                    )));
                }
            }
        }
        return Ok(Value::string(out));
    }
    if any.downcast_ref::<VmStringForEach>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("string-for-each: needs proc + string"));
        }
        let proc_val = args.remove(0);
        let chars: Vec<char> = match &args[0] {
            Value::String(s) => s.borrow().chars().collect(),
            other => {
                return Err(VmError::new(format!(
                    "string-for-each: expected string, got {}",
                    other.type_name()
                )));
            }
        };
        for c in chars {
            vm_call_sync(&proc_val, &[Value::Character(c)], syms)?;
        }
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmHashtableWalk>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("hashtable-walk: needs ht + proc"));
        }
        let h = match &args[0] {
            Value::Hashtable(h) => h.clone(),
            other => {
                return Err(VmError::new(format!(
                    "hashtable-walk: expected hashtable, got {}",
                    other.type_name()
                )));
            }
        };
        let proc_val = args.remove(1);
        let entries: Vec<(Value, Value)> = h.items.borrow().clone();
        for (k, v) in entries {
            vm_call_sync(&proc_val, &[k, v], syms)?;
        }
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmHashtableForEach>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("hashtable-for-each: needs proc + ht"));
        }
        let proc_val = args.remove(0);
        let h = match &args[0] {
            Value::Hashtable(h) => h.clone(),
            other => {
                return Err(VmError::new(format!(
                    "hashtable-for-each: expected hashtable, got {}",
                    other.type_name()
                )));
            }
        };
        let entries: Vec<(Value, Value)> = h.items.borrow().clone();
        for (k, v) in entries {
            vm_call_sync(&proc_val, &[k, v], syms)?;
        }
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmHashtableFold>().is_some() {
        if args.len() != 3 {
            return Err(VmError::new("hashtable-fold: needs proc + init + ht"));
        }
        let proc_val = args.remove(0);
        let mut acc = args.remove(0);
        let h = match &args[0] {
            Value::Hashtable(h) => h.clone(),
            other => {
                return Err(VmError::new(format!(
                    "hashtable-fold: expected hashtable, got {}",
                    other.type_name()
                )));
            }
        };
        let entries: Vec<(Value, Value)> = h.items.borrow().clone();
        for (k, v) in entries {
            acc = vm_call_sync(&proc_val, &[k, v, acc], syms)?;
        }
        return Ok(acc);
    }
    if any.downcast_ref::<VmHashtableUpdate>().is_some() {
        if args.len() != 4 {
            return Err(VmError::new(
                "hashtable-update!: needs ht + key + proc + default",
            ));
        }
        let h = match &args[0] {
            Value::Hashtable(h) => h.clone(),
            other => {
                return Err(VmError::new(format!(
                    "hashtable-update!: expected hashtable, got {}",
                    other.type_name()
                )));
            }
        };
        let kind = h.eq_kind;
        let current = {
            let items = h.items.borrow();
            items
                .iter()
                .find(|(k, _)| ht_eq_local(kind, k, &args[1]))
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| args[3].clone())
        };
        let new_val = vm_call_sync(&args[2], &[current], syms)?;
        let mut items = h.items.borrow_mut();
        if let Some(slot) = items
            .iter_mut()
            .find(|(k, _)| ht_eq_local(kind, k, &args[1]))
        {
            slot.1 = new_val;
        } else {
            items.push((args[1].clone(), new_val));
        }
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmUnfold>().is_some() {
        if args.len() != 4 {
            return Err(VmError::new("unfold: needs pred + map + next + seed"));
        }
        let pred = args.remove(0);
        let map_fn = args.remove(0);
        let next_fn = args.remove(0);
        let mut seed = args.remove(0);
        let mut out = Vec::new();
        for _ in 0..1_000_000 {
            let stop = vm_call_sync(&pred, &[seed.clone()], syms)?;
            if stop.is_truthy() {
                return Ok(Value::list(out));
            }
            let mapped = vm_call_sync(&map_fn, &[seed.clone()], syms)?;
            out.push(mapped);
            seed = vm_call_sync(&next_fn, &[seed], syms)?;
        }
        return Err(VmError::new("unfold: exceeded 1,000,000 iterations"));
    }
    if any.downcast_ref::<VmListSort>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("list-sort: needs cmp + list"));
        }
        let cmp = args.remove(0);
        let mut items = collect_proper_list(&args[0])?;
        sort_with_predicate(&mut items, &cmp, syms)?;
        return Ok(Value::list(items));
    }
    if any.downcast_ref::<VmVectorSort>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("vector-sort: needs cmp + vector"));
        }
        let cmp = args.remove(0);
        let mut items = match &args[0] {
            Value::Vector(v) => v.borrow().clone(),
            other => {
                return Err(VmError::new(format!(
                    "vector-sort: expected vector, got {}",
                    other.type_name()
                )));
            }
        };
        sort_with_predicate(&mut items, &cmp, syms)?;
        return Ok(Value::Vector(Rc::new(RefCell::new(items))));
    }
    if any.downcast_ref::<VmVectorSortBang>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("vector-sort!: needs cmp + vector"));
        }
        let cmp = args.remove(0);
        let vec_rc = match &args[0] {
            Value::Vector(v) => v.clone(),
            other => {
                return Err(VmError::new(format!(
                    "vector-sort!: expected vector, got {}",
                    other.type_name()
                )));
            }
        };
        let mut items = vec_rc.borrow().clone();
        sort_with_predicate(&mut items, &cmp, syms)?;
        *vec_rc.borrow_mut() = items;
        return Ok(Value::Unspecified);
    }
    if any.downcast_ref::<VmTabulate>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("tabulate: needs n + proc"));
        }
        let n = match &args[0] {
            Value::Number(cs_core::Number::Fixnum(n)) => *n,
            other => {
                return Err(VmError::new(format!(
                    "tabulate: expected fixnum, got {}",
                    other.type_name()
                )));
            }
        };
        if n < 0 {
            return Err(VmError::new("tabulate: negative count"));
        }
        let proc_val = args.remove(1);
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let r = vm_call_sync(&proc_val, &[Value::fixnum(i)], syms)?;
            out.push(r);
        }
        return Ok(Value::list(out));
    }
    if any.downcast_ref::<VmRemove>().is_some() {
        if args.len() != 2 {
            return Err(VmError::new("remove: needs pred + list"));
        }
        let pred = args.remove(0);
        let items = collect_proper_list(&args[0])?;
        let mut out = Vec::new();
        for item in items {
            let r = vm_call_sync(&pred, std::slice::from_ref(&item), syms)?;
            if !r.is_truthy() {
                out.push(item);
            }
        }
        return Ok(Value::list(out));
    }
    if any.downcast_ref::<VmForce>().is_some() {
        if args.len() != 1 {
            return Err(VmError::new("force: 1 arg"));
        }
        let arg = args.remove(0);
        match arg {
            Value::Promise(p) => {
                {
                    let state = p.state.borrow();
                    if let cs_core::PromiseState::Forced(v) = &*state {
                        return Ok(v.clone());
                    }
                }
                let thunk = match &*p.state.borrow() {
                    cs_core::PromiseState::Pending(t) => t.clone(),
                    cs_core::PromiseState::Forced(_) => unreachable!(),
                };
                let v = vm_call_sync(&thunk, &[], syms)?;
                *p.state.borrow_mut() = cs_core::PromiseState::Forced(v.clone());
                return Ok(v);
            }
            other => return Ok(other),
        }
    }
    Err(VmError::new("ho_apply: unrecognized HO marker"))
}

fn collect_proper_list(v: &Value) -> Result<Vec<Value>, VmError> {
    let mut out = Vec::new();
    let mut cur = v.clone();
    loop {
        match cur {
            Value::Null => return Ok(out),
            Value::Pair(p) => {
                out.push(p.car.borrow().clone());
                cur = p.cdr.borrow().clone();
            }
            other => {
                return Err(VmError::new(format!(
                    "expected proper list, got {}",
                    other.type_name()
                )));
            }
        }
    }
}

/// Return true when `p` is one of the HO markers handled by `ho_apply`
/// (i.e., everything except `values` and `call-with-values`, which have
/// pending-values side-channel logic).
fn is_pure_ho_marker(p: &dyn Procedure) -> bool {
    let any = p.as_any();
    any.downcast_ref::<VmVectorMap>().is_some()
        || any.downcast_ref::<VmVectorForEach>().is_some()
        || any.downcast_ref::<VmVectorFold>().is_some()
        || any.downcast_ref::<VmVectorFilter>().is_some()
        || any.downcast_ref::<VmStringMap>().is_some()
        || any.downcast_ref::<VmStringForEach>().is_some()
        || any.downcast_ref::<VmHashtableWalk>().is_some()
        || any.downcast_ref::<VmHashtableForEach>().is_some()
        || any.downcast_ref::<VmHashtableFold>().is_some()
        || any.downcast_ref::<VmHashtableUpdate>().is_some()
        || any.downcast_ref::<VmUnfold>().is_some()
        || any.downcast_ref::<VmListSort>().is_some()
        || any.downcast_ref::<VmVectorSort>().is_some()
        || any.downcast_ref::<VmVectorSortBang>().is_some()
        || any.downcast_ref::<VmTabulate>().is_some()
        || any.downcast_ref::<VmRemove>().is_some()
        || any.downcast_ref::<VmForce>().is_some()
}

fn ht_eq_local(kind: cs_core::HtEqKind, a: &Value, b: &Value) -> bool {
    match kind {
        cs_core::HtEqKind::Eq => cs_core::eq::eq(a, b),
        cs_core::HtEqKind::Eqv => cs_core::eq::eqv(a, b),
        cs_core::HtEqKind::Equal => cs_core::eq::equal(a, b),
    }
}

/// Sort `items` in place using `cmp` (a 2-arg procedure returning truthy when
/// the first arg should sort before the second). Stable mergesort.
fn sort_with_predicate(
    items: &mut Vec<Value>,
    cmp: &Value,
    syms: &mut SymbolTable,
) -> Result<(), VmError> {
    let n = items.len();
    if n <= 1 {
        return Ok(());
    }
    let mut buf: Vec<Value> = items.clone();
    let mut size: usize = 1;
    while size < n {
        let mut left = 0;
        while left < n {
            let mid = (left + size).min(n);
            let right = (left + 2 * size).min(n);
            let mut i = left;
            let mut j = mid;
            let mut k = left;
            while i < mid && j < right {
                // Stable merge: take items[i] when items[i] <= items[j], i.e.
                // !(cmp(items[j], items[i])). Using strict-less-than `cmp`,
                // equal elements have cmp false in both directions; this rule
                // takes the left-hand item first, preserving original order.
                let b_lt_a = vm_call_sync(cmp, &[items[j].clone(), items[i].clone()], syms)?;
                if !b_lt_a.is_truthy() {
                    buf[k] = items[i].clone();
                    i += 1;
                } else {
                    buf[k] = items[j].clone();
                    j += 1;
                }
                k += 1;
            }
            while i < mid {
                buf[k] = items[i].clone();
                i += 1;
                k += 1;
            }
            while j < right {
                buf[k] = items[j].clone();
                j += 1;
                k += 1;
            }
            left += 2 * size;
        }
        std::mem::swap(items, &mut buf);
        size *= 2;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile;
    use cs_core::{Number, SymbolTable, Value};
    use cs_diag::Span;
    use cs_ir::{CoreExpr, Params};

    fn b_add(args: &[Value]) -> Result<Value, String> {
        let mut acc: i64 = 0;
        for a in args {
            match a {
                Value::Number(Number::Fixnum(n)) => acc += n,
                _ => return Err("expected fixnum".into()),
            }
        }
        Ok(Value::fixnum(acc))
    }

    fn b_sub(args: &[Value]) -> Result<Value, String> {
        if args.is_empty() {
            return Err("sub: 0 args".into());
        }
        let mut iter = args.iter();
        let first = match iter.next().unwrap() {
            Value::Number(Number::Fixnum(n)) => *n,
            _ => return Err("expected fixnum".into()),
        };
        let mut acc = first;
        let mut consumed_more = false;
        for a in iter {
            consumed_more = true;
            match a {
                Value::Number(Number::Fixnum(n)) => acc -= n,
                _ => return Err("expected fixnum".into()),
            }
        }
        if !consumed_more {
            acc = -acc;
        }
        Ok(Value::fixnum(acc))
    }

    fn b_mul(args: &[Value]) -> Result<Value, String> {
        let mut acc: i64 = 1;
        for a in args {
            match a {
                Value::Number(Number::Fixnum(n)) => acc *= n,
                _ => return Err("expected fixnum".into()),
            }
        }
        Ok(Value::fixnum(acc))
    }

    fn b_eq(args: &[Value]) -> Result<Value, String> {
        if args.len() != 2 {
            return Err("=: 2 args".into());
        }
        match (&args[0], &args[1]) {
            (Value::Number(Number::Fixnum(a)), Value::Number(Number::Fixnum(b))) => {
                Ok(Value::Boolean(a == b))
            }
            _ => Err("expected fixnums".into()),
        }
    }

    fn make_test_env(syms: &mut SymbolTable) -> Rc<Env> {
        let env = Env::root();
        env.define(syms.intern("+"), make_vm_builtin("+", b_add));
        env.define(syms.intern("-"), make_vm_builtin("-", b_sub));
        env.define(syms.intern("*"), make_vm_builtin("*", b_mul));
        env.define(syms.intern("="), make_vm_builtin("=", b_eq));
        env
    }

    #[test]
    fn vm_const() {
        let mut syms = SymbolTable::new();
        let env = make_test_env(&mut syms);
        let expr = CoreExpr::Const {
            value: Value::fixnum(42),
            span: Span::DUMMY,
        };
        let bc = compile(&expr).unwrap();
        let r = run(&bc, env, &mut syms).unwrap();
        match r {
            Value::Number(Number::Fixnum(42)) => {}
            other => panic!("expected 42, got {:?}", other),
        }
    }

    #[test]
    fn vm_add() {
        let mut syms = SymbolTable::new();
        let env = make_test_env(&mut syms);
        let plus = syms.intern("+");
        let expr = CoreExpr::App {
            func: Rc::new(CoreExpr::Ref {
                name: plus,
                span: Span::DUMMY,
            }),
            args: vec![
                CoreExpr::Const {
                    value: Value::fixnum(1),
                    span: Span::DUMMY,
                },
                CoreExpr::Const {
                    value: Value::fixnum(2),
                    span: Span::DUMMY,
                },
                CoreExpr::Const {
                    value: Value::fixnum(3),
                    span: Span::DUMMY,
                },
            ],
            span: Span::DUMMY,
        };
        let bc = compile(&expr).unwrap();
        let r = run(&bc, env, &mut syms).unwrap();
        match r {
            Value::Number(Number::Fixnum(6)) => {}
            other => panic!("expected 6, got {:?}", other),
        }
    }

    #[test]
    fn vm_if_then_branch() {
        let mut syms = SymbolTable::new();
        let env = make_test_env(&mut syms);
        let expr = CoreExpr::If {
            cond: Rc::new(CoreExpr::Const {
                value: Value::Boolean(true),
                span: Span::DUMMY,
            }),
            then: Rc::new(CoreExpr::Const {
                value: Value::fixnum(1),
                span: Span::DUMMY,
            }),
            alt: Rc::new(CoreExpr::Const {
                value: Value::fixnum(2),
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        let bc = compile(&expr).unwrap();
        let r = run(&bc, env, &mut syms).unwrap();
        match r {
            Value::Number(Number::Fixnum(1)) => {}
            other => panic!("expected 1, got {:?}", other),
        }
    }

    #[test]
    fn vm_lambda_call() {
        let mut syms = SymbolTable::new();
        let env = make_test_env(&mut syms);
        let x = syms.intern("x");
        let plus = syms.intern("+");
        // ((lambda (x) (+ x 1)) 41)
        let lam = CoreExpr::Lambda {
            params: Params::fixed(vec![x]),
            body: Rc::new(CoreExpr::App {
                func: Rc::new(CoreExpr::Ref {
                    name: plus,
                    span: Span::DUMMY,
                }),
                args: vec![
                    CoreExpr::Ref {
                        name: x,
                        span: Span::DUMMY,
                    },
                    CoreExpr::Const {
                        value: Value::fixnum(1),
                        span: Span::DUMMY,
                    },
                ],
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        let app = CoreExpr::App {
            func: Rc::new(lam),
            args: vec![CoreExpr::Const {
                value: Value::fixnum(41),
                span: Span::DUMMY,
            }],
            span: Span::DUMMY,
        };
        let bc = compile(&app).unwrap();
        let r = run(&bc, env, &mut syms).unwrap();
        match r {
            Value::Number(Number::Fixnum(42)) => {}
            other => panic!("expected 42, got {:?}", other),
        }
    }

    #[test]
    fn vm_letrec_recursive() {
        // (letrec ((fact (lambda (n) (if (= n 0) 1 (* n (fact (- n 1))))))) (fact 5))
        let mut syms = SymbolTable::new();
        let env = make_test_env(&mut syms);
        let fact = syms.intern("fact");
        let n = syms.intern("n");
        let plus = syms.intern("+");
        let _ = plus;
        let mul = syms.intern("*");
        let sub = syms.intern("-");
        let eq = syms.intern("=");
        let body = CoreExpr::Lambda {
            params: Params::fixed(vec![n]),
            body: Rc::new(CoreExpr::If {
                cond: Rc::new(CoreExpr::App {
                    func: Rc::new(CoreExpr::Ref {
                        name: eq,
                        span: Span::DUMMY,
                    }),
                    args: vec![
                        CoreExpr::Ref {
                            name: n,
                            span: Span::DUMMY,
                        },
                        CoreExpr::Const {
                            value: Value::fixnum(0),
                            span: Span::DUMMY,
                        },
                    ],
                    span: Span::DUMMY,
                }),
                then: Rc::new(CoreExpr::Const {
                    value: Value::fixnum(1),
                    span: Span::DUMMY,
                }),
                alt: Rc::new(CoreExpr::App {
                    func: Rc::new(CoreExpr::Ref {
                        name: mul,
                        span: Span::DUMMY,
                    }),
                    args: vec![
                        CoreExpr::Ref {
                            name: n,
                            span: Span::DUMMY,
                        },
                        CoreExpr::App {
                            func: Rc::new(CoreExpr::Ref {
                                name: fact,
                                span: Span::DUMMY,
                            }),
                            args: vec![CoreExpr::App {
                                func: Rc::new(CoreExpr::Ref {
                                    name: sub,
                                    span: Span::DUMMY,
                                }),
                                args: vec![
                                    CoreExpr::Ref {
                                        name: n,
                                        span: Span::DUMMY,
                                    },
                                    CoreExpr::Const {
                                        value: Value::fixnum(1),
                                        span: Span::DUMMY,
                                    },
                                ],
                                span: Span::DUMMY,
                            }],
                            span: Span::DUMMY,
                        },
                    ],
                    span: Span::DUMMY,
                }),
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        let letrec = CoreExpr::Letrec {
            bindings: vec![(fact, body)],
            body: Rc::new(CoreExpr::App {
                func: Rc::new(CoreExpr::Ref {
                    name: fact,
                    span: Span::DUMMY,
                }),
                args: vec![CoreExpr::Const {
                    value: Value::fixnum(5),
                    span: Span::DUMMY,
                }],
                span: Span::DUMMY,
            }),
            span: Span::DUMMY,
        };
        let bc = compile(&letrec).unwrap();
        let r = run(&bc, env, &mut syms).unwrap();
        match r {
            Value::Number(Number::Fixnum(120)) => {}
            other => panic!("expected 120, got {:?}", other),
        }
    }
}
