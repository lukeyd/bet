//! An ergonomic, boring builder for constructing [`Func`]s programmatically — used
//! identically by the frontend and by hand-written tests.
//!
//! Types are interned on the [`Module`](crate::ir::Module) first; the builder records
//! locals, blocks, statements, and terminators over those [`TyId`]s. Creating a block
//! makes it the current block; [`FuncBuilder::at`] re-selects one.

use crate::ir::*;

struct PartialBlock {
    stmts: Vec<Stmt>,
    term: Option<Terminator>,
}

/// Builds one [`Func`]. Construct with [`FuncBuilder::new`], populate, then
/// [`finish`](FuncBuilder::finish).
pub struct FuncBuilder {
    name: String,
    params: Vec<TyId>,
    rets: Vec<TyId>,
    locals: Vec<Local>,
    blocks: Vec<PartialBlock>,
    current: Option<BlockId>,
}

impl FuncBuilder {
    /// Start a function. The parameters become the first locals, in order.
    pub fn new(name: impl Into<String>, params: Vec<TyId>, rets: Vec<TyId>) -> FuncBuilder {
        let locals = params
            .iter()
            .map(|&ty| Local {
                ty,
                name: None,
                kind: LocalKind::Param,
            })
            .collect();
        FuncBuilder {
            name: name.into(),
            params,
            rets,
            locals,
            blocks: Vec::new(),
            current: None,
        }
    }

    /// The id of the `i`-th parameter local.
    pub fn param(&self, i: usize) -> LocalId {
        assert!(i < self.params.len(), "parameter index out of range");
        LocalId(i as u32)
    }

    /// Declare a fresh temporary local.
    pub fn local(&mut self, ty: TyId) -> LocalId {
        self.local_impl(ty, None)
    }

    /// Declare a fresh, named temporary local (the name is cosmetic — for `.mir` output).
    pub fn local_named(&mut self, ty: TyId, name: impl Into<String>) -> LocalId {
        self.local_impl(ty, Some(name.into()))
    }

    fn local_impl(&mut self, ty: TyId, name: Option<String>) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(Local {
            ty,
            name,
            kind: LocalKind::Temp,
        });
        id
    }

    /// Create a new, empty block and make it the current block.
    pub fn block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(PartialBlock {
            stmts: Vec::new(),
            term: None,
        });
        self.current = Some(id);
        id
    }

    /// Select an existing block as the current block.
    pub fn at(&mut self, bb: BlockId) {
        assert!(bb.index() < self.blocks.len(), "no such block");
        self.current = Some(bb);
    }

    fn cur(&mut self) -> &mut PartialBlock {
        let bb = self
            .current
            .expect("no current block (call `block()` first)");
        &mut self.blocks[bb.index()]
    }

    // --- statements ---

    /// Append `place = rvalue` to the current block.
    pub fn assign(&mut self, place: Place, rvalue: Rvalue) {
        self.cur().stmts.push(Stmt::Assign(place, rvalue));
    }

    /// Append `evict crib` to the current block.
    pub fn evict(&mut self, crib: Operand) {
        self.cur().stmts.push(Stmt::Evict(crib));
    }

    /// Append `evict tag in crib` (single-slot free) to the current block.
    pub fn evict_slot(&mut self, crib: Operand, tag: Operand) {
        self.cur().stmts.push(Stmt::EvictSlot { crib, tag });
    }

    /// Append an explicit no-op.
    pub fn nop(&mut self) {
        self.cur().stmts.push(Stmt::Nop);
    }

    // --- terminators ---

    fn terminate(&mut self, term: Terminator) {
        let slot = &mut self.cur().term;
        assert!(slot.is_none(), "block already has a terminator");
        *slot = Some(term);
    }

    pub fn goto(&mut self, bb: BlockId) {
        self.terminate(Terminator::Goto(bb));
    }

    pub fn branch(&mut self, cond: Operand, then_bb: BlockId, else_bb: BlockId) {
        self.terminate(Terminator::Branch {
            cond,
            then_bb,
            else_bb,
        });
    }

    pub fn switch(&mut self, scrutinee: Operand, cases: Vec<(u64, BlockId)>, default: BlockId) {
        self.terminate(Terminator::Switch {
            scrutinee,
            cases,
            default,
        });
    }

    pub fn holla_check(
        &mut self,
        tag: Operand,
        crib: Operand,
        resolved: Place,
        live: BlockId,
        ghosted: BlockId,
    ) {
        self.terminate(Terminator::HollaCheck {
            tag,
            crib,
            resolved,
            live,
            ghosted,
        });
    }

    pub fn ret(&mut self, values: Vec<Operand>) {
        self.terminate(Terminator::Return(values));
    }

    pub fn panic(&mut self, msg: Operand) {
        self.terminate(Terminator::Panic(msg));
    }

    pub fn unreachable(&mut self) {
        self.terminate(Terminator::Unreachable);
    }

    // --- place / operand helpers ---

    /// A bare local place.
    pub fn place(&self, local: LocalId) -> Place {
        Place::local(local)
    }

    /// Extend a place with a `.field(i)` projection.
    pub fn field(&self, base: &Place, i: u32) -> Place {
        self.proj(base, Proj::Field(i))
    }

    /// Extend a place with an `[index]` projection.
    pub fn index(&self, base: &Place, i: Operand) -> Place {
        self.proj(base, Proj::Index(i))
    }

    /// Extend a place with a dereference.
    pub fn deref(&self, base: &Place) -> Place {
        self.proj(base, Proj::Deref)
    }

    /// Extend a place with a sum-variant downcast (precedes payload `field`s).
    pub fn downcast(&self, base: &Place, variant: u32) -> Place {
        self.proj(base, Proj::Downcast(variant))
    }

    fn proj(&self, base: &Place, p: Proj) -> Place {
        let mut place = base.clone();
        place.proj.push(p);
        place
    }

    /// `copy place`.
    pub fn copy(&self, place: Place) -> Operand {
        Operand::Copy(place)
    }

    /// `move place`.
    pub fn mv(&self, place: Place) -> Operand {
        Operand::Move(place)
    }

    /// Finish building, producing the [`Func`]. Panics if any block was left without a
    /// terminator or if no blocks were created; the entry is the first block.
    pub fn finish(self) -> Func {
        assert!(!self.blocks.is_empty(), "function has no blocks");
        let blocks = self
            .blocks
            .into_iter()
            .enumerate()
            .map(|(i, b)| Block {
                id: BlockId(i as u32),
                stmts: b.stmts,
                term: b
                    .term
                    .unwrap_or_else(|| panic!("block bb{i} has no terminator")),
            })
            .collect();
        Func {
            name: self.name,
            params: self.params,
            rets: self.rets,
            locals: self.locals,
            blocks,
            entry: BlockId(0),
        }
    }
}
