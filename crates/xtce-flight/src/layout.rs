//! From an `xtce-codegen` plan to something an encoder can be written from.
//!
//! The plan already answers where every field sits and how its bits become a value. Three
//! questions it does not answer are the encoder's:
//!
//! * **What Rust type does the caller supply?** A decoder can hand everything back as `u64`
//!   or `f64` and let the caller narrow. An encoder cannot: the type in the struct is the
//!   API, and a 12-bit field taking a `u64` invites a value that does not fit.
//! * **Which fields are the caller's to set?** A container is chosen by its restriction
//!   criteria. Those bits are not free — they are what makes the packet recognisable — so
//!   they are written as constants and do not appear in the struct at all.
//! * **What cannot be encoded?** Refused by name here, never silently dropped.

use std::collections::HashMap;
use std::fmt::Write as _;

use xtce_codegen::plan::{TextCharset, TextDelimiter};
use xtce_codegen::{ContainerPlan, Field, Guard, Node, Plan, Repr};
use xtce_model::CompareOp;
use xtce_model::types::IntegerCoding;

use crate::FlightError;

/// Everything the emitter needs.
#[derive(Clone, Debug)]
pub struct Layout {
    /// One entry per concrete container, in definition order.
    pub containers: Vec<Container>,
    /// Enumerations lifted out of the fields that use them, deduplicated.
    pub enums: Vec<EnumType>,
    /// Name of the root container, for the generated file's header.
    pub root_name: String,
}

/// One concrete container: a struct, an `encode` and a `decode`.
#[derive(Clone, Debug)]
pub struct Container {
    /// Name as written in the definition.
    pub xtce_name: String,
    /// Name of the generated struct.
    pub type_ident: String,
    /// Total encoded size. Always a whole number of bytes; a container that is not one is
    /// rounded up, because a packet is bytes.
    pub len_bytes: usize,
    /// Fields the caller supplies.
    pub fields: Vec<FlightField>,
    /// Bits `encode` writes itself, so that the packet matches this container's criteria.
    pub constants: Vec<Constant>,
    /// Whether any field borrows from the buffer, which decides if the struct has a lifetime.
    pub borrows: bool,
}

/// A field the caller sets.
#[derive(Clone, Debug)]
pub struct FlightField {
    /// Name as written in the definition, for diagnostics.
    pub xtce_name: String,
    /// Name of the generated struct field.
    pub ident: String,
    /// Bit offset from the start of the packet.
    pub bit_offset: usize,
    /// Width in bits.
    pub bit_width: u32,
    /// What the caller supplies and how it becomes bits.
    pub kind: Kind,
}

/// A restriction criterion, as bits the encoder writes.
#[derive(Clone, Debug)]
pub struct Constant {
    /// The parameter the criterion tests, for the generated comment.
    pub xtce_name: String,
    /// Where its bits sit.
    pub bit_offset: usize,
    /// How wide they are.
    pub bit_width: u32,
    /// The raw value to write, already masked to the width.
    pub raw: u64,
}

/// What the caller supplies for a field, and therefore how it is written.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// An unsigned integer, in the narrowest Rust type that holds the width.
    Unsigned,
    /// A signed integer, in one of XTCE's three codings.
    Signed(IntegerCoding),
    /// IEEE-754 binary16, exposed as `f32`.
    ///
    /// `f32` and not `f64` because binary16 has fewer bits than either and `f32` is what a
    /// flight computer with an FPU is likely to be holding. Encoding rounds to nearest, ties
    /// to even; a value that is not representable comes back rounded, not rejected.
    Float16,
    /// IEEE-754 binary32.
    Float32,
    /// IEEE-754 binary64.
    Float64,
    /// A single bit, exposed as `bool`.
    Bool,
    /// An enumeration, exposed as a generated Rust enum. Holds its index in [`Layout::enums`].
    Enumerated(usize),
    /// Text, exposed as `&str`.
    Text {
        /// Which validation the decoder applies.
        charset: TextCharset,
        /// How the string is delimited inside its fixed buffer.
        delimiter: TextDelimiter,
    },
    /// Raw bytes, exposed as `&[u8]`.
    Binary,
}

impl Kind {
    /// Whether a value of this kind borrows from the caller's buffer.
    pub(crate) fn borrows(&self) -> bool {
        matches!(self, Self::Text { .. } | Self::Binary)
    }
}

/// One generated Rust enum.
#[derive(Clone, Debug)]
pub struct EnumType {
    /// Name of the generated type.
    pub type_ident: String,
    /// `(variant identifier, raw value, XTCE label)`, in definition order.
    pub variants: Vec<(String, u64, String)>,
}

/// The Rust integer width that holds `bits`, for an unsigned or signed field.
pub(crate) const fn natural_bits(bits: u32) -> u32 {
    match bits {
        0..=8 => 8,
        9..=16 => 16,
        17..=32 => 32,
        _ => 64,
    }
}

/// Turns a plan into a layout, or names what stopped it.
///
/// # Errors
///
/// [`FlightError::Unsupported`] names the element that cannot be encoded and the container
/// it appeared in, and [`FlightError::NothingToEncode`] means the root has no concrete
/// container under it.
pub fn build(plan: &Plan) -> Result<Layout, FlightError> {
    let mut builder = Builder {
        enums: Vec::new(),
        by_variants: HashMap::new(),
        taken: HashMap::new(),
    };

    let mut containers = Vec::new();
    builder.walk(&plan.root, &mut Vec::new(), plan, &mut containers)?;

    if containers.is_empty() {
        return Err(FlightError::NothingToEncode {
            root: plan.root_name.clone(),
        });
    }

    Ok(Layout {
        containers,
        enums: builder.enums,
        root_name: plan.root_name.clone(),
    })
}

struct Builder {
    enums: Vec<EnumType>,
    /// Enumerations already lifted, keyed by their variant list, so two parameters sharing an
    /// XTCE type share one Rust type rather than generating two identical ones.
    by_variants: HashMap<String, usize>,
    /// Type names already handed out, so a sanitised name cannot collide with another.
    taken: HashMap<String, u32>,
}

impl Builder {
    /// Walks the inheritance tree, carrying the criteria that select each branch.
    fn walk(
        &mut self,
        node: &Node,
        guards: &mut Vec<Guard>,
        plan: &Plan,
        out: &mut Vec<Container>,
    ) -> Result<(), FlightError> {
        if let Some(index) = node.plan {
            let container = plan
                .containers
                .get(index)
                .ok_or(FlightError::DanglingIndex)?;
            out.push(self.container(container, guards)?);
        }

        for (criteria, child) in &node.children {
            let depth = guards.len();
            guards.extend(criteria.iter().cloned());
            self.walk(child, guards, plan, out)?;
            guards.truncate(depth);
        }
        Ok(())
    }

    fn container(
        &mut self,
        plan: &ContainerPlan,
        guards: &[Guard],
    ) -> Result<Container, FlightError> {
        let refuse = |element: &str, reason: &'static str| FlightError::Unsupported {
            element: element.to_owned(),
            container: plan.xtce_name.clone(),
            reason,
        };

        let Some(bit_length) = plan.bit_length else {
            return Err(refuse(
                "SizeInBits",
                "a container whose length depends on packet content cannot be encoded from a \
                 fixed-size struct; only containers laid out entirely at generation time are",
            ));
        };

        let constants = guards
            .iter()
            .map(|guard| Self::constant(guard, &plan.xtce_name))
            .collect::<Result<Vec<_>, _>>()?;

        let mut fields = Vec::new();
        for field in &plan.fields {
            // A criterion and the field it tests are the same bits. Writing both would mean
            // two sources of truth for one range, so the criterion wins and the field is not
            // the caller's to set.
            if constants
                .iter()
                .any(|constant| Self::same_span(constant, field))
            {
                continue;
            }
            fields.push(self.field(field, &plan.xtce_name)?);
        }

        // Two entries may name the same parameter — a container that repeats one, or an
        // inherited entry re-listed. The decoder collapses them; a struct cannot have the
        // field twice, so the last one wins, matching what a reader of the packet sees.
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut deduplicated: Vec<FlightField> = Vec::new();
        for field in fields {
            if let Some(&at) = seen.get(&field.ident) {
                deduplicated[at] = field;
            } else {
                seen.insert(field.ident.clone(), deduplicated.len());
                deduplicated.push(field);
            }
        }

        Ok(Container {
            type_ident: unique(&mut self.taken, type_ident(&plan.xtce_name)),
            xtce_name: plan.xtce_name.clone(),
            len_bytes: bit_length.div_ceil(8),
            borrows: deduplicated.iter().any(|field| field.kind.borrows()),
            fields: deduplicated,
            constants,
        })
    }

    /// Whether a criterion covers exactly the bits of a field.
    fn same_span(constant: &Constant, field: &Field) -> bool {
        field.static_span() == Some((constant.bit_offset, constant.bit_width))
    }

    fn constant(guard: &Guard, container: &str) -> Result<Constant, FlightError> {
        if guard.operator != CompareOp::Equal {
            return Err(FlightError::Unsupported {
                element: "Comparison".to_owned(),
                container: container.to_owned(),
                reason: "only an equality criterion has one value an encoder could write; \
                         an inequality names a set",
            });
        }
        if guard.bit_width > 64 {
            return Err(FlightError::Unsupported {
                element: "Comparison".to_owned(),
                container: container.to_owned(),
                reason: "a criterion wider than 64 bits cannot be written as one literal",
            });
        }

        // The criterion's literal was read as a signed integer because the parameter it tests
        // may be signed. What goes into the packet is the raw bit pattern, so it is masked to
        // the field's width — the same truncation the comparison itself performs.
        let mask = mask_for(guard.bit_width);
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let raw = (guard.value as i64 as u64) & mask;

        Ok(Constant {
            xtce_name: guard.xtce_name.clone(),
            bit_offset: guard.bit_offset,
            bit_width: guard.bit_width,
            raw,
        })
    }

    fn field(&mut self, field: &Field, container: &str) -> Result<FlightField, FlightError> {
        let refuse = |reason: &'static str| FlightError::Unsupported {
            element: "ParameterRefEntry".to_owned(),
            container: container.to_owned(),
            reason,
        };

        let Some((bit_offset, bit_width)) = field.static_span() else {
            return Err(refuse(
                "a field whose width comes from the packet has no fixed place in a struct",
            ));
        };

        // A number's width picks its Rust type, and the plan already bounds that. Anything
        // outside the range would silently pick `u64` and encode the wrong number of bits.
        if !field.repr.borrows() && !(1..=64).contains(&bit_width) {
            return Err(refuse(
                "only fields of 1 to 64 bits become a Rust integer or float",
            ));
        }

        let kind = match &field.repr {
            Repr::Unsigned => Kind::Unsigned,
            Repr::Signed(coding) => Kind::Signed(*coding),
            // A float's width is its format. There is no 24-bit IEEE-754, so a definition
            // asking for one is a definition this generator would have to invent an
            // encoding for.
            Repr::Float16 if bit_width == 16 => Kind::Float16,
            Repr::Float32 if bit_width == 32 => Kind::Float32,
            Repr::Float64 if bit_width == 64 => Kind::Float64,
            Repr::Float16 | Repr::Float32 | Repr::Float64 => {
                return Err(refuse(
                    "a float must be 16, 32 or 64 bits wide; no other IEEE-754 format exists",
                ));
            }
            Repr::Bool => Kind::Bool,
            Repr::Enumerated(variants) => {
                Kind::Enumerated(self.enumeration(variants, &field.ident, bit_width, container)?)
            }
            Repr::Text { charset, delimiter } => {
                Self::check_text(delimiter, bit_offset, bit_width, container)?;
                Kind::Text {
                    charset: *charset,
                    delimiter: delimiter.clone(),
                }
            }
            Repr::Binary => {
                if bit_offset % 8 != 0 || bit_width % 8 != 0 {
                    return Err(refuse(
                        "binary is written as whole bytes, so it has to start on a byte and \
                         occupy a whole number of them",
                    ));
                }
                Kind::Binary
            }
        };

        Ok(FlightField {
            xtce_name: field.xtce_name.clone(),
            ident: field.ident.clone(),
            bit_offset,
            bit_width,
            kind,
        })
    }

    /// What a text field has to look like for an encoder to fill it.
    fn check_text(
        delimiter: &TextDelimiter,
        bit_offset: usize,
        bit_width: u32,
        container: &str,
    ) -> Result<(), FlightError> {
        let refuse = |reason: &'static str| FlightError::Unsupported {
            element: "StringDataEncoding".to_owned(),
            container: container.to_owned(),
            reason,
        };

        if bit_offset % 8 != 0 || bit_width % 8 != 0 {
            return Err(refuse(
                "text is written as whole bytes, so it has to start on a byte and occupy a \
                 whole number of them",
            ));
        }

        match delimiter {
            TextDelimiter::WholeBuffer => {}
            TextDelimiter::TerminationChar(terminator) => {
                if terminator.is_empty() {
                    return Err(refuse(
                        "an empty termination character terminates every string at its start",
                    ));
                }
                if terminator.len() * 8 > bit_width as usize {
                    return Err(refuse("the terminator does not fit inside the field"));
                }
            }
            TextDelimiter::LeadingSize { size_in_bits } => {
                // The prefix is followed immediately by the text. A prefix that is not a
                // whole number of bytes would put the text off a byte boundary, and writing
                // a string bit-shifted is a different piece of code than this one.
                if size_in_bits % 8 != 0 {
                    return Err(refuse(
                        "a length prefix that is not a whole number of bytes leaves the text \
                         unaligned",
                    ));
                }
                if *size_in_bits == 0 || *size_in_bits > 32 {
                    return Err(refuse("a length prefix must be 8, 16, 24 or 32 bits"));
                }
                if *size_in_bits as usize > bit_width as usize {
                    return Err(refuse("the length prefix does not fit inside the field"));
                }
            }
        }
        Ok(())
    }

    /// Lifts an enumeration into a named Rust type, reusing one if the variants match.
    fn enumeration(
        &mut self,
        variants: &[(i128, i128, String)],
        field_ident: &str,
        bit_width: u32,
        container: &str,
    ) -> Result<usize, FlightError> {
        if variants.is_empty() {
            return Err(FlightError::Unsupported {
                element: "EnumerationList".to_owned(),
                container: container.to_owned(),
                reason: "an enumeration with no labels has no value an encoder could write",
            });
        }

        // The key has to distinguish enumerations that differ only in a label's value, so it
        // is built from both halves of every entry rather than from the labels alone.
        let mut key = String::new();
        for (value, _, label) in variants {
            let _ = write!(key, "{value}={label};");
        }
        if let Some(&index) = self.by_variants.get(&key) {
            return Ok(index);
        }

        let mask = mask_for(bit_width);
        let mut taken_variants: HashMap<String, u32> = HashMap::new();
        let mut built = Vec::with_capacity(variants.len());
        for (value, _, label) in variants {
            // A label is free text: `LOW-POWER`, `9600`, `in transit`. What goes into the
            // generated enum has to be an identifier, and two labels must not sanitise to
            // the same one.
            // A label with nothing identifier-like in it is named after its value, which
            // is at least something a reader can find in the definition.
            let base = if label.chars().any(char::is_alphanumeric) {
                type_ident(label)
            } else {
                format!("Value{value}")
            };
            let ident = unique(&mut taken_variants, base);
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
            let raw = (*value as i64 as u64) & mask;
            built.push((ident, raw, label.clone()));
        }

        let type_ident = unique(&mut self.taken, type_ident(field_ident));
        let index = self.enums.len();
        self.enums.push(EnumType {
            type_ident,
            variants: built,
        });
        self.by_variants.insert(key, index);
        Ok(index)
    }
}

/// An XTCE name as a Rust type or variant identifier.
///
/// Not the one `xtce-codegen` uses. That one upper-camel-cases everything, which turns
/// `StatusReport` into `Statusreport` — fine for a name nobody types, wrong for the public
/// API of a library. Here a segment that is already mixed case is left alone, and only an
/// all-capitals one is folded, so `StatusReport` survives and `JPSS_ATT_EPHEM` still becomes
/// `JpssAttEphem` rather than a shout.
pub fn type_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for segment in name.split(|character: char| !character.is_alphanumeric()) {
        if segment.is_empty() {
            continue;
        }
        if segment.chars().any(char::is_lowercase) {
            let mut characters = segment.chars();
            if let Some(first) = characters.next() {
                out.extend(first.to_uppercase());
                out.push_str(characters.as_str());
            }
        } else {
            let mut characters = segment.chars();
            if let Some(first) = characters.next() {
                out.extend(first.to_uppercase());
                out.extend(characters.flat_map(char::to_lowercase));
            }
        }
    }

    if out.is_empty() {
        return "Value".to_owned();
    }
    // An identifier cannot start with a digit, and enumeration labels are often bare
    // numbers: `9600`, `115200`.
    if out.starts_with(|character: char| character.is_ascii_digit()) {
        out.insert(0, 'C');
    }
    out
}

/// `base`, or `base2`, `base3`… if it has been handed out already.
fn unique(taken: &mut HashMap<String, u32>, base: String) -> String {
    let count = taken.entry(base.clone()).or_insert(0);
    *count += 1;
    if *count == 1 {
        base
    } else {
        format!("{base}{count}")
    }
}

/// The low `width` bits set.
pub(crate) const fn mask_for(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}
