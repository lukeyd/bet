//! [`Value`] — the runtime value domain of the interpreter, plus the corpus display rules.
//!
//! Values are plain, `Clone`-able data. Because bindings and assignments clone on the way in
//! and out of the environment, `bet`'s value-copy semantics for `drip` structs (corpus
//! `05-structs/drip-basics`) fall out for free — no aliasing, no reference counting.
//!
//! Memory-model values now have first-class representations: [`Value::Tag`] is a generational
//! handle into a typed `crib`, and [`Value::Crib`] a handle to an in-process arena. They are
//! backed by a simple generational slab inside the interpreter (see `interp::Arena`); an
//! `rt-abi`-backed slab is a later refinement, which is why the crate already depends on `rt-abi`.
//! `yikes` errors are [`Value::Yikes`], the value half of `(value, yikes)` returns.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

/// A runtime value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A signed 64-bit integer — the `int`/`i64` default and (for this slice) every sized int.
    Int(i64),
    /// A 64-bit float (`f64`/`float`).
    Float(f64),
    /// A single byte literal.
    Byte(u8),
    /// A boolean; prints as the slang literals `nocap`/`cap`.
    Bool(bool),
    /// A UTF-8 string.
    Str(String),
    /// `ghosted` — the nil / no-error value.
    Ghosted,
    /// An array (`[e1, e2, ...]`); the element type is uniform in well-typed programs.
    Array(Vec<Value>),
    /// A `drip` struct instance: its type name and its fields (keyed for O(log n) access).
    Struct {
        ty: String,
        fields: BTreeMap<String, Value>,
    },
    /// A `moods` sum-type value: the enum name, the active variant, and its payload.
    Variant {
        moods: String,
        name: String,
        payload: Vec<Value>,
    },
    /// A first-class reference to a top-level `finna` function, callable as a value.
    Fn(String),
    /// A `yikes` error value carrying its message. `ghosted` is the no-error value; a
    /// non-`ghosted` `yikes` is a live error (hence the `!= ghosted` idiom). `.tea(ctx)`
    /// wraps one by prefixing context.
    Yikes(String),
    /// A generational handle into a typed `crib` — what `cop` into a typed slab hands back.
    /// `arena` names the crib, `slot` the physical index, and `gen` the generation stamped at
    /// allocation; a `holla` check compares `gen` against the slot's current generation, so a
    /// stale tag (its slot evicted and its generation bumped) takes the `ghosted` arm.
    Tag {
        arena: usize,
        slot: usize,
        generation: u64,
    },
    /// A handle to an in-process arena (`crib`): the target of `cop`/`holla`/`trust`/`evict`.
    Crib(usize),
    /// A `stash` hash map. Reference-counted so a map mutated through a method receiver (`.put`)
    /// or passed to a function is shared, not value-copied — matching the compiled path's opaque
    /// handle. Entries are `(key, value)` pairs (linear probing; the corpus keeps them small).
    Stash(Rc<RefCell<Vec<(Value, Value)>>>),
}

/// The default display of a value (corpus "Display rules"): the text `spill.it` prints before
/// its trailing newline, and the text each `{}` in `spill.f` expands to.
pub fn display(v: &Value) -> String {
    match v {
        Value::Int(i) => i.to_string(),
        Value::Byte(b) => b.to_string(),
        Value::Float(x) => display_float(*x),
        Value::Bool(true) => "nocap".to_string(),
        Value::Bool(false) => "cap".to_string(),
        Value::Str(s) => s.clone(),
        Value::Ghosted => "ghosted".to_string(),
        Value::Array(xs) => {
            let inner = xs.iter().map(display).collect::<Vec<_>>().join(", ");
            format!("[{inner}]")
        }
        Value::Struct { ty, fields } => {
            let inner = fields
                .iter()
                .map(|(k, val)| format!("{k}: {}", display(val)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{ty}{{{inner}}}")
        }
        Value::Variant { name, payload, .. } => {
            if payload.is_empty() {
                name.clone()
            } else {
                let inner = payload.iter().map(display).collect::<Vec<_>>().join(", ");
                format!("{name}({inner})")
            }
        }
        Value::Fn(name) => format!("<finna {name}>"),
        // An error value prints as just its message (corpus `07-errors`: `spill.it(y)`).
        Value::Yikes(m) => m.clone(),
        // Tags and crib handles are never `spill`'d in a corpus program; these are diagnostic.
        Value::Tag {
            slot, generation, ..
        } => format!("<tag #{slot}@{generation}>"),
        Value::Crib(id) => format!("<crib #{id}>"),
        Value::Stash(m) => format!("<stash x{}>", m.borrow().len()),
    }
}

/// Floats are never printed bare in a corpus program (round-trip formatting is unsettled), so
/// this exists only for diagnostics / non-golden callers. Integral values render without a
/// trailing `.0`, matching the usual expectation for whole numbers.
fn display_float(x: f64) -> String {
    if x.is_finite() && x.fract() == 0.0 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

impl Value {
    /// The value's type name, for error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Byte(_) => "byte",
            Value::Bool(_) => "bool",
            Value::Str(_) => "str",
            Value::Ghosted => "ghosted",
            Value::Array(_) => "array",
            Value::Struct { .. } => "drip",
            Value::Variant { .. } => "moods",
            Value::Fn(_) => "finna",
            Value::Yikes(_) => "yikes",
            Value::Tag { .. } => "tag",
            Value::Crib(_) => "crib",
            Value::Stash(_) => "stash",
        }
    }
}
