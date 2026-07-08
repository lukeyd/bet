//! The resolve-and-mangle pass: turn a loaded [`ModuleGraph`] into one merged [`ast::Program`]
//! whose names are globally unique, so the interpreter, lowering, and backend need no notion of
//! modules at all.
//!
//! Each non-root module's top-level declarations are renamed to a per-module-unique mangled name
//! (`geometry$2$area`); the `$` is illegal in a source identifier, so a mangled name can never
//! collide with a user name. The **root (entry) file keeps its names unmangled** so `main` stays
//! `main`. Every reference is rewritten to match:
//!
//! * a **bare** reference to the current module's own top-level item → that item's mangled name;
//! * a **qualified** reference `ns.member` (where `ns` is an imported namespace) → the target
//!   module's mangled name, provided the item is `flex` (public) — otherwise a load error;
//! * a `spill.it(…)`-style built-in module receiver, or a local variable → left untouched.
//!
//! `ns.fn(args)` (parsed as a `Method`) becomes a plain `Call` to the mangled function; `ns.CONST`
//! (a `Field`) becomes a mangled `Name`; qualified types / struct literals / variant constructors
//! resolve the same way. Modules are concatenated in the graph's post-order (dependencies first,
//! root last) for reproducible output.

use crate::CompileError;
use crate::ast::{
    self, Arg, Block, CopInit, Expr, ExprKind, FieldInit, Item, MatchArm, Stmt, StmtKind,
    StructLit, Type, TypeKind, Vis,
};
use crate::loader::{Import, ModuleGraph};
use std::collections::{HashMap, HashSet};

/// What a top-level name denotes — enough to decide whether it may appear in type position.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SymKind {
    Func,
    Value, // facts const, module-level lowkey var, or crib
    Struct,
    Sum,
    Variant,
}

#[derive(Clone)]
struct Sym {
    kind: SymKind,
    vis: Vis,
    mangled: String,
}

type Table = HashMap<String, Sym>;

/// Resolve a loaded graph into one merged program.
pub(crate) fn resolve(graph: &ModuleGraph) -> Result<ast::Program, CompileError> {
    // Per-module mangle prefix: empty for the root (names unchanged), `<stem>$<idx>$` otherwise.
    // The index (unique per file) disambiguates two imported files that share a stem.
    let prefixes: Vec<String> = graph
        .modules
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if m.is_root {
                String::new()
            } else {
                format!("{}${}$", m.stem, i)
            }
        })
        .collect();

    let tables: Vec<Table> = graph
        .modules
        .iter()
        .enumerate()
        .map(|(i, m)| build_table(&m.program, &prefixes[i]))
        .collect();

    let mut items = Vec::new();
    for (i, m) in graph.modules.iter().enumerate() {
        let mut r = Resolver {
            modidx: i,
            tables: &tables,
            imports: &m.imports,
            scopes: Vec::new(),
            type_params: HashSet::new(),
        };
        for item in &m.program.items {
            if matches!(item, Item::Pull(_)) {
                continue; // imports have done their job; drop them from the merged program
            }
            items.push(r.item(item.clone())?);
        }
    }
    Ok(ast::Program { items })
}

/// Build a module's top-level symbol table, keyed by source name → mangled name + kind + vis.
fn build_table(prog: &ast::Program, prefix: &str) -> Table {
    let mut t = Table::new();
    let mut put = |name: &str, kind: SymKind, vis: Vis| {
        t.insert(
            name.to_string(),
            Sym {
                kind,
                vis,
                mangled: format!("{prefix}{name}"),
            },
        );
    };
    for item in &prog.items {
        match item {
            Item::Func(f) => put(&f.name, SymKind::Func, f.vis),
            Item::Const(c) => put(&c.name, SymKind::Value, c.vis),
            Item::Var(v) => {
                for name in &v.targets {
                    put(name, SymKind::Value, v.vis);
                }
            }
            Item::Crib(c) => put(&c.name, SymKind::Value, c.vis),
            Item::Drip(d) => put(&d.name, SymKind::Struct, d.vis),
            Item::Moods(m) => {
                put(&m.name, SymKind::Sum, m.vis);
                for v in &m.variants {
                    // A variant is reachable across files iff its enum is `flex`.
                    put(&v.name, SymKind::Variant, m.vis);
                }
            }
            // Externs are FFI/ABI symbols — never mangled, never namespaced.
            Item::Extern(_) | Item::Pull(_) => {}
        }
    }
    t
}

struct Resolver<'a> {
    modidx: usize,
    tables: &'a [Table],
    imports: &'a [Import],
    /// Lexical scopes of local bindings (params, `lowkey`, loop vars, …). A local shadows a
    /// top-level name and an imported namespace within its scope.
    scopes: Vec<HashSet<String>>,
    /// Generic type parameters of the current item — never mangled in type position.
    type_params: HashSet<String>,
}

impl Resolver<'_> {
    fn own(&self) -> &Table {
        &self.tables[self.modidx]
    }

    fn is_local(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains(name))
    }

    fn bind(&mut self, name: &str) {
        if let Some(top) = self.scopes.last_mut() {
            top.insert(name.to_string());
        }
    }

    /// If `e` is a bare name bound to an imported namespace (and not shadowed by a local), return
    /// the target module index plus the namespace name (for diagnostics).
    fn as_namespace<'e>(&self, e: &'e Expr) -> Option<(usize, &'e str)> {
        if let ExprKind::Name { name, generics } = &e.kind
            && generics.is_empty()
            && !self.is_local(name)
            && let Some(imp) = self.imports.iter().find(|i| &i.name == name)
        {
            return Some((imp.target, name));
        }
        None
    }

    /// Resolve `member` as a `flex` export of module `target`.
    fn lookup_flex(&self, target: usize, member: &str, ns: &str) -> Result<Sym, CompileError> {
        match self.tables[target].get(member) {
            None => Err(CompileError::Load(format!(
                "module `{ns}` has no exported item `{member}`"
            ))),
            Some(s) if s.vis != Vis::Flex => Err(CompileError::Load(format!(
                "`{member}` is hush (private) in module `{ns}`; mark it `flex` to export it"
            ))),
            Some(s) => Ok(s.clone()),
        }
    }

    /// Resolve a possibly-qualified name string (`"ns.member"` or bare). Used for type names and
    /// struct-literal / variant heads, which carry the namespace inside a single string.
    fn resolve_named(&self, name: &str, want_type: bool) -> Result<String, CompileError> {
        if let Some((ns, member)) = name.split_once('.') {
            let target = self
                .imports
                .iter()
                .find(|i| i.name == ns)
                .ok_or_else(|| {
                    CompileError::Load(format!("unknown module namespace `{ns}` in `{name}`"))
                })?
                .target;
            return Ok(self.lookup_flex(target, member, ns)?.mangled);
        }
        // Bare: mangle iff it's this module's own top-level item (of the right flavor).
        if self.type_params.contains(name) {
            return Ok(name.to_string());
        }
        if let Some(sym) = self.own().get(name) {
            let usable = if want_type {
                matches!(sym.kind, SymKind::Struct | SymKind::Sum)
            } else {
                true
            };
            if usable {
                return Ok(sym.mangled.clone());
            }
        }
        Ok(name.to_string())
    }

    // ---- items -----------------------------------------------------------------------------

    fn item(&mut self, item: Item) -> Result<Item, CompileError> {
        Ok(match item {
            Item::Func(mut f) => {
                f.name = self.own_mangle(&f.name);
                self.type_params = f.generics.iter().cloned().collect();
                self.scopes.push(HashSet::new());
                if let Some(recv) = &mut f.receiver {
                    recv.ty = self.ty(recv.ty.clone())?;
                    self.bind(&recv.name);
                }
                for p in &mut f.params {
                    p.ty = self.ty(p.ty.clone())?;
                    self.bind(&p.name);
                }
                f.ret = self.ret(f.ret)?;
                f.body = self.block(f.body, &[])?;
                self.scopes.pop();
                self.type_params.clear();
                Item::Func(f)
            }
            Item::Drip(mut d) => {
                d.name = self.own_mangle(&d.name);
                self.type_params = d.generics.iter().cloned().collect();
                for fld in &mut d.fields {
                    fld.ty = self.ty(fld.ty.clone())?;
                }
                self.type_params.clear();
                Item::Drip(d)
            }
            Item::Moods(mut m) => {
                m.name = self.own_mangle(&m.name);
                self.type_params = m.generics.iter().cloned().collect();
                for v in &mut m.variants {
                    v.name = self.own_mangle(&v.name);
                    for t in &mut v.payload {
                        *t = self.ty(t.clone())?;
                    }
                }
                self.type_params.clear();
                Item::Moods(m)
            }
            Item::Const(mut c) => {
                c.name = self.own_mangle(&c.name);
                if let Some(t) = c.ty {
                    c.ty = Some(self.ty(t)?);
                }
                c.value = self.expr(c.value)?;
                Item::Const(c)
            }
            Item::Var(mut v) => {
                for name in &mut v.targets {
                    *name = self.own_mangle(name);
                }
                if let Some(t) = v.ty {
                    v.ty = Some(self.ty(t)?);
                }
                v.values = v
                    .values
                    .into_iter()
                    .map(|e| self.expr(e))
                    .collect::<Result<_, _>>()?;
                Item::Var(v)
            }
            Item::Crib(mut c) => {
                c.name = self.own_mangle(&c.name);
                if let Some(t) = c.ty {
                    c.ty = Some(self.ty(t)?);
                }
                Item::Crib(c)
            }
            Item::Extern(mut e) => {
                // Name stays bare (ABI symbol); param/return types may still name user structs.
                for p in &mut e.params {
                    p.ty = self.ty(p.ty.clone())?;
                }
                e.ret = self.ret(e.ret)?;
                Item::Extern(e)
            }
            Item::Pull(p) => Item::Pull(p), // unreachable: pulls are dropped by resolve()
        })
    }

    /// Mangle a declaration name via this module's own table (root prefix is empty → unchanged).
    fn own_mangle(&self, name: &str) -> String {
        self.own()
            .get(name)
            .map(|s| s.mangled.clone())
            .unwrap_or_else(|| name.to_string())
    }

    fn ret(&mut self, ret: ast::RetType) -> Result<ast::RetType, CompileError> {
        Ok(match ret {
            ast::RetType::None => ast::RetType::None,
            ast::RetType::Single(t) => ast::RetType::Single(self.ty(t)?),
            ast::RetType::Multi(ts) => ast::RetType::Multi(
                ts.into_iter()
                    .map(|t| self.ty(t))
                    .collect::<Result<_, _>>()?,
            ),
        })
    }

    // ---- types -----------------------------------------------------------------------------

    fn ty(&mut self, t: Type) -> Result<Type, CompileError> {
        let kind = match t.kind {
            TypeKind::Slice(b) => TypeKind::Slice(Box::new(self.ty(*b)?)),
            TypeKind::Array(b, n) => TypeKind::Array(Box::new(self.ty(*b)?), n),
            TypeKind::Tag(b) => TypeKind::Tag(Box::new(self.ty(*b)?)),
            TypeKind::Crib(b) => TypeKind::Crib(Box::new(self.ty(*b)?)),
            TypeKind::Soa(b) => TypeKind::Soa(Box::new(self.ty(*b)?)),
            TypeKind::Fn(ps, r) => TypeKind::Fn(
                ps.into_iter()
                    .map(|p| self.ty(p))
                    .collect::<Result<_, _>>()?,
                Box::new(self.ty(*r)?),
            ),
            TypeKind::RawPtr => TypeKind::RawPtr,
            TypeKind::Named(name, generics) => {
                let name = self.resolve_named(&name, true)?;
                TypeKind::Named(name, self.tys(generics)?)
            }
        };
        Ok(Type { kind, span: t.span })
    }

    fn tys(&mut self, ts: Vec<Type>) -> Result<Vec<Type>, CompileError> {
        ts.into_iter().map(|t| self.ty(t)).collect()
    }

    // ---- statements ------------------------------------------------------------------------

    /// Walk a block in a fresh scope. `binds` are names introduced by the enclosing construct
    /// (loop var, `holla`/`sheesh`/arm bindings) that are visible for the block's whole extent.
    fn block(&mut self, b: Block, binds: &[String]) -> Result<Block, CompileError> {
        self.scopes.push(HashSet::new());
        for name in binds {
            self.bind(name);
        }
        let mut stmts = Vec::with_capacity(b.stmts.len());
        for s in b.stmts {
            stmts.push(self.stmt(s)?);
        }
        self.scopes.pop();
        Ok(Block {
            stmts,
            span: b.span,
        })
    }

    fn stmt(&mut self, s: Stmt) -> Result<Stmt, CompileError> {
        let kind = match s.kind {
            StmtKind::Var(mut v) => {
                if let Some(t) = v.ty {
                    v.ty = Some(self.ty(t)?);
                }
                v.values = v
                    .values
                    .into_iter()
                    .map(|e| self.expr(e))
                    .collect::<Result<_, _>>()?;
                for name in &v.targets {
                    self.bind(name); // local — do NOT mangle the target
                }
                StmtKind::Var(v)
            }
            StmtKind::Const(mut c) => {
                if let Some(t) = c.ty {
                    c.ty = Some(self.ty(t)?);
                }
                c.value = self.expr(c.value)?;
                self.bind(&c.name);
                StmtKind::Const(c)
            }
            StmtKind::Crib(mut c) => {
                if let Some(t) = c.ty {
                    c.ty = Some(self.ty(t)?);
                }
                self.bind(&c.name);
                StmtKind::Crib(c)
            }
            StmtKind::Fr(fr) => {
                let cond = self.expr(fr.cond)?;
                let then = self.block(fr.then, &[])?;
                let mut elifs = Vec::with_capacity(fr.elifs.len());
                for (c, b) in fr.elifs {
                    elifs.push((self.expr(c)?, self.block(b, &[])?));
                }
                let els = match fr.els {
                    Some(b) => Some(self.block(b, &[])?),
                    None => None,
                };
                StmtKind::Fr(ast::FrStmt {
                    cond,
                    then,
                    elifs,
                    els,
                })
            }
            StmtKind::Vibin { cond, body } => StmtKind::Vibin {
                cond: self.expr(cond)?,
                body: self.block(body, &[])?,
            },
            StmtKind::Squad { var, iter, body } => {
                let iter = self.expr(iter)?;
                let body = self.block(body, std::slice::from_ref(&var))?;
                StmtKind::Squad { var, iter, body }
            }
            StmtKind::Vibe {
                scrutinee,
                arms,
                default,
            } => {
                let scrutinee = self.expr(scrutinee)?;
                let mut new_arms = Vec::with_capacity(arms.len());
                for arm in arms {
                    // The variant name is a constructor reference — mangle like any other.
                    let variant = self.resolve_named(&arm.variant, false)?;
                    let body = self.block(arm.body, &arm.bindings)?;
                    new_arms.push(MatchArm {
                        variant,
                        bindings: arm.bindings,
                        body,
                        span: arm.span,
                    });
                }
                let default = match default {
                    Some(b) => Some(self.block(b, &[])?),
                    None => None,
                };
                StmtKind::Vibe {
                    scrutinee,
                    arms: new_arms,
                    default,
                }
            }
            StmtKind::Holla {
                binding,
                tag,
                crib,
                live,
                ghosted,
            } => {
                let tag = self.expr(tag)?;
                let crib = self.expr(crib)?;
                let live = self.block(live, std::slice::from_ref(&binding))?;
                let ghosted = self.block(ghosted, &[])?;
                StmtKind::Holla {
                    binding,
                    tag,
                    crib,
                    live,
                    ghosted,
                }
            }
            StmtKind::Sheesh { body, recover } => {
                let body = self.block(body, &[])?;
                let recover = match recover {
                    Some((name, b)) => {
                        let b = self.block(b, std::slice::from_ref(&name))?;
                        Some((name, b))
                    }
                    None => None,
                };
                StmtKind::Sheesh { body, recover }
            }
            StmtKind::Evict { crib, tag } => StmtKind::Evict {
                crib: self.expr(crib)?,
                tag: tag.map(|t| self.expr(t)).transpose()?,
            },
            StmtKind::Slide(e) => StmtKind::Slide(self.expr(e)?),
            StmtKind::Bet(es) => StmtKind::Bet(self.exprs(es)?),
            StmtKind::Bounce(e) => StmtKind::Bounce(self.expr(e)?),
            StmtKind::Yeet(e) => StmtKind::Yeet(self.expr(e)?),
            StmtKind::Dip => StmtKind::Dip,
            StmtKind::Skip => StmtKind::Skip,
            StmtKind::Assign {
                targets,
                op,
                values,
            } => StmtKind::Assign {
                targets: self.exprs(targets)?,
                op,
                values: self.exprs(values)?,
            },
            StmtKind::Expr(e) => StmtKind::Expr(self.expr(e)?),
        };
        Ok(Stmt { kind, span: s.span })
    }

    // ---- expressions -----------------------------------------------------------------------

    fn exprs(&mut self, es: Vec<Expr>) -> Result<Vec<Expr>, CompileError> {
        es.into_iter().map(|e| self.expr(e)).collect()
    }

    fn args(&mut self, args: Vec<Arg>) -> Result<Vec<Arg>, CompileError> {
        args.into_iter()
            .map(|a| {
                Ok(Arg {
                    label: a.label,
                    value: self.expr(a.value)?,
                })
            })
            .collect()
    }

    fn expr(&mut self, e: Expr) -> Result<Expr, CompileError> {
        let span = e.span;
        let kind = match e.kind {
            ExprKind::Name { name, generics } => {
                let generics = self.tys(generics)?;
                let name = if self.is_local(&name) {
                    name
                } else {
                    self.resolve_named(&name, false)?
                };
                ExprKind::Name { name, generics }
            }
            ExprKind::Method {
                receiver,
                method,
                generics,
                args,
            } => {
                if let Some((target, ns)) = self.as_namespace(&receiver) {
                    // `ns.method(args)` — a qualified call: resolve to the mangled free function
                    // (or variant constructor) and drop the receiver.
                    let mangled = self.lookup_flex(target, &method, ns)?.mangled;
                    let generics = self.tys(generics)?;
                    let args = self.args(args)?;
                    ExprKind::Call {
                        callee: Box::new(Expr {
                            kind: ExprKind::Name {
                                name: mangled,
                                generics,
                            },
                            span,
                        }),
                        args,
                    }
                } else {
                    ExprKind::Method {
                        receiver: Box::new(self.expr(*receiver)?),
                        method,
                        generics: self.tys(generics)?,
                        args: self.args(args)?,
                    }
                }
            }
            ExprKind::Field {
                base,
                name,
                generics,
            } => {
                if let Some((target, ns)) = self.as_namespace(&base) {
                    // `ns.CONST` (or a nullary variant / fn value): a qualified value reference.
                    let mangled = self.lookup_flex(target, &name, ns)?.mangled;
                    ExprKind::Name {
                        name: mangled,
                        generics: self.tys(generics)?,
                    }
                } else {
                    ExprKind::Field {
                        base: Box::new(self.expr(*base)?),
                        name,
                        generics: self.tys(generics)?,
                    }
                }
            }
            ExprKind::Call { callee, args } => ExprKind::Call {
                callee: Box::new(self.expr(*callee)?),
                args: self.args(args)?,
            },
            ExprKind::Struct(lit) => ExprKind::Struct(self.struct_lit(lit)?),
            ExprKind::Cop { init, crib } => ExprKind::Cop {
                init: Box::new(self.cop_init(*init)?),
                crib: Box::new(self.expr(*crib)?),
            },
            ExprKind::Unary(op, x) => ExprKind::Unary(op, Box::new(self.expr(*x)?)),
            ExprKind::Binary(op, l, r) => {
                ExprKind::Binary(op, Box::new(self.expr(*l)?), Box::new(self.expr(*r)?))
            }
            ExprKind::Cast(x, t) => ExprKind::Cast(Box::new(self.expr(*x)?), self.ty(t)?),
            ExprKind::Index { base, index } => ExprKind::Index {
                base: Box::new(self.expr(*base)?),
                index: Box::new(self.expr(*index)?),
            },
            ExprKind::Trust { tag, crib } => ExprKind::Trust {
                tag: Box::new(self.expr(*tag)?),
                crib: Box::new(self.expr(*crib)?),
            },
            ExprKind::Array(xs) => ExprKind::Array(self.exprs(xs)?),
            // Literals — nothing to resolve.
            other @ (ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Str(_)
            | ExprKind::Byte(_)
            | ExprKind::Bool(_)
            | ExprKind::Ghosted) => other,
        };
        Ok(Expr { kind, span })
    }

    fn struct_lit(&mut self, lit: StructLit) -> Result<StructLit, CompileError> {
        let name = self.resolve_named(&lit.name, true)?;
        let generics = self.tys(lit.generics)?;
        let mut fields = Vec::with_capacity(lit.fields.len());
        for f in lit.fields {
            fields.push(FieldInit {
                name: f.name,
                value: self.expr(f.value)?,
                span: f.span,
            });
        }
        Ok(StructLit {
            name,
            generics,
            fields,
            span: lit.span,
        })
    }

    fn cop_init(&mut self, init: CopInit) -> Result<CopInit, CompileError> {
        Ok(match init {
            CopInit::Struct(lit) => CopInit::Struct(self.struct_lit(lit)?),
            CopInit::Variant { name, args } => CopInit::Variant {
                name: self.resolve_named(&name, false)?,
                args: self.args(args)?,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_graph_with;
    use std::io;
    use std::path::Path;

    fn resolved(entry: &str, files: &[(&str, &str)]) -> Result<ast::Program, CompileError> {
        let map: HashMap<std::path::PathBuf, String> = files
            .iter()
            .map(|(p, s)| (Path::new(p).to_path_buf(), s.to_string()))
            .collect();
        let mut reader = |p: &Path| {
            map.get(p)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))
        };
        let graph = load_graph_with(Path::new(entry), &mut reader)?;
        resolve(&graph)
    }

    /// The top-level declaration names of a merged program, in order.
    fn item_names(p: &ast::Program) -> Vec<String> {
        p.items
            .iter()
            .filter_map(|it| match it {
                Item::Func(f) => Some(f.name.clone()),
                Item::Const(c) => Some(c.name.clone()),
                Item::Drip(d) => Some(d.name.clone()),
                Item::Moods(m) => Some(m.name.clone()),
                _ => None,
            })
            .collect()
    }

    const GEO_MAIN: &[(&str, &str)] = &[
        (
            "main.bet",
            "pull \"geometry\"\nfinna main() {\n spill.it(geometry.area(3, 4))\n spill.it(geometry.PI)\n}\n",
        ),
        (
            "geometry.bet",
            "flex facts PI: int = 3\nflex finna area(w: int, h: int) -> int {\n bet w * h\n}\nfinna secret() -> int {\n bet 9\n}\n",
        ),
    ];

    #[test]
    fn root_names_stay_bare_deps_are_mangled() {
        let p = resolved("main.bet", GEO_MAIN).unwrap();
        let names = item_names(&p);
        // geometry (idx 0) is mangled; root main stays bare; deps come first (post-order).
        assert!(names.contains(&"main".to_string()));
        assert!(names.contains(&"geometry$0$area".to_string()));
        assert!(names.contains(&"geometry$0$PI".to_string()));
        // `main` is not renamed.
        assert!(!names.iter().any(|n| n.contains("$main")));
    }

    #[test]
    fn qualified_call_becomes_a_plain_call_to_the_mangled_fn() {
        let p = resolved("main.bet", GEO_MAIN).unwrap();
        let main = p
            .items
            .iter()
            .find_map(|it| match it {
                Item::Func(f) if f.name == "main" => Some(f),
                _ => None,
            })
            .unwrap();
        // First stmt: spill.it(geometry.area(3,4)) — the inner arg must now be a Call to the
        // mangled function, NOT a Method on a `geometry` receiver.
        let mut saw_mangled_call = false;
        for s in &main.body.stmts {
            if let StmtKind::Expr(Expr {
                kind: ExprKind::Method { args, .. },
                ..
            }) = &s.kind
                && let Some(Arg {
                    value:
                        Expr {
                            kind: ExprKind::Call { callee, .. },
                            ..
                        },
                    ..
                }) = args.first()
                && let ExprKind::Name { name, .. } = &callee.kind
                && name == "geometry$0$area"
            {
                saw_mangled_call = true;
            }
        }
        assert!(saw_mangled_call, "expected a Call to geometry$0$area");
    }

    #[test]
    fn hush_item_is_not_reachable() {
        let files = &[
            (
                "main.bet",
                "pull \"geometry\"\nfinna main() {\n spill.it(geometry.secret())\n}\n",
            ),
            ("geometry.bet", "finna secret() -> int {\n bet 9\n}\n"),
        ];
        let err = resolved("main.bet", files).unwrap_err();
        assert!(
            matches!(&err, CompileError::Load(m) if m.contains("hush")),
            "got {err:?}"
        );
    }

    #[test]
    fn unknown_export_is_an_error() {
        let files = &[
            (
                "main.bet",
                "pull \"geometry\"\nfinna main() {\n spill.it(geometry.nope())\n}\n",
            ),
            ("geometry.bet", "flex finna area() -> int {\n bet 1\n}\n"),
        ];
        let err = resolved("main.bet", files).unwrap_err();
        assert!(
            matches!(&err, CompileError::Load(m) if m.contains("no exported item")),
            "got {err:?}"
        );
    }

    #[test]
    fn builtin_receiver_is_left_untouched() {
        // `spill.it(...)` must remain a Method on `spill` (intrinsic dispatch), never a Call.
        let p = resolved(
            "main.bet",
            &[("main.bet", "finna main() {\n spill.it(1)\n}\n")],
        )
        .unwrap();
        let main = p
            .items
            .iter()
            .find_map(|it| match it {
                Item::Func(f) => Some(f),
                _ => None,
            })
            .unwrap();
        assert!(matches!(
            &main.body.stmts[0].kind,
            StmtKind::Expr(Expr {
                kind: ExprKind::Method { .. },
                ..
            })
        ));
    }
}
