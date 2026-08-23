//! Compile an XTCE definition into a `no_std` Rust encoder and decoder for on-board use.
//!
//! # What this is for
//!
//! Ground software reads packets. Flight software *writes* them, on a part with no heap, no
//! operating system, and a hard rule against a code path that can panic. Public XTCE tooling
//! is almost entirely ground-side: the generators that produce on-board code are written
//! inside the companies that fly, and stay there.
//!
//! This generator emits the other half. Given the same XTCE file the ground uses, it produces
//! a `struct` per container with `encode` and `decode`, in code that:
//!
//! * compiles under `#![no_std]` — no `alloc`, no `String`, no `Vec`;
//! * contains no `unsafe`;
//! * has no panicking branch, which is checked in CI against the emitted LLVM IR rather than
//!   asserted in a README;
//! * refuses to write a value that does not fit its field, instead of truncating it.
//!
//! # How it relates to `xtce-rs`
//!
//! It is one new back end, not a new tool chain. Parsing, the intermediate representation,
//! container flattening, restriction criteria and the refusal rules all come from
//! [`xtce_codegen::plan`], which [`xtce-rs`](https://github.com/RustSpaceLab/xtce-rs) already
//! validates against the Python reference implementation on roughly 17 000 real packets.
//! What is new here is the emitter and the direction: values in, bits out.
//!
//! That layering is also how correctness is argued. `xtce-flight` encodes; `xtce-decode` —
//! independent code, already proven equal to the reference — decodes; the values are compared
//! to what went in. See `crates/xtce-flight-e2e`.
//!
//! # Scope
//!
//! Only containers laid out entirely at generation time. A field whose width comes from the
//! packet has no fixed place in a `struct`, and is refused by name through
//! [`FlightError::Unsupported`] rather than quietly skipped.
//!
//! # Example
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let db = xtce_model::XtceDb::from_path("definition.xml")?;
//! let source = xtce_flight::generate(&db, &xtce_flight::Options::default())?;
//! std::fs::write("telemetry.rs", source)?;
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![deny(clippy::todo, clippy::unimplemented)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]
// Bit offsets and widths move between `usize` and `u32` throughout. Every one of them is a
// position inside a packet that the plan has already bounded to 64 bits.
#![allow(clippy::cast_possible_truncation)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod emit;
pub mod harness;
pub mod layout;

use xtce_model::XtceDb;

pub use layout::{
    Constant, Container, ContextCriterion, ContextTest, EnumType, FixedValue, FlightContext,
    FlightField, Kind, Layout,
};

/// What to generate.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Container to start from. Defaults to the database's own default root.
    pub root: Option<String>,
    /// Text recorded in the generated file's header, usually the source path.
    pub source_label: Option<String>,
}

/// Why a definition could not be compiled into flight code.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FlightError {
    /// No container to start from.
    #[error("no container named {name:?}")]
    NoSuchContainer {
        /// The name that was looked up.
        name: String,
    },

    /// The database offers no unambiguous root and none was named.
    #[error("no root container could be chosen automatically; name one explicitly")]
    AmbiguousRoot,

    /// The root exists but nothing under it is a concrete container.
    #[error("nothing under {root} is a container a packet can be encoded as")]
    NothingToEncode {
        /// The root that was walked.
        root: String,
    },

    /// A construct this generator does not emit flight code for.
    ///
    /// Fatal rather than a fallback. A generator whose output silently covers half a
    /// definition is worse than one that stops: the gap only shows up in flight.
    #[error("cannot compile <{element}> in {container}: {reason}")]
    Unsupported {
        /// The element that stopped compilation.
        element: String,
        /// Where it appeared.
        container: String,
        /// Why it cannot be compiled.
        reason: &'static str,
    },

    /// The plan could not be built.
    #[error("{0}")]
    Plan(#[from] xtce_codegen::CodegenError),

    /// The plan is internally inconsistent.
    #[error("internal: an index in the plan does not resolve")]
    DanglingIndex,
}

/// Compiles `db` into `no_std` Rust source.
///
/// # Errors
///
/// See [`FlightError`]. In particular [`FlightError::Unsupported`] names the element that
/// cannot be compiled and the container it appeared in.
pub fn generate(db: &XtceDb, options: &Options) -> Result<String, FlightError> {
    let layout = layout(db, options)?;
    let source = options
        .source_label
        .clone()
        .or_else(|| db.source().map(|path| path.display().to_string()))
        .unwrap_or_else(|| "<memory>".to_owned());
    Ok(emit::module(&layout, &source))
}

/// Analyses a definition without generating code, to report what would compile.
///
/// # Errors
///
/// As [`generate`].
pub fn layout(db: &XtceDb, options: &Options) -> Result<Layout, FlightError> {
    let plan_options = xtce_codegen::Options {
        root: options.root.clone(),
        source_label: options.source_label.clone(),
    };
    let plan = xtce_codegen::plan(db, &plan_options).map_err(|error| match error {
        xtce_codegen::CodegenError::NoSuchContainer { name } => {
            FlightError::NoSuchContainer { name }
        }
        xtce_codegen::CodegenError::AmbiguousRoot => FlightError::AmbiguousRoot,
        other => FlightError::Plan(other),
    })?;
    layout::build(&plan)
}
