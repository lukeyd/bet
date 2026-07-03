//! [`Value`] — the runtime value domain of the interpreter, plus the corpus display rules.
//!
//! Values are plain, `Clone`-able data. Because bindings and assignments clone on the way in
//! and out of the environment, `bet`'s value-copy semantics for `drip` structs (corpus
//! `05-structs/drip-basics`) fall out for free — no aliasing, no reference counting.
//!
//! Memory-model values (`tag` handles into cribs, `holla`-checked references) are deliberately
//! absent from this slice: when they land they'll be a `Value::Tag(rt_abi::Tag)` variant backed
//! by a generational slab, which is why the crate already depends on `rt-abi`.

use std::collections::BTreeMap;

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
        }
    }
}
