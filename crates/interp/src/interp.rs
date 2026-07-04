//! The tree-walking evaluator: registers a program's declarations, then executes `main`.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use frontend::ast::{
    Arg, AssignOp, BinOp, Block, CopInit, Expr, ExprKind, FnDecl, Item, MatchArm, Program, RetType,
    Stmt, StmtKind, StructLit, Type, TypeKind, UnOp, VarDecl,
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

/// One generational slot in an arena: the stored value, the generation currently stamped on
/// the slot, and whether it is presently allocated. A `cop` stamps a `tag` with the slot's
/// current `gen`; `evict` bumps every `gen` (invalidating all outstanding tags) and frees the
/// slots. A `holla` check succeeds only when a tag's `gen` still equals its slot's `gen`.
struct Slot {
    value: Value,
    generation: u64,
    live: bool,
}

/// An in-process arena backing a `crib`. `typed` cribs (`crib e: Enemy[N]`) hand `cop` a
/// generational [`Value::Tag`]; untyped bump cribs (`crib frame`) hand back the value directly.
struct Arena {
    slots: Vec<Slot>,
    typed: bool,
}

impl Arena {
    /// Allocate `value`, reusing the first freed slot (so post-`evict` a slot is reused at its
    /// newer generation) or growing the slab. Returns the slot index and the generation to
    /// stamp on the tag.
    fn alloc(&mut self, value: Value) -> (usize, u64) {
        if let Some((i, slot)) = self.slots.iter_mut().enumerate().find(|(_, s)| !s.live) {
            slot.value = value;
            slot.live = true;
            (i, slot.generation)
        } else {
            self.slots.push(Slot {
                value,
                generation: 0,
                live: true,
            });
            (self.slots.len() - 1, 0)
        }
    }

    /// A checked resolve (the `holla` live arm): the slot value iff the tag is still current.
    fn resolve(&self, slot: usize, generation: u64) -> Option<&Value> {
        self.slots
            .get(slot)
            .filter(|s| s.live && s.generation == generation)
            .map(|s| &s.value)
    }

    /// An unchecked resolve (`trust`): the slot value regardless of generation.
    fn resolve_unchecked(&self, slot: usize) -> Option<&Value> {
        self.slots.get(slot).map(|s| &s.value)
    }

    /// Write a value back into a slot iff the tag is still current — the `holla` live-arm
    /// writeback, so mutations to the bound reference persist into the crib (mutation-through-
    /// `holla`, matching the compiled path where the binding is a live pointer into the slot).
    fn write_slot(&mut self, slot: usize, generation: u64, value: Value) {
        if let Some(s) = self.slots.get_mut(slot)
            && s.live
            && s.generation == generation
        {
            s.value = value;
        }
    }

    /// Free the whole arena in O(slots): every slot's generation is bumped, invalidating all
    /// outstanding tags, and the slots become available for reuse.
    fn evict(&mut self) {
        for s in &mut self.slots {
            s.generation = s.generation.wrapping_add(1);
            s.live = false;
        }
    }
}

/// The interpreter: declaration tables, arenas, and the captured output buffer.
pub struct Interp<'p> {
    funcs: HashMap<String, &'p FnDecl>,
    methods: HashMap<(String, String), &'p FnDecl>,
    variants: HashMap<String, VariantInfo>,
    globals: HashMap<String, Value>,
    /// Names declared via `extern "C" finna` — resolved to a small built-in shim table so the
    /// interpreter can run FFI programs the compiled path links against libc for (`12-ffi`).
    externs: std::collections::HashSet<String>,
    /// Every live `crib`, keyed by the id carried in a [`Value::Crib`] handle.
    arenas: HashMap<usize, Arena>,
    /// Monotonic id source for arenas (never reused, so ids stay unique across `evict`).
    next_arena: usize,
    /// The declared return type of each `finna` on the active call stack, so `bounce` can build
    /// a correctly-shaped `(value, yikes)` early return.
    ret_stack: Vec<&'p RetType>,
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
            externs: std::collections::HashSet::new(),
            arenas: HashMap::new(),
            next_arena: 0,
            ret_stack: Vec::new(),
            out: Vec::new(),
        };
        // First pass: functions, methods, moods variants, and top-level cribs (order-independent).
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
                // A top-level `crib` is a program-lifetime arena reachable by name from every
                // function (corpus `08-memory`, `11-reference`); its name binds a handle globally.
                Item::Crib(c) => {
                    let id = me.new_arena(c.ty.is_some());
                    me.globals.insert(c.name.clone(), Value::Crib(id));
                }
                Item::Extern(e) => {
                    me.externs.insert(e.name.clone());
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

    /// Register a fresh arena and return its unique id (the payload of its [`Value::Crib`]).
    fn new_arena(&mut self, typed: bool) -> usize {
        let id = self.next_arena;
        self.next_arena += 1;
        self.arenas.insert(
            id,
            Arena {
                slots: Vec::new(),
                typed,
            },
        );
        id
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
        // Track this frame's return type so a `bounce` inside can shape its early return.
        self.ret_stack.push(&f.ret);
        let result = self.exec_block(&mut env, &f.body);
        self.ret_stack.pop();
        match result? {
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
                    // A vec iterates over a snapshot of its current contents.
                    Value::Vec(rc) => rc.borrow().clone(),
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
            // A function-local `crib`: a fresh arena, bound to its name in the current scope.
            StmtKind::Crib(c) => {
                let id = self.new_arena(c.ty.is_some());
                env.declare(&c.name, Value::Crib(id));
                Ok(Flow::Normal)
            }
            StmtKind::Holla {
                binding,
                tag,
                crib,
                live,
                ghosted,
            } => self.exec_holla(env, binding, tag, crib, live, ghosted),
            StmtKind::Sheesh { body, recover } => self.exec_sheesh(env, body, recover),
            StmtKind::Evict(e) => {
                let id = self.eval_crib(env, e)?;
                if let Some(arena) = self.arenas.get_mut(&id) {
                    arena.evict();
                }
                Ok(Flow::Normal)
            }
            StmtKind::Bounce(e) => self.exec_bounce(env, e),
            // Concurrency is out of this slice (corpus `13-concurrency` is `skip`).
            // `slide call()` — the interpreter is single-threaded, so it runs the task
            // synchronously. Corpus tasks are required to be observably order-independent, so a
            // deterministic inline run matches any real scheduling of the compiled path.
            StmtKind::Slide(call) => {
                self.eval_call_or_expr(env, call)?;
                Ok(Flow::Normal)
            }
        }
    }

    /// `holla binding = tag in crib { live } ghosted { ghosted }` — a checked tag resolve. The
    /// live arm binds a snapshot of the referenced value; a stale tag runs the ghosted arm.
    fn exec_holla(
        &mut self,
        env: &mut Env,
        binding: &str,
        tag: &Expr,
        crib: &Expr,
        live: &Block,
        ghosted: &Block,
    ) -> Result<Flow, RunError> {
        let tag_val = self.eval_expr(env, tag)?;
        let id = self.eval_crib(env, crib)?;
        let (slot, generation) = match tag_val {
            Value::Tag {
                slot, generation, ..
            } => (slot, generation),
            other => {
                return Err(RunError::Type(format!(
                    "`holla` needs a tag, got {}",
                    other.type_name()
                )));
            }
        };
        let resolved = self
            .arenas
            .get(&id)
            .and_then(|a| a.resolve(slot, generation))
            .cloned();
        match resolved {
            Some(val) => {
                let snapshot = val.clone();
                env.push();
                env.declare(binding, val);
                let flow = self.exec_block(env, live);
                // Persist a mutation of the bound reference back into the crib slot, so the
                // `holla` binding behaves like the compiled path's live pointer into the slot.
                // Only write back when the binding actually changed in this block: a nested
                // call may have mutated the same slot through its own `holla`, and writing our
                // untouched snapshot would clobber that (the reference is live, not a copy).
                if let Some(updated) = env.get(binding).cloned()
                    && updated != snapshot
                    && let Some(a) = self.arenas.get_mut(&id)
                {
                    a.write_slot(slot, generation, updated);
                }
                env.pop();
                flow
            }
            None => self.exec_block(env, ghosted),
        }
    }

    /// `sheesh { body } naw name { recover }` — run `body`, catching a propagating `yeet`. The
    /// recover arm (if present) binds the yeeted value to `name`; without one, the panic is
    /// swallowed.
    fn exec_sheesh(
        &mut self,
        env: &mut Env,
        body: &Block,
        recover: &Option<(String, Block)>,
    ) -> Result<Flow, RunError> {
        match self.exec_block(env, body) {
            Err(RunError::Yeet(v)) => match recover {
                Some((name, rblock)) => {
                    env.push();
                    env.declare(name, v);
                    let flow = self.exec_block(env, rblock);
                    env.pop();
                    flow
                }
                None => Ok(Flow::Normal),
            },
            other => other,
        }
    }

    /// `bounce e` — early-return-on-error sugar. When `e` is `ghosted` it is a no-op; otherwise
    /// it returns from the enclosing `finna` with the error in the last slot and type-defaulted
    /// zeros in the leading value slots (matching the `(value, yikes)` return shape).
    fn exec_bounce(&mut self, env: &mut Env, e: &Expr) -> Result<Flow, RunError> {
        let v = self.eval_expr(env, e)?;
        if matches!(v, Value::Ghosted) {
            return Ok(Flow::Normal);
        }
        let ret_tys: &[Type] = match self.ret_stack.last() {
            Some(RetType::Multi(tys)) => tys,
            Some(RetType::Single(t)) => std::slice::from_ref(t),
            _ => &[],
        };
        let mut vals: Vec<Value> = ret_tys
            .split_last()
            .map(|(_, leading)| leading.iter().map(default_value).collect())
            .unwrap_or_default();
        vals.push(v);
        Ok(Flow::Return(vals))
    }

    /// Evaluate a `crib` position (a name or `mem.scratch()`) to its arena id.
    fn eval_crib(&mut self, env: &mut Env, e: &Expr) -> Result<usize, RunError> {
        match self.eval_expr(env, e)? {
            Value::Crib(id) => Ok(id),
            other => Err(RunError::Type(format!(
                "expected a crib, got {}",
                other.type_name()
            ))),
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
            ExprKind::Index { base, index } => {
                let i = match self.eval_expr(env, index)? {
                    Value::Int(i) if i >= 0 => i as usize,
                    other => {
                        return Err(RunError::Type(format!(
                            "index must be a non-negative int, got {}",
                            other.type_name()
                        )));
                    }
                };
                let place = self.place_mut(env, base)?;
                match place {
                    Value::Array(xs) => {
                        let len = xs.len();
                        if let Some(slot) = xs.get_mut(i) {
                            *slot = val;
                            Ok(())
                        } else {
                            Err(RunError::Type(format!(
                                "index {i} out of bounds (len {len})"
                            )))
                        }
                    }
                    Value::Vec(rc) => {
                        let mut xs = rc.borrow_mut();
                        let len = xs.len();
                        if let Some(slot) = xs.get_mut(i) {
                            *slot = val;
                            Ok(())
                        } else {
                            Err(RunError::Type(format!(
                                "index {i} out of bounds (len {len})"
                            )))
                        }
                    }
                    other => Err(RunError::Type(format!(
                        "cannot index-assign into {}",
                        other.type_name()
                    ))),
                }
            }
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
                    Value::Vec(rc) => {
                        let xs = rc.borrow();
                        xs.get(i).cloned().ok_or_else(|| {
                            RunError::Type(format!("index {i} out of bounds (len {})", xs.len()))
                        })
                    }
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
            ExprKind::Cop { init, crib } => self.eval_cop(env, init, crib),
            ExprKind::Trust { tag, crib } => {
                let tag_val = self.eval_expr(env, tag)?;
                let id = self.eval_crib(env, crib)?;
                let slot = match tag_val {
                    Value::Tag { slot, .. } => slot,
                    other => {
                        return Err(RunError::Type(format!(
                            "`trust` needs a tag, got {}",
                            other.type_name()
                        )));
                    }
                };
                self.arenas
                    .get(&id)
                    .and_then(|a| a.resolve_unchecked(slot))
                    .cloned()
                    .ok_or_else(|| RunError::Type("`trust` on an out-of-range tag".into()))
            }
        }
    }

    /// `cop init in crib` — allocate `init` into the arena. A typed crib hands back a
    /// generational [`Value::Tag`]; an untyped bump crib hands back the value directly (a
    /// "direct reference"), matching corpus `08-memory`.
    fn eval_cop(&mut self, env: &mut Env, init: &CopInit, crib: &Expr) -> Result<Value, RunError> {
        let init_val = match init {
            CopInit::Struct(lit) => self.eval_struct_lit(env, lit)?,
            CopInit::Variant { name, args } => {
                let info = self
                    .variants
                    .get(name)
                    .ok_or_else(|| RunError::Undefined(name.clone()))?;
                let (moods, arity) = (info.moods.clone(), info.arity);
                self.construct_variant(env, name, &moods, arity, args)?
            }
        };
        let id = self.eval_crib(env, crib)?;
        let arena = self
            .arenas
            .get_mut(&id)
            .ok_or_else(|| RunError::Type("cop into an unknown crib".into()))?;
        if arena.typed {
            let (slot, generation) = arena.alloc(init_val);
            Ok(Value::Tag {
                arena: id,
                slot,
                generation,
            })
        } else {
            // Untyped bump arena: store it (so `evict` is meaningful) but return the value.
            arena.alloc(init_val.clone());
            Ok(init_val)
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
            if !shadowed {
                match modname.as_str() {
                    "spill" => return self.call_spill(env, method, args).map(|()| Vec::new()),
                    "str" => return self.call_str(env, method, args).map(|v| vec![v]),
                    "bytes" => return self.call_bytes(env, method, args).map(|v| vec![v]),
                    // `yikes.new(msg)` constructs an error value.
                    "yikes" => return self.call_yikes(env, method, args).map(|v| vec![v]),
                    // `stash.new[K, V]()` — or `stash.new(in: crib)`, the allocator-context
                    // override (SP0.1). The interpreter has no arena allocator, so `in:` is
                    // validated (must be a crib) then a no-op: the map is the same shared handle
                    // either way, so observable behavior matches the compiled path.
                    "stash" if method == "new" => {
                        self.eval_alloc_ctx_arg(env, args)?;
                        return Ok(vec![Value::Stash(Rc::new(RefCell::new(Vec::new())))]);
                    }
                    // `vec.new[T]()` constructs an empty growable vec — a shared, reference-counted
                    // handle (like `stash`), so `stack`/`pop` through any holder are visible to all,
                    // matching the compiled path's runtime-backed VecHandle.
                    "vec" if method == "new" => {
                        return Ok(vec![Value::Vec(Rc::new(RefCell::new(Vec::new())))]);
                    }
                    // `mem.scratch()` is a fresh, untyped per-frame arena.
                    "mem" if method == "scratch" => {
                        let id = self.new_arena(false);
                        return Ok(vec![Value::Crib(id)]);
                    }
                    "sys" => return self.call_sys(env, method, args).map(|v| vec![v]),
                    _ => {}
                }
            }
        }
        // `stack`/`pop` on a fixed `Array` mutate in place, so they need an addressable receiver
        // rather than an evaluated copy (squadops value semantics). On a `Vec` they go through the
        // shared handle instead; a temporary vec (e.g. returned from a call) is handled by the
        // fall-through to `call_method` -> `call_vec_method`.
        if matches!(method, "stack" | "pop") {
            let arg_vals = self.eval_args(env, args)?;
            match self.place_mut(env, receiver) {
                Ok(Value::Array(xs)) => {
                    return match method {
                        "stack" => {
                            let v = arg_vals
                                .into_iter()
                                .next()
                                .ok_or_else(|| RunError::Type("`stack` takes a value".into()))?;
                            xs.push(v);
                            Ok(Vec::new())
                        }
                        // "pop"
                        _ => {
                            let v = xs
                                .pop()
                                .ok_or_else(|| RunError::Type("`pop` on an empty array".into()))?;
                            Ok(vec![v])
                        }
                    };
                }
                Ok(Value::Vec(rc)) => {
                    let rc = rc.clone();
                    return vec_stack_pop(&rc, method, arg_vals);
                }
                _ => {}
            }
        }
        // Otherwise it's a user method call: evaluate the receiver, then dispatch.
        let recv_val = self.eval_expr(env, receiver)?;
        self.call_method(env, recv_val, method, args)
    }

    /// Pure `squad` collection methods on an array value: `gang` (length), `vibeCheck` (filter
    /// by a predicate), `glowUp` (map through a function).
    fn call_array_method(
        &mut self,
        env: &mut Env,
        xs: Vec<Value>,
        method: &str,
        args: &[Arg],
    ) -> Result<Vec<Value>, RunError> {
        match method {
            "gang" => Ok(vec![Value::Int(xs.len() as i64)]),
            "vibeCheck" => {
                let f = self.one_fn_arg(env, method, args)?;
                let mut out = Vec::new();
                for x in xs {
                    if matches!(self.apply_fn_value(&f, vec![x.clone()])?, Value::Bool(true)) {
                        out.push(x);
                    }
                }
                Ok(vec![Value::Array(out)])
            }
            "glowUp" => {
                let f = self.one_fn_arg(env, method, args)?;
                let mut out = Vec::with_capacity(xs.len());
                for x in xs {
                    out.push(self.apply_fn_value(&f, vec![x])?);
                }
                Ok(vec![Value::Array(out)])
            }
            other => Err(RunError::Undefined(format!("array.{other}"))),
        }
    }

    /// Evaluate an optional `in: <crib>` allocator-context argument (the SP0.1 override shared by
    /// the collection constructors). The interpreter has no arena allocator, so the context is
    /// evaluated for its side effects and validated to be a crib, then discarded — the resulting
    /// collection is identical, matching the compiled path's observable behavior. Positional args
    /// are rejected.
    fn eval_alloc_ctx_arg(&mut self, env: &mut Env, args: &[Arg]) -> Result<(), RunError> {
        match args {
            [] => Ok(()),
            [a] if a.label.as_deref() == Some("in") => match self.eval_expr(env, &a.value)? {
                Value::Crib(_) => Ok(()),
                other => Err(RunError::Type(format!(
                    "`in:` needs a crib allocator context, got {}",
                    other.type_name()
                ))),
            },
            _ => Err(RunError::Type(
                "collection constructor takes only an optional `in: <crib>`".into(),
            )),
        }
    }

    /// Evaluate the single function argument of a collection method (`vibeCheck`/`glowUp`).
    fn one_fn_arg(&mut self, env: &mut Env, method: &str, args: &[Arg]) -> Result<Value, RunError> {
        match args {
            [a] => self.eval_expr(env, &a.value),
            _ => Err(RunError::Type(format!(
                "`{method}` takes one `finna` argument"
            ))),
        }
    }

    /// Apply a first-class function value (or a function name) to pre-evaluated argument values,
    /// returning its single result — the higher-order callback in `vibeCheck`/`glowUp`.
    fn apply_fn_value(&mut self, f: &Value, arg_vals: Vec<Value>) -> Result<Value, RunError> {
        let Value::Fn(fname) = f else {
            return Err(RunError::Type(format!(
                "expected a `finna` value, got {}",
                f.type_name()
            )));
        };
        let decl = *self
            .funcs
            .get(fname)
            .ok_or_else(|| RunError::Undefined(fname.clone()))?;
        let mut out = self.call_fn(decl, arg_vals, None)?;
        Ok(out.drain(..).next().unwrap_or(Value::Ghosted))
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
            if self.externs.contains(name) {
                return self.call_extern_shim(env, name, args);
            }
            return Err(RunError::Undefined(name.clone()));
        }

        // An arbitrary callee expression that had better evaluate to a function value.
        match self.eval_expr(env, callee)? {
            Value::Fn(fname) => self.call_named_fn(env, &fname, args),
            other => Err(RunError::NotCallable(other.type_name().to_string())),
        }
    }

    /// A small built-in shim for the `extern "C"` functions the corpus links against libc for.
    /// The interpreter has no real FFI, so it emulates the handful of pure libc calls used by
    /// `12-ffi`; anything else is a clean "no interpreter shim" error rather than a crash.
    fn call_extern_shim(
        &mut self,
        env: &mut Env,
        name: &str,
        args: &[Arg],
    ) -> Result<Vec<Value>, RunError> {
        let vals = self.eval_args(env, args)?;
        match (name, vals.as_slice()) {
            ("abs", [Value::Int(x)]) => Ok(vec![Value::Int(x.abs())]),
            (other, _) => Err(RunError::Unsupported(format!(
                "no interpreter shim for extern `{other}`"
            ))),
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
        // `.tea(context)` on an error value wraps it, prefixing the context (Go's `%w`).
        if let Value::Yikes(msg) = &recv {
            if method == "tea" {
                let ctx = match self.eval_args(env, args)?.as_slice() {
                    [Value::Str(c)] => c.clone(),
                    _ => {
                        return Err(RunError::Type("`.tea` takes a single str context".into()));
                    }
                };
                return Ok(vec![Value::Yikes(format!("{ctx}: {msg}"))]);
            }
            return Err(RunError::Undefined(format!("yikes.{method}")));
        }
        // `stash` methods dispatch on the reference-counted map (mutations are shared).
        if let Value::Stash(map) = &recv {
            let map = map.clone();
            return self.call_stash(env, map, method, args);
        }
        // `vec` methods dispatch on the reference-counted handle: `stack`/`pop` mutate it in place
        // (shared with every holder); `gang`/`vibeCheck`/`glowUp` read a snapshot.
        if let Value::Vec(rc) = &recv {
            let rc = rc.clone();
            return match method {
                "stack" | "pop" => {
                    let arg_vals = self.eval_args(env, args)?;
                    vec_stack_pop(&rc, method, arg_vals)
                }
                // `v.append(s)` — bulk-append a str's bytes (the string-builder primitive).
                "append" => {
                    let arg_vals = self.eval_args(env, args)?;
                    match arg_vals.as_slice() {
                        [Value::Str(s)] => {
                            let mut buf = rc.borrow_mut();
                            buf.extend(s.bytes().map(Value::Byte));
                            Ok(Vec::new())
                        }
                        _ => Err(RunError::Type("`vec.append` takes a single str".into())),
                    }
                }
                // `v.str()` — collect a `vec[u8]` into an owned str (unchecked, like the compiled
                // `bet_str_concat` copy; the builder only ever holds valid UTF-8).
                "str" => {
                    if !args.is_empty() {
                        return Err(RunError::Type("`vec.str` takes no arguments".into()));
                    }
                    let bytes = bytes_of(rc.borrow().as_slice())?;
                    Ok(vec![Value::Str(
                        String::from_utf8_lossy(&bytes).into_owned(),
                    )])
                }
                _ => {
                    let xs = rc.borrow().clone();
                    self.call_array_method(env, xs, method, args)
                }
            };
        }
        // Pure collection methods on an array (squadops): `gang`, `vibeCheck`, `glowUp`. The
        // mutating `stack`/`pop` are handled earlier in `eval_method_call` (they need a place).
        if let Value::Array(xs) = &recv {
            let xs = xs.clone();
            return self.call_array_method(env, xs, method, args);
        }
        let ty = match &recv {
            Value::Struct { ty, .. } => ty.clone(),
            other => {
                return Err(RunError::Type(format!(
                    "no method `{method}` on {}",
                    other.type_name()
                )));
            }
        };
        let f = match self.methods.get(&(ty.clone(), method.to_string())).copied() {
            Some(f) => f,
            None => {
                // A function-pointer struct field called through the receiver: `m.think(e)`.
                // The field's `finna` value governs the call; the receiver is not prepended.
                if let Value::Struct { fields, .. } = &recv
                    && let Some(Value::Fn(fname)) = fields.get(method)
                {
                    let fname = fname.clone();
                    return self.call_named_fn(env, &fname, args);
                }
                return Err(RunError::Undefined(format!("{ty}.{method}")));
            }
        };
        let arg_vals = self.eval_args(env, args)?;
        self.call_fn(f, arg_vals, Some(recv))
    }

    /// Dispatch a `stash` method on its shared map: `put`, `peep`, `yeet`, or `gang`. Mutations
    /// go through the `Rc<RefCell<..>>`, so they are visible to every holder of the map.
    fn call_stash(
        &mut self,
        env: &mut Env,
        map: Rc<RefCell<Vec<(Value, Value)>>>,
        method: &str,
        args: &[Arg],
    ) -> Result<Vec<Value>, RunError> {
        let arg_vals = self.eval_args(env, args)?;
        match method {
            "put" => {
                let (k, v) = match arg_vals.as_slice() {
                    [k, v] => (k.clone(), v.clone()),
                    _ => return Err(RunError::Type("`stash.put` takes a key and a value".into())),
                };
                let mut m = map.borrow_mut();
                if let Some(e) = m.iter_mut().find(|(ek, _)| *ek == k) {
                    e.1 = v;
                } else {
                    m.push((k, v));
                }
                Ok(vec![])
            }
            "peep" => {
                let k = match arg_vals.as_slice() {
                    [k] => k,
                    _ => return Err(RunError::Type("`stash.peep` takes a single key".into())),
                };
                let m = map.borrow();
                match m.iter().find(|(ek, _)| ek == k) {
                    Some((_, v)) => Ok(vec![v.clone(), Value::Bool(true)]),
                    // On a miss the value slot is unused (the caller checks the flag first).
                    None => Ok(vec![Value::Ghosted, Value::Bool(false)]),
                }
            }
            "yeet" => {
                let k = match arg_vals.as_slice() {
                    [k] => k,
                    _ => return Err(RunError::Type("`stash.yeet` takes a single key".into())),
                };
                let mut m = map.borrow_mut();
                let before = m.len();
                m.retain(|(ek, _)| ek != k);
                Ok(vec![Value::Bool(m.len() != before)])
            }
            "gang" => Ok(vec![Value::Int(map.borrow().len() as i64)]),
            other => Err(RunError::Undefined(format!("stash.{other}"))),
        }
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
            // `str.len(s)` — byte length as an `int` (matches the fat-`str` len projection).
            ("len", [Value::Str(s)]) => Ok(Value::Int(s.len() as i64)),
            // `str.at(s, i)` — the byte at index `i`, as an `int` (0..=255).
            ("at", [Value::Str(s), Value::Int(i)]) if *i >= 0 => {
                let bytes = s.as_bytes();
                let i = *i as usize;
                match bytes.get(i) {
                    Some(b) => Ok(Value::Int(i64::from(*b))),
                    None => Err(RunError::Type(format!(
                        "str.at index {i} out of range (len {})",
                        bytes.len()
                    ))),
                }
            }
            // `str.sub(s, start, end)` — the byte substring `s[start..end]`.
            ("sub", [Value::Str(s), Value::Int(a), Value::Int(b)]) if *a >= 0 && *b >= *a => {
                let bytes = s.as_bytes();
                let (a, b) = (*a as usize, *b as usize);
                if b > bytes.len() {
                    return Err(RunError::Type(format!(
                        "str.sub end {b} out of range (len {})",
                        bytes.len()
                    )));
                }
                Ok(Value::Str(
                    String::from_utf8_lossy(&bytes[a..b]).into_owned(),
                ))
            }
            // `str.bytes(s)` — a `[]u8` view (each element a byte value).
            ("bytes", [Value::Str(s)]) => Ok(Value::Array(
                s.as_bytes().iter().map(|b| Value::Byte(*b)).collect(),
            )),
            // `str.fromBytesTrust(b)` — unchecked `[]u8` -> `str`.
            ("fromBytesTrust", [Value::Array(bs)]) => Ok(Value::Str(
                String::from_utf8_lossy(&bytes_of(bs)?).into_owned(),
            )),
            // `str.fromBytes(b)` — checked `[]u8` -> `str`, empty on malformed UTF-8.
            ("fromBytes", [Value::Array(bs)]) => Ok(Value::Str(
                String::from_utf8(bytes_of(bs)?).unwrap_or_default(),
            )),
            (
                m @ ("glow" | "slaps" | "len" | "at" | "sub" | "bytes" | "fromBytes"
                | "fromBytesTrust"),
                _,
            ) => Err(RunError::Type(format!(
                "str.{m} called with the wrong argument shape"
            ))),
            (other, _) => Err(RunError::Unsupported(format!("str.{other}"))),
        }
    }

    /// The `yikes` error constructor: `yikes.new(msg)` builds an error value.
    fn call_yikes(&mut self, env: &mut Env, method: &str, args: &[Arg]) -> Result<Value, RunError> {
        match method {
            "new" => match self.eval_args(env, args)?.as_slice() {
                [Value::Str(m)] => Ok(Value::Yikes(m.clone())),
                _ => Err(RunError::Type(
                    "`yikes.new` takes a single str message".into(),
                )),
            },
            other => Err(RunError::Unsupported(format!("yikes.{other}"))),
        }
    }

    /// A minimal `bytes` module: `bytes.readU32le(buf, off)` decodes a little-endian u32 out of
    /// a `[]u8` (corpus `10-stdlib/bytes-parse`).
    /// `sys.arg(i)` / `sys.argc()` — process arguments. Under the interpreter these are the host
    /// process's args (`bet run …`), which differ from a compiled binary's; only out-of-range
    /// emptiness and the "argv[0] always exists" invariant are portable across paths (see the
    /// `sys-args` corpus test). Uses `args_os` so a non-UTF-8 arg never panics.
    fn call_sys(&mut self, env: &mut Env, method: &str, args: &[Arg]) -> Result<Value, RunError> {
        let vals = self.eval_args(env, args)?;
        match (method, vals.as_slice()) {
            ("argc", []) => Ok(Value::Int(std::env::args_os().count() as i64)),
            ("arg", [Value::Int(i)]) if *i >= 0 => {
                let arg = std::env::args_os()
                    .nth(*i as usize)
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Ok(Value::Str(arg))
            }
            ("arg", [Value::Int(_)]) => Ok(Value::Str(String::new())),
            ("argc", _) => Err(RunError::Type("`sys.argc` takes no arguments".into())),
            ("arg", _) => Err(RunError::Type("`sys.arg` takes a single index".into())),
            (other, _) => Err(RunError::Unsupported(format!("sys.{other}"))),
        }
    }

    fn call_bytes(&mut self, env: &mut Env, method: &str, args: &[Arg]) -> Result<Value, RunError> {
        let vals = self.eval_args(env, args)?;
        match (method, vals.as_slice()) {
            ("readU32le", [Value::Array(bytes), Value::Int(off)]) if *off >= 0 => {
                let off = *off as usize;
                let mut acc: u32 = 0;
                for k in 0..4 {
                    let byte = match bytes.get(off + k) {
                        Some(Value::Int(b)) => (*b as u32) & 0xFF,
                        Some(Value::Byte(b)) => *b as u32,
                        _ => {
                            return Err(RunError::Type(
                                "bytes.readU32le needs 4 in-range byte elements".into(),
                            ));
                        }
                    };
                    acc |= byte << (8 * k as u32);
                }
                Ok(Value::Int(acc as i64))
            }
            ("readU32le", _) => Err(RunError::Type(
                "bytes.readU32le(buf, off) called with the wrong argument shape".into(),
            )),
            (other, _) => Err(RunError::Unsupported(format!("bytes.{other}"))),
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

/// The mutating `vec` methods on the shared handle: `stack` (push) and `pop`. Mutations go
/// through the `Rc<RefCell<..>>`, so they are visible to every holder — matching the compiled
/// path's runtime-backed VecHandle.
fn vec_stack_pop(
    rc: &Rc<RefCell<Vec<Value>>>,
    method: &str,
    arg_vals: Vec<Value>,
) -> Result<Vec<Value>, RunError> {
    match method {
        "stack" => {
            let v = arg_vals
                .into_iter()
                .next()
                .ok_or_else(|| RunError::Type("`stack` takes a value".into()))?;
            rc.borrow_mut().push(v);
            Ok(Vec::new())
        }
        "pop" => {
            let v = rc
                .borrow_mut()
                .pop()
                .ok_or_else(|| RunError::Type("`pop` on an empty vec".into()))?;
            Ok(vec![v])
        }
        other => Err(RunError::Undefined(format!("vec.{other}"))),
    }
}

/// Collect a `[]u8`-shaped array (elements are byte or in-range int values) into raw bytes,
/// for `str.fromBytes` / `str.fromBytesTrust`.
fn bytes_of(vals: &[Value]) -> Result<Vec<u8>, RunError> {
    vals.iter()
        .map(|v| match v {
            Value::Byte(b) => Ok(*b),
            Value::Int(i) => Ok(*i as u8),
            _ => Err(RunError::Type(
                "str.fromBytes* needs a []u8 (byte-valued elements)".into(),
            )),
        })
        .collect()
}

fn exactly_one(mut vals: Vec<Value>) -> Result<Value, RunError> {
    match vals.len() {
        1 => Ok(vals.pop().expect("length checked")),
        n => Err(RunError::Destructure {
            expected: 1,
            got: n,
        }),
    }
}

/// The zero value for a declared type, used to fill the leading value slots of a `bounce`
/// early return. Only the shapes that appear as non-error return slots need real defaults;
/// anything else falls back to `ghosted`.
fn default_value(ty: &Type) -> Value {
    match &ty.kind {
        TypeKind::Named(name, _) => match name.as_str() {
            "i8" | "i16" | "i32" | "i64" | "int" | "u8" | "u16" | "u32" | "u64" => Value::Int(0),
            "f32" | "f64" | "float" => Value::Float(0.0),
            "bool" => Value::Bool(false),
            "str" => Value::Str(String::new()),
            _ => Value::Ghosted,
        },
        _ => Value::Ghosted,
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
