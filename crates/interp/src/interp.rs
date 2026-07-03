//! The tree-walking evaluator: registers a program's declarations, then executes `main`.

use std::collections::{BTreeMap, HashMap};

use frontend::ast::{
    Arg, AssignOp, BinOp, Block, Expr, ExprKind, FnDecl, Item, MatchArm, Program, Stmt, StmtKind,
    StructLit, Type, TypeKind, UnOp, VarDecl,
};

use crate::error::RunError;
use crate::value::{Value, display};

/// Non-local control flow produced by executing a statement or block.
enum Flow {
    /// Fell off the end normally.
    Normal,
    /// `dip` — break out of the enclosing loop.
    Break,
    /// `skip` — continue the enclosing loop.
    Continue,
    /// `bet [e, ...]` — return 0..n values from the enclosing function.
    Return(Vec<Value>),
}

/// A lexically-scoped variable environment: a stack of scopes, innermost last.
#[derive(Default)]
struct Env {
    scopes: Vec<HashMap<String, Value>>,
}

impl Env {
    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    /// Declare a fresh binding in the innermost scope (shadowing any outer one).
    fn declare(&mut self, name: &str, val: Value) {
        self.scopes
            .last_mut()
            .expect("at least one scope is always live")
            .insert(name.to_string(), val);
    }

    fn get(&self, name: &str) -> Option<&Value> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    fn get_mut(&mut self, name: &str) -> Option<&mut Value> {
        self.scopes.iter_mut().rev().find_map(|s| s.get_mut(name))
    }
}

/// What a bare `moods` variant name resolves to at a use site.
struct VariantInfo {
    moods: String,
    arity: usize,
}

/// The interpreter: declaration tables plus the captured output buffer.
pub struct Interp<'p> {
    funcs: HashMap<String, &'p FnDecl>,
    methods: HashMap<(String, String), &'p FnDecl>,
    variants: HashMap<String, VariantInfo>,
    globals: HashMap<String, Value>,
    out: Vec<u8>,
}

impl<'p> Interp<'p> {
    /// Register every top-level declaration and evaluate module-level constants/variables.
    pub fn new(program: &'p Program) -> Result<Self, RunError> {
        let mut me = Interp {
            funcs: HashMap::new(),
            methods: HashMap::new(),
            variants: HashMap::new(),
            globals: HashMap::new(),
            out: Vec::new(),
        };
        // First pass: functions, methods, and moods variants (order-independent).
        for item in &program.items {
            match item {
                Item::Func(f) => me.register_fn(f),
                Item::Moods(m) => {
                    for v in &m.variants {
                        me.variants.insert(
                            v.name.clone(),
                            VariantInfo {
                                moods: m.name.clone(),
                                arity: v.payload.len(),
                            },
                        );
                    }
                }
                _ => {}
            }
        }
        // Second pass: module-level constants/variables, which may reference the above.
        for item in &program.items {
            match item {
                Item::Const(c) => {
                    let mut env = Env::default();
                    env.push();
                    let val = me.eval_expr(&mut env, &c.value)?;
                    let val = me.coerce(val, c.ty.as_ref());
                    me.globals.insert(c.name.clone(), val);
                }
                Item::Var(v) => {
                    let mut env = Env::default();
                    env.push();
                    let vals = me.bind_values(&mut env, v)?;
                    for (name, val) in v.targets.iter().zip(vals) {
                        me.globals.insert(name.clone(), val);
                    }
                }
                _ => {}
            }
        }
        Ok(me)
    }

    fn register_fn(&mut self, f: &'p FnDecl) {
        match &f.receiver {
            Some(recv) => {
                let ty = type_head(&recv.ty).to_string();
                self.methods.insert((ty, f.name.clone()), f);
            }
            None => {
                self.funcs.insert(f.name.clone(), f);
            }
        }
    }

    /// Run `finna main()`. Errors if there is no such function.
    pub fn exec_main(&mut self) -> Result<(), RunError> {
        let main = *self.funcs.get("main").ok_or(RunError::NoMain)?;
        self.call_fn(main, Vec::new(), None)?;
        Ok(())
    }

    /// Consume the captured output as a UTF-8 string.
    pub fn into_output_string(self) -> Result<String, RunError> {
        String::from_utf8(self.out).map_err(|e| RunError::Io(e.to_string()))
    }

    /// The captured output bytes.
    pub fn output(&self) -> &[u8] {
        &self.out
    }

    // ---- calls ----------------------------------------------------------------

    fn call_fn(
        &mut self,
        f: &'p FnDecl,
        args: Vec<Value>,
        receiver: Option<Value>,
    ) -> Result<Vec<Value>, RunError> {
        if args.len() != f.params.len() {
            return Err(RunError::Arity {
                what: f.name.clone(),
                expected: f.params.len(),
                got: args.len(),
            });
        }
        let mut env = Env::default();
        env.push();
        if let (Some(recv_param), Some(recv_val)) = (&f.receiver, receiver) {
            env.declare(&recv_param.name, recv_val);
        }
        for (param, val) in f.params.iter().zip(args) {
            let val = self.coerce(val, Some(&param.ty));
            env.declare(&param.name, val);
        }
        match self.exec_block(&mut env, &f.body)? {
            Flow::Return(vals) => Ok(vals),
            Flow::Normal => Ok(Vec::new()),
            Flow::Break | Flow::Continue => Err(RunError::Type(format!(
                "`dip`/`skip` outside a loop in `{}`",
                f.name
            ))),
        }
    }

    // ---- statements -----------------------------------------------------------

    fn exec_block(&mut self, env: &mut Env, block: &Block) -> Result<Flow, RunError> {
        env.push();
        let mut flow = Flow::Normal;
        for stmt in &block.stmts {
            flow = self.exec_stmt(env, stmt)?;
            if !matches!(flow, Flow::Normal) {
                break;
            }
        }
        env.pop();
        Ok(flow)
    }

    fn exec_stmt(&mut self, env: &mut Env, stmt: &Stmt) -> Result<Flow, RunError> {
        match &stmt.kind {
            StmtKind::Var(v) => {
                let vals = self.bind_values(env, v)?;
                for (name, val) in v.targets.iter().zip(vals) {
                    let val = self.coerce(val, v.ty.as_ref());
                    env.declare(name, val);
                }
                Ok(Flow::Normal)
            }
            StmtKind::Const(c) => {
                let val = self.eval_expr(env, &c.value)?;
                let val = self.coerce(val, c.ty.as_ref());
                env.declare(&c.name, val);
                Ok(Flow::Normal)
            }
            StmtKind::Fr(fr) => {
                if self.eval_bool(env, &fr.cond)? {
                    return self.exec_block(env, &fr.then);
                }
                for elif in &fr.elifs {
                    if self.eval_bool(env, &elif.0)? {
                        return self.exec_block(env, &elif.1);
                    }
                }
                match &fr.els {
                    Some(body) => self.exec_block(env, body),
                    None => Ok(Flow::Normal),
                }
            }
            StmtKind::Vibin { cond, body } => {
                while self.eval_bool(env, cond)? {
                    match self.exec_block(env, body)? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Normal | Flow::Continue => {}
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::Squad { var, iter, body } => {
                let iter_val = self.eval_expr(env, iter)?;
                let items = match iter_val {
                    Value::Array(xs) => xs,
                    other => {
                        return Err(RunError::Type(format!(
                            "`squad` needs an array to iterate, got {}",
                            other.type_name()
                        )));
                    }
                };
                for item in items {
                    env.push();
                    env.declare(var, item);
                    let flow = self.exec_block(env, body);
                    env.pop();
                    match flow? {
                        Flow::Break => break,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Normal | Flow::Continue => {}
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::Vibe {
                scrutinee,
                arms,
                default,
            } => {
                let scrutinee = self.eval_expr(env, scrutinee)?;
                self.exec_vibe(env, &scrutinee, arms, default)
            }
            StmtKind::Bet(exprs) => {
                let mut vals = Vec::with_capacity(exprs.len());
                for e in exprs {
                    vals.push(self.eval_expr(env, e)?);
                }
                Ok(Flow::Return(vals))
            }
            StmtKind::Dip => Ok(Flow::Break),
            StmtKind::Skip => Ok(Flow::Continue),
            StmtKind::Assign {
                targets,
                op,
                values,
            } => {
                self.exec_assign(env, targets, *op, values)?;
                Ok(Flow::Normal)
            }
            StmtKind::Expr(e) => {
                self.eval_call_or_expr(env, e)?;
                Ok(Flow::Normal)
            }
            StmtKind::Yeet(e) => {
                let v = self.eval_expr(env, e)?;
                Err(RunError::Yeet(v))
            }
            // Memory-model, error-handling, and concurrency statements are out of this slice.
            StmtKind::Crib(_) => Err(RunError::Unsupported("crib arena declaration".into())),
            StmtKind::Holla { .. } => Err(RunError::Unsupported("holla tag deref".into())),
            StmtKind::Sheesh { .. } => Err(RunError::Unsupported("sheesh recover".into())),
            StmtKind::Evict(_) => Err(RunError::Unsupported("evict".into())),
            StmtKind::Slide(_) => Err(RunError::Unsupported("slide task spawn".into())),
            StmtKind::Bounce(_) => Err(RunError::Unsupported("bounce error return".into())),
        }
    }

    fn exec_vibe(
        &mut self,
        env: &mut Env,
        scrutinee: &Value,
        arms: &[MatchArm],
        default: &Option<Block>,
    ) -> Result<Flow, RunError> {
        let (var_name, payload) = match scrutinee {
            Value::Variant { name, payload, .. } => (name.as_str(), payload.as_slice()),
            other => {
                return Err(RunError::Type(format!(
                    "`vibe` needs a moods value, got {}",
                    other.type_name()
                )));
            }
        };
        for arm in arms {
            if arm.variant == var_name {
                if arm.bindings.len() != payload.len() {
                    return Err(RunError::Arity {
                        what: format!("pattern `{}`", arm.variant),
                        expected: payload.len(),
                        got: arm.bindings.len(),
                    });
                }
                env.push();
                for (bind, val) in arm.bindings.iter().zip(payload) {
                    env.declare(bind, val.clone());
                }
                let flow = self.exec_block(env, &arm.body);
                env.pop();
                return flow;
            }
        }
        // No arm's variant matched: fall through to the `naw` default block, or error.
        match default {
            Some(body) => self.exec_block(env, body),
            None => Err(RunError::NonExhaustive(format!("variant `{var_name}`"))),
        }
    }

    fn exec_assign(
        &mut self,
        env: &mut Env,
        targets: &[Expr],
        op: AssignOp,
        values: &[Expr],
    ) -> Result<(), RunError> {
        if op != AssignOp::Eq {
            // Compound assignment is always one target and one value (grammar §S4).
            let (target, value) = match (targets.first(), values.first()) {
                (Some(t), Some(v)) if targets.len() == 1 && values.len() == 1 => (t, v),
                _ => {
                    return Err(RunError::Type(
                        "compound assignment takes exactly one target and value".into(),
                    ));
                }
            };
            let old = self.eval_expr(env, target)?;
            let rhs = self.eval_expr(env, value)?;
            let new = binary_op(compound_binop(op), &old, &rhs)?;
            return self.assign_place(env, target, new);
        }

        // Plain `=`: either pairwise, or a single multi-value call spread across targets.
        if targets.len() == values.len() {
            let mut computed = Vec::with_capacity(values.len());
            for v in values {
                computed.push(self.eval_expr(env, v)?);
            }
            for (target, val) in targets.iter().zip(computed) {
                self.assign_place(env, target, val)?;
            }
            Ok(())
        } else if values.len() == 1 {
            let vals = self.eval_call_expr(env, &values[0])?;
            if vals.len() != targets.len() {
                return Err(RunError::Destructure {
                    expected: targets.len(),
                    got: vals.len(),
                });
            }
            for (target, val) in targets.iter().zip(vals) {
                self.assign_place(env, target, val)?;
            }
            Ok(())
        } else {
            Err(RunError::Destructure {
                expected: targets.len(),
                got: values.len(),
            })
        }
    }

    /// Write `val` into the place denoted by `target` (a name or a `.field` path).
    fn assign_place(&mut self, env: &mut Env, target: &Expr, val: Value) -> Result<(), RunError> {
        match &target.kind {
            ExprKind::Name { name, .. } => {
                if let Some(slot) = env.get_mut(name) {
                    *slot = val;
                    Ok(())
                } else if let Some(slot) = self.globals.get_mut(name) {
                    *slot = val;
                    Ok(())
                } else {
                    Err(RunError::Undefined(name.clone()))
                }
            }
            ExprKind::Field { base, name, .. } => {
                let place = self.place_mut(env, base)?;
                match place {
                    Value::Struct { ty, fields } => {
                        if let Some(slot) = fields.get_mut(name) {
                            *slot = val;
                            Ok(())
                        } else {
                            Err(RunError::UnknownField {
                                ty: ty.clone(),
                                field: name.clone(),
                            })
                        }
                    }
                    other => Err(RunError::Type(format!(
                        "cannot assign field `{name}` of {}",
                        other.type_name()
                    ))),
                }
            }
            ExprKind::Index { .. } => Err(RunError::Unsupported(
                "assignment to an indexed element".into(),
            )),
            _ => Err(RunError::Type("invalid assignment target".into())),
        }
    }

    /// A mutable borrow of the place denoted by `expr` (names and `.field` chains only).
    fn place_mut<'e>(&self, env: &'e mut Env, expr: &Expr) -> Result<&'e mut Value, RunError> {
        match &expr.kind {
            ExprKind::Name { name, .. } => env
                .get_mut(name)
                .ok_or_else(|| RunError::Undefined(name.clone())),
            ExprKind::Field { base, name, .. } => {
                let base = self.place_mut(env, base)?;
                match base {
                    Value::Struct { ty, fields } => {
                        let ty = ty.clone();
                        fields.get_mut(name).ok_or(RunError::UnknownField {
                            ty,
                            field: name.clone(),
                        })
                    }
                    other => Err(RunError::Type(format!(
                        "cannot reach field `{name}` through {}",
                        other.type_name()
                    ))),
                }
            }
            _ => Err(RunError::Type("invalid place expression".into())),
        }
    }

    /// Evaluate the right-hand side of a binding: pairwise, or a spread multi-value call.
    fn bind_values(&mut self, env: &mut Env, v: &VarDecl) -> Result<Vec<Value>, RunError> {
        if v.targets.len() == v.values.len() {
            let mut out = Vec::with_capacity(v.values.len());
            for e in &v.values {
                out.push(self.eval_expr(env, e)?);
            }
            Ok(out)
        } else if v.values.len() == 1 {
            let vals = self.eval_call_expr(env, &v.values[0])?;
            if vals.len() != v.targets.len() {
                return Err(RunError::Destructure {
                    expected: v.targets.len(),
                    got: vals.len(),
                });
            }
            Ok(vals)
        } else {
            Err(RunError::Destructure {
                expected: v.targets.len(),
                got: v.values.len(),
            })
        }
    }

    // ---- expressions ----------------------------------------------------------

    /// Evaluate an expression that must yield exactly one value.
    fn eval_expr(&mut self, env: &mut Env, e: &Expr) -> Result<Value, RunError> {
        match &e.kind {
            // Literals arrive normalized to `i128`; the value model stores an `i64`, so a
            // literal above `i64::MAX` truncates two's-complement (as the old `u64` path did).
            ExprKind::Int(i) => Ok(Value::Int(*i as i64)),
            ExprKind::Float(f) => Ok(Value::Float(*f)),
            ExprKind::Str(s) => Ok(Value::Str(s.clone())),
            ExprKind::Byte(b) => Ok(Value::Byte(*b)),
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Ghosted => Ok(Value::Ghosted),
            ExprKind::Name { name, .. } => self.eval_name(env, name),
            ExprKind::Field { base, name, .. } => {
                let base = self.eval_expr(env, base)?;
                match base {
                    Value::Struct { ty, fields } => {
                        fields.get(name).cloned().ok_or(RunError::UnknownField {
                            ty,
                            field: name.clone(),
                        })
                    }
                    other => Err(RunError::Type(format!(
                        "cannot read field `{name}` of {}",
                        other.type_name()
                    ))),
                }
            }
            ExprKind::Index { base, index } => {
                let base = self.eval_expr(env, base)?;
                let idx = self.eval_expr(env, index)?;
                let i = match idx {
                    Value::Int(i) if i >= 0 => i as usize,
                    other => {
                        return Err(RunError::Type(format!(
                            "index must be a non-negative int, got {}",
                            other.type_name()
                        )));
                    }
                };
                match base {
                    Value::Array(xs) => xs.get(i).cloned().ok_or_else(|| {
                        RunError::Type(format!("index {i} out of bounds (len {})", xs.len()))
                    }),
                    other => Err(RunError::Type(format!(
                        "cannot index {}",
                        other.type_name()
                    ))),
                }
            }
            ExprKind::Call { .. } | ExprKind::Method { .. } => {
                let vals = self.eval_call_expr(env, e)?;
                exactly_one(vals)
            }
            ExprKind::Unary(op, operand) => {
                let v = self.eval_expr(env, operand)?;
                unary_op(*op, &v)
            }
            ExprKind::Binary(op, lhs, rhs) => self.eval_binary(env, *op, lhs, rhs),
            ExprKind::Cast(expr, ty) => {
                let v = self.eval_expr(env, expr)?;
                cast(v, ty)
            }
            ExprKind::Struct(lit) => self.eval_struct_lit(env, lit),
            ExprKind::Array(elems) => {
                let mut xs = Vec::with_capacity(elems.len());
                for e in elems {
                    xs.push(self.eval_expr(env, e)?);
                }
                Ok(Value::Array(xs))
            }
            ExprKind::Cop { .. } => Err(RunError::Unsupported("cop arena allocation".into())),
            ExprKind::Trust { .. } => Err(RunError::Unsupported("trust tag deref".into())),
        }
    }

    fn eval_name(&mut self, env: &Env, name: &str) -> Result<Value, RunError> {
        if let Some(v) = env.get(name) {
            return Ok(v.clone());
        }
        if let Some(v) = self.globals.get(name) {
            return Ok(v.clone());
        }
        if let Some(info) = self.variants.get(name) {
            if info.arity == 0 {
                return Ok(Value::Variant {
                    moods: info.moods.clone(),
                    name: name.to_string(),
                    payload: Vec::new(),
                });
            }
            return Err(RunError::Arity {
                what: format!("variant `{name}`"),
                expected: info.arity,
                got: 0,
            });
        }
        if self.funcs.contains_key(name) {
            return Ok(Value::Fn(name.to_string()));
        }
        Err(RunError::Undefined(name.to_string()))
    }

    fn eval_bool(&mut self, env: &mut Env, e: &Expr) -> Result<bool, RunError> {
        match self.eval_expr(env, e)? {
            Value::Bool(b) => Ok(b),
            other => Err(RunError::Type(format!(
                "condition must be a bool, got {}",
                other.type_name()
            ))),
        }
    }

    fn eval_binary(
        &mut self,
        env: &mut Env,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
    ) -> Result<Value, RunError> {
        // Short-circuit the logical connectives before touching the right operand.
        match op {
            BinOp::And => {
                if !self.eval_bool(env, lhs)? {
                    return Ok(Value::Bool(false));
                }
                return Ok(Value::Bool(self.eval_bool(env, rhs)?));
            }
            BinOp::Or => {
                if self.eval_bool(env, lhs)? {
                    return Ok(Value::Bool(true));
                }
                return Ok(Value::Bool(self.eval_bool(env, rhs)?));
            }
            _ => {}
        }
        let l = self.eval_expr(env, lhs)?;
        let r = self.eval_expr(env, rhs)?;
        binary_op(op, &l, &r)
    }

    fn eval_struct_lit(&mut self, env: &mut Env, lit: &StructLit) -> Result<Value, RunError> {
        // The struct's runtime type is its base name; generic args are erased at runtime.
        let ty = lit.name.clone();
        let mut fields = BTreeMap::new();
        for f in &lit.fields {
            let val = self.eval_expr(env, &f.value)?;
            fields.insert(f.name.clone(), val);
        }
        Ok(Value::Struct { ty, fields })
    }

    // ---- calls (may yield 0..n values) ---------------------------------------

    /// Evaluate a bare-expression statement, tolerating a call that returns 0 or many values.
    fn eval_call_or_expr(&mut self, env: &mut Env, e: &Expr) -> Result<(), RunError> {
        if matches!(e.kind, ExprKind::Call { .. } | ExprKind::Method { .. }) {
            self.eval_call_expr(env, e)?;
        } else {
            self.eval_expr(env, e)?;
        }
        Ok(())
    }

    /// Evaluate a call (or method call) expression, returning all of its result values.
    fn eval_call_expr(&mut self, env: &mut Env, e: &Expr) -> Result<Vec<Value>, RunError> {
        match &e.kind {
            ExprKind::Method {
                receiver,
                method,
                args,
                ..
            } => self.eval_method_call(env, receiver, method, args),
            ExprKind::Call { callee, args } => self.eval_plain_call(env, callee, args),
            // Any other expression yields exactly one value.
            _ => Ok(vec![self.eval_expr(env, e)?]),
        }
    }

    /// Dispatch `receiver.method(args)`: first the `spill`/`str` module builtins (unless the
    /// module name is shadowed by a binding), otherwise a user method keyed by receiver type.
    fn eval_method_call(
        &mut self,
        env: &mut Env,
        receiver: &Expr,
        method: &str,
        args: &[Arg],
    ) -> Result<Vec<Value>, RunError> {
        if let ExprKind::Name { name: modname, .. } = &receiver.kind {
            let shadowed = env.get(modname).is_some() || self.globals.contains_key(modname);
            if !shadowed && modname == "spill" {
                return self.call_spill(env, method, args).map(|()| Vec::new());
            }
            if !shadowed && modname == "str" {
                return self.call_str(env, method, args).map(|v| vec![v]);
            }
        }
        // Otherwise it's a user method call: evaluate the receiver, then dispatch.
        let recv_val = self.eval_expr(env, receiver)?;
        self.call_method(env, recv_val, method, args)
    }

    /// Dispatch a `callee(args)` call: a local fn value, variant constructor, or free function.
    fn eval_plain_call(
        &mut self,
        env: &mut Env,
        callee: &Expr,
        args: &[Arg],
    ) -> Result<Vec<Value>, RunError> {
        if let ExprKind::Name { name, .. } = &callee.kind {
            if let Some(v) = env.get(name).cloned() {
                return match v {
                    Value::Fn(fname) => self.call_named_fn(env, &fname, args),
                    other => Err(RunError::NotCallable(other.type_name().to_string())),
                };
            }
            if let Some(info) = self.variants.get(name) {
                let (moods, arity) = (info.moods.clone(), info.arity);
                return self
                    .construct_variant(env, name, &moods, arity, args)
                    .map(|v| vec![v]);
            }
            if self.funcs.contains_key(name) {
                return self.call_named_fn(env, name, args);
            }
            return Err(RunError::Undefined(name.clone()));
        }

        // An arbitrary callee expression that had better evaluate to a function value.
        match self.eval_expr(env, callee)? {
            Value::Fn(fname) => self.call_named_fn(env, &fname, args),
            other => Err(RunError::NotCallable(other.type_name().to_string())),
        }
    }

    fn call_named_fn(
        &mut self,
        env: &mut Env,
        fname: &str,
        args: &[Arg],
    ) -> Result<Vec<Value>, RunError> {
        let f = *self
            .funcs
            .get(fname)
            .ok_or_else(|| RunError::Undefined(fname.to_string()))?;
        let arg_vals = self.eval_args(env, args)?;
        self.call_fn(f, arg_vals, None)
    }

    fn call_method(
        &mut self,
        env: &mut Env,
        recv: Value,
        method: &str,
        args: &[Arg],
    ) -> Result<Vec<Value>, RunError> {
        let ty = match &recv {
            Value::Struct { ty, .. } => ty.clone(),
            other => {
                return Err(RunError::Type(format!(
                    "no method `{method}` on {}",
                    other.type_name()
                )));
            }
        };
        let f = *self
            .methods
            .get(&(ty.clone(), method.to_string()))
            .ok_or_else(|| RunError::Undefined(format!("{ty}.{method}")))?;
        let arg_vals = self.eval_args(env, args)?;
        self.call_fn(f, arg_vals, Some(recv))
    }

    fn construct_variant(
        &mut self,
        env: &mut Env,
        name: &str,
        moods: &str,
        arity: usize,
        args: &[Arg],
    ) -> Result<Value, RunError> {
        if args.len() != arity {
            return Err(RunError::Arity {
                what: format!("variant `{name}`"),
                expected: arity,
                got: args.len(),
            });
        }
        let payload = self.eval_args(env, args)?;
        Ok(Value::Variant {
            moods: moods.to_string(),
            name: name.to_string(),
            payload,
        })
    }

    fn eval_args(&mut self, env: &mut Env, args: &[Arg]) -> Result<Vec<Value>, RunError> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            out.push(self.eval_expr(env, &a.value)?);
        }
        Ok(out)
    }

    // ---- builtins -------------------------------------------------------------

    fn call_spill(&mut self, env: &mut Env, method: &str, args: &[Arg]) -> Result<(), RunError> {
        match method {
            "it" => {
                if args.len() != 1 {
                    return Err(RunError::Arity {
                        what: "spill.it".into(),
                        expected: 1,
                        got: args.len(),
                    });
                }
                let v = self.eval_expr(env, &args[0].value)?;
                let mut s = display(&v);
                s.push('\n');
                self.out.extend_from_slice(s.as_bytes());
                Ok(())
            }
            "f" => {
                let (fmt_arg, rest) = args.split_first().ok_or_else(|| RunError::Arity {
                    what: "spill.f".into(),
                    expected: 1,
                    got: 0,
                })?;
                let fmt = match self.eval_expr(env, &fmt_arg.value)? {
                    Value::Str(s) => s,
                    other => {
                        return Err(RunError::Type(format!(
                            "spill.f format must be a str, got {}",
                            other.type_name()
                        )));
                    }
                };
                let mut vals = Vec::with_capacity(rest.len());
                for a in rest {
                    vals.push(self.eval_expr(env, &a.value)?);
                }
                let rendered = format_str(&fmt, &vals)?;
                self.out.extend_from_slice(rendered.as_bytes());
                Ok(())
            }
            other => Err(RunError::Unsupported(format!("spill.{other}"))),
        }
    }

    fn call_str(&mut self, env: &mut Env, method: &str, args: &[Arg]) -> Result<Value, RunError> {
        let vals = self.eval_args(env, args)?;
        match (method, vals.as_slice()) {
            ("glow", [Value::Str(s)]) => Ok(Value::Str(s.to_uppercase())),
            ("slaps", [Value::Str(a), Value::Str(b)]) => Ok(Value::Bool(a == b)),
            (m @ ("glow" | "slaps"), _) => Err(RunError::Type(format!(
                "str.{m} called with the wrong argument shape"
            ))),
            (other, _) => Err(RunError::Unsupported(format!("str.{other}"))),
        }
    }

    // ---- coercion -------------------------------------------------------------

    /// Apply a binding's declared type to a value where it is observable at runtime — namely,
    /// wrapping an integer into a sized-integer type (mirrors an explicit `as` cast).
    fn coerce(&self, val: Value, ty: Option<&Type>) -> Value {
        match (ty.map(|t| &t.kind), &val) {
            (Some(TypeKind::Named(name, _)), Value::Int(i)) => match int_type(name) {
                Some((bits, signed)) => Value::Int(wrap_int(*i as i128, bits, signed)),
                None => val,
            },
            _ => val,
        }
    }
}

// ================================================================================
// Free helpers (no `self`).
// ================================================================================

fn exactly_one(mut vals: Vec<Value>) -> Result<Value, RunError> {
    match vals.len() {
        1 => Ok(vals.pop().expect("length checked")),
        n => Err(RunError::Destructure {
            expected: 1,
            got: n,
        }),
    }
}

/// The bare name at the head of a type (`Player`, `Pair`, `int`), ignoring generic args.
fn type_head(ty: &Type) -> &str {
    match &ty.kind {
        TypeKind::Named(name, _) => name,
        TypeKind::Slice(inner) | TypeKind::Tag(inner) | TypeKind::Crib(inner) => type_head(inner),
        TypeKind::Array(elem, _) => type_head(elem),
        TypeKind::Fn(..) => "finna",
        TypeKind::RawPtr => "rawptr",
    }
}

/// `(bits, signed)` for a sized-integer type name, if it names one.
fn int_type(name: &str) -> Option<(u32, bool)> {
    Some(match name {
        "i8" => (8, true),
        "i16" => (16, true),
        "i32" => (32, true),
        "i64" | "int" => (64, true),
        "u8" => (8, false),
        "u16" => (16, false),
        "u32" => (32, false),
        "u64" => (64, false),
        _ => return None,
    })
}

/// Wrap `v` into a `bits`-wide integer of the given signedness (two's-complement).
fn wrap_int(v: i128, bits: u32, signed: bool) -> i64 {
    if bits >= 64 {
        // i64/u64 already fit our storage; nothing to truncate for the corpus range.
        return v as i64;
    }
    let modulus = 1i128 << bits;
    let mut m = v.rem_euclid(modulus);
    if signed && m >= (1i128 << (bits - 1)) {
        m -= modulus;
    }
    m as i64
}

fn compound_binop(op: AssignOp) -> BinOp {
    match op {
        AssignOp::AddEq => BinOp::Add,
        AssignOp::SubEq => BinOp::Sub,
        AssignOp::MulEq => BinOp::Mul,
        AssignOp::DivEq => BinOp::Div,
        AssignOp::RemEq => BinOp::Rem,
        AssignOp::AndEq => BinOp::BitAnd,
        AssignOp::OrEq => BinOp::BitOr,
        AssignOp::XorEq => BinOp::BitXor,
        AssignOp::ShlEq => BinOp::Shl,
        AssignOp::ShrEq => BinOp::Shr,
        AssignOp::Eq => unreachable!("plain `=` is handled before compound dispatch"),
    }
}

fn unary_op(op: UnOp, v: &Value) -> Result<Value, RunError> {
    match (op, v) {
        (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
        (UnOp::Neg, Value::Int(i)) => i
            .checked_neg()
            .map(Value::Int)
            .ok_or_else(|| RunError::Overflow("negation".into())),
        (UnOp::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
        (UnOp::BitNot, Value::Int(i)) => Ok(Value::Int(!i)),
        (op, v) => Err(RunError::Type(format!(
            "cannot apply `{op:?}` to {}",
            v.type_name()
        ))),
    }
}

fn binary_op(op: BinOp, l: &Value, r: &Value) -> Result<Value, RunError> {
    use BinOp::*;
    match op {
        // Equality is defined across matching scalar shapes.
        Eq => Ok(Value::Bool(values_equal(l, r))),
        Ne => Ok(Value::Bool(!values_equal(l, r))),
        And | Or => unreachable!("logical connectives are short-circuited in eval_binary"),
        _ => match (l, r) {
            (Value::Int(a), Value::Int(b)) => int_binary(op, *a, *b),
            (Value::Float(a), Value::Float(b)) => float_binary(op, *a, *b),
            (Value::Int(a), Value::Float(b)) => float_binary(op, *a as f64, *b),
            (Value::Float(a), Value::Int(b)) => float_binary(op, *a, *b as f64),
            (Value::Str(a), Value::Str(b)) if matches!(op, Add) => {
                Ok(Value::Str(format!("{a}{b}")))
            }
            _ => Err(RunError::Type(format!(
                "cannot apply `{op:?}` to {} and {}",
                l.type_name(),
                r.type_name()
            ))),
        },
    }
}

fn values_equal(l: &Value, r: &Value) -> bool {
    match (l, r) {
        (Value::Int(a), Value::Float(b)) => (*a as f64) == *b,
        (Value::Float(a), Value::Int(b)) => *a == (*b as f64),
        _ => l == r,
    }
}

fn int_binary(op: BinOp, a: i64, b: i64) -> Result<Value, RunError> {
    use BinOp::*;
    let arith = |o: Option<i64>, what: &str| {
        o.map(Value::Int)
            .ok_or_else(|| RunError::Overflow(what.to_string()))
    };
    match op {
        Add => arith(a.checked_add(b), "addition"),
        Sub => arith(a.checked_sub(b), "subtraction"),
        Mul => arith(a.checked_mul(b), "multiplication"),
        Div => {
            if b == 0 {
                Err(RunError::DivByZero)
            } else {
                arith(a.checked_div(b), "division")
            }
        }
        Rem => {
            if b == 0 {
                Err(RunError::DivByZero)
            } else {
                arith(a.checked_rem(b), "remainder")
            }
        }
        BitAnd => Ok(Value::Int(a & b)),
        BitOr => Ok(Value::Int(a | b)),
        BitXor => Ok(Value::Int(a ^ b)),
        Shl => Ok(Value::Int(a.wrapping_shl(b as u32))),
        Shr => Ok(Value::Int(a.wrapping_shr(b as u32))),
        Lt => Ok(Value::Bool(a < b)),
        Le => Ok(Value::Bool(a <= b)),
        Gt => Ok(Value::Bool(a > b)),
        Ge => Ok(Value::Bool(a >= b)),
        Eq | Ne | And | Or => unreachable!("handled by binary_op"),
    }
}

fn float_binary(op: BinOp, a: f64, b: f64) -> Result<Value, RunError> {
    use BinOp::*;
    match op {
        Add => Ok(Value::Float(a + b)),
        Sub => Ok(Value::Float(a - b)),
        Mul => Ok(Value::Float(a * b)),
        Div => Ok(Value::Float(a / b)),
        Rem => Ok(Value::Float(a % b)),
        Lt => Ok(Value::Bool(a < b)),
        Le => Ok(Value::Bool(a <= b)),
        Gt => Ok(Value::Bool(a > b)),
        Ge => Ok(Value::Bool(a >= b)),
        BitAnd | BitOr | BitXor | Shl | Shr => {
            Err(RunError::Type("bitwise operators need integers".into()))
        }
        Eq | Ne | And | Or => unreachable!("handled by binary_op"),
    }
}

fn cast(v: Value, ty: &Type) -> Result<Value, RunError> {
    let TypeKind::Named(name, _) = &ty.kind else {
        return Err(RunError::Unsupported(format!("cast to {ty:?}")));
    };
    if let Some((bits, signed)) = int_type(name) {
        let raw = match v {
            Value::Int(i) => i as i128,
            Value::Byte(b) => b as i128,
            Value::Float(f) => f.trunc() as i128,
            Value::Bool(b) => b as i128,
            other => {
                return Err(RunError::Type(format!(
                    "cannot cast {} to {name}",
                    other.type_name()
                )));
            }
        };
        return Ok(Value::Int(wrap_int(raw, bits, signed)));
    }
    match name.as_str() {
        "f32" | "f64" | "float" => match v {
            Value::Int(i) => Ok(Value::Float(i as f64)),
            Value::Float(f) => Ok(Value::Float(f)),
            Value::Byte(b) => Ok(Value::Float(b as f64)),
            other => Err(RunError::Type(format!(
                "cannot cast {} to {name}",
                other.type_name()
            ))),
        },
        other => Err(RunError::Unsupported(format!("cast to `{other}`"))),
    }
}

/// Expand a `spill.f` format string: `{}` consumes the next argument's display, `{{`/`}}` are
/// literal braces. No trailing newline is added (the corpus convention).
fn format_str(fmt: &str, args: &[Value]) -> Result<String, RunError> {
    let mut out = String::with_capacity(fmt.len());
    let mut next = 0usize;
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => match chars.peek() {
                Some('{') => {
                    chars.next();
                    out.push('{');
                }
                Some('}') => {
                    chars.next();
                    let val = args.get(next).ok_or_else(|| {
                        RunError::BadFormat(format!(
                            "format needs at least {} argument(s)",
                            next + 1
                        ))
                    })?;
                    out.push_str(&display(val));
                    next += 1;
                }
                _ => return Err(RunError::BadFormat("lone `{` in format string".into())),
            },
            '}' => match chars.peek() {
                Some('}') => {
                    chars.next();
                    out.push('}');
                }
                _ => return Err(RunError::BadFormat("lone `}` in format string".into())),
            },
            other => out.push(other),
        }
    }
    Ok(out)
}
