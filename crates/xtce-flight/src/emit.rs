//! Turning a [`Layout`] into Rust source.
//!
//! Two rules shape everything here.
//!
//! **Every index is a literal.** `encode` and `decode` convert the caller's slice to a
//! reference to a fixed-size array once, at the top, and everything after that indexes the
//! array at offsets computed when this file was generated. A literal index into an array of
//! known length is provably in bounds, so no bounds check survives optimisation and no
//! panicking branch is emitted. That is the whole reason for the conversion; a slice would
//! leave a panic path behind every one of them.
//!
//! **A value that does not fit is an error, not a truncation.** A 12-bit field takes a `u16`,
//! and the range that does not fit in twelve bits is rejected. Masking silently would put a
//! wrong number in a packet that is otherwise perfectly well formed, which is the failure
//! mode hardest to see from the ground.

use std::collections::BTreeSet;

use proc_macro2::{Ident, Literal, Span, TokenStream};
use quote::{format_ident, quote};
use xtce_codegen::plan::{TextCharset, TextDelimiter};
use xtce_model::CompareOp;
use xtce_model::types::IntegerCoding;

use xtce_codegen::Calibration;

use crate::layout::{
    Container, ContextComparison, ContextCriterion, ContextTest, EnumType, FlightField, Kind,
    Layout, mask_for, natural_bits,
};

/// Renders the whole module.
pub fn module(layout: &Layout, source: &str) -> String {
    let header = header(layout, source);

    let enums = layout.enums.iter().map(enumeration);
    let containers = layout
        .containers
        .iter()
        .map(|item| container(item, &layout.enums));
    let dispatcher = dispatcher(layout);
    let helpers = helpers(layout);

    let tokens = quote! {
        #(#enums)*
        #(#containers)*
        #dispatcher
        #helpers
    };

    let body = match syn::parse2(tokens.clone()) {
        Ok(file) => prettyplease::unparse(&file),
        // Unparsed source is still valid Rust; only the formatting is lost. Failing the whole
        // generation because the pretty-printer choked would be the wrong trade.
        Err(_) => tokens.to_string(),
    };
    format!("{header}{body}")
}

fn header(layout: &Layout, source: &str) -> String {
    let mut text = format!(
        "// Flight encoder and decoder generated from `{source}` by `xtce-flight`, rooted at \
         `{}`.\n//\n\
         // {} container(s). Every bit offset, mask and length below was computed when this\n\
         // file was generated; nothing consults the XTCE definition at run time.\n//\n\
         // The code is `no_std`: it names nothing outside `core`, allocates nothing, and\n\
         // contains no `unsafe`. `encode` and `decode` have no panicking branch.\n//\n\
         // Do not edit: regenerate instead. Intended to be included inside a module that\n\
         // carries the lint allowances generated code needs, for example:\n//\n\
         //     #[allow(dead_code, clippy::all, clippy::pedantic)]\n\
         //     mod telemetry {{\n\
         //         include!(concat!(env!(\"OUT_DIR\"), \"/telemetry.rs\"));\n\
         //     }}\n\n",
        layout.root_name,
        layout.containers.len(),
    );
    for container in &layout.containers {
        let _ = std::fmt::Write::write_fmt(
            &mut text,
            format_args!(
                "// {:<28} {:>5} byte(s), {} field(s), {} constant(s)\n",
                container.xtce_name,
                container.len_bytes,
                container.fields.len(),
                container.constants.len(),
            ),
        );
    }
    text.push('\n');
    text
}

// ---------------------------------------------------------------------------------------
// Enumerations
// ---------------------------------------------------------------------------------------

fn enumeration(definition: &EnumType) -> TokenStream {
    let name = ident(&definition.type_ident);
    let variants = definition.variants.iter().map(|(variant, _, label)| {
        let variant = ident(variant);
        let doc = format!("XTCE label `{label}`.");
        quote! {
            #[doc = #doc]
            #variant
        }
    });

    let to_raw = definition.variants.iter().map(|(variant, raw, _)| {
        let variant = ident(variant);
        let raw = Literal::u64_unsuffixed(*raw);
        quote! { Self::#variant => #raw }
    });
    let from_raw = definition.variants.iter().map(|(variant, raw, _)| {
        let variant = ident(variant);
        let raw = Literal::u64_unsuffixed(*raw);
        quote! { #raw => Some(Self::#variant) }
    });
    let labels = definition.variants.iter().map(|(variant, _, label)| {
        let variant = ident(variant);
        quote! { Self::#variant => #label }
    });

    let doc = format!(
        "An enumerated parameter, with {} label(s) from the definition.",
        definition.variants.len()
    );

    quote! {
        #[doc = #doc]
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum #name {
            #(#variants,)*
        }

        impl #name {
            /// The value written into the packet.
            #[must_use]
            pub const fn raw(self) -> u64 {
                match self {
                    #(#to_raw,)*
                }
            }

            /// The variant a raw value selects, or `None` if the definition has no label
            /// for it.
            #[must_use]
            pub const fn from_raw(raw: u64) -> Option<Self> {
                match raw {
                    #(#from_raw,)*
                    _ => None,
                }
            }

            /// The label exactly as the definition spells it, which is not always a Rust
            /// identifier.
            #[must_use]
            pub const fn label(self) -> &'static str {
                match self {
                    #(#labels,)*
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Containers
// ---------------------------------------------------------------------------------------

fn container(container: &Container, enums: &[EnumType]) -> TokenStream {
    let name = ident(&container.type_ident);
    let len = Literal::usize_unsuffixed(container.len_bytes);

    let lifetime = if container.borrows {
        quote!(<'a>)
    } else {
        quote!()
    };
    let struct_fields = container.fields.iter().map(|field| {
        let ident = ident(&field.ident);
        let ty = field_type(field, enums);
        let doc = format!(
            "`{}`, {} bit(s) at bit {}.",
            field.xtce_name, field.bit_width, field.bit_offset
        );
        quote! {
            #[doc = #doc]
            pub #ident: #ty
        }
    });

    let fixed_doc = if container.fixed.is_empty() {
        String::new()
    } else {
        let listed = container
            .fixed
            .iter()
            .map(|fixed| match &fixed.xtce_name {
                Some(name) => format!("`{name}`"),
                None => format!("an unnamed value at bit {}", fixed.bit_offset),
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "\n\nIt also writes the values the definition fixes ({listed}). `decode` steps over \
             them without reading them: XTCE selects a container by its restriction criteria, \
             and a fixed value is packaging rather than a discriminator."
        )
    };
    let constant_doc = if container.constants.is_empty() {
        String::new()
    } else {
        let listed = container
            .constants
            .iter()
            .map(|constant| format!("`{}` = {}", constant.xtce_name, constant.raw))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "\n\nEncoding also writes the restriction criteria that identify this container \
             ({listed}); they are not fields because they are not the caller's to choose."
        )
    };
    let doc = format!(
        "`{}`, {} byte(s) on the wire.{constant_doc}{fixed_doc}",
        container.xtce_name, container.len_bytes
    );

    let encode = encode_body(container);
    let decode = decode_body(container, enums);
    let matches = matches_body(container);
    let accessors = container.fields.iter().filter_map(calibrated_accessor);

    quote! {
        #[doc = #doc]
        #[derive(Clone, Copy, PartialEq, Debug)]
        pub struct #name #lifetime {
            #(#struct_fields,)*
        }

        impl #lifetime #name #lifetime {
            /// Encoded size, in bytes.
            pub const LEN: usize = #len;

            #encode
            #decode
            #matches
            #(#accessors)*
        }
    }
}

/// The accessor that applies a field's calibrator, for the fields that have one.
///
/// Deliberately not a struct field. Encoding is unaffected — a spacecraft has an ADC count,
/// and turning counts into engineering units is the ground's job — so storing the result
/// would cost bytes of RAM per packet on the part least able to spare them, to hold a number
/// the flight side usually never looks at.
///
/// The arithmetic is `xtce-decode`'s, line for line, because that is what it is checked
/// against. Terms are summed in document order: floating-point addition is not associative,
/// so any other order is a different answer. An integral raw value is raised to its power
/// exactly in `i128` and converted once; a float raw goes through `powi`, which rounds at
/// every step. Those are different numbers, and the path follows the field's encoding.
fn calibrated_accessor(field: &FlightField) -> Option<TokenStream> {
    let calibration = field.calibration.as_ref()?;
    let name = format_ident!("{}_calibrated", field.ident.trim_end_matches('_'));
    let xtce_name = &field.xtce_name;

    // The default is the last arm; the contexts go in front of it in definition order, so
    // the first one whose criteria hold is the one that applies.
    let mut body = apply_calibration(field, calibration);
    if !field.contexts.is_empty() {
        // Built from the back so the chain renders as `else if`, rather than as a block per
        // context nested inside the one before it.
        body = quote! { { #body } };
        for context in field.contexts.iter().rev() {
            let condition = context_condition(&context.criteria);
            let applied = apply_calibration(field, &context.calibration);
            body = quote! { if #condition { #applied } else #body };
        }
    }

    let contexts_doc = if field.contexts.is_empty() {
        String::new()
    } else {
        let listed = field
            .contexts
            .iter()
            .map(|context| format!("`{}`", criteria_prose(&context.criteria)))
            .collect::<Vec<_>>()
            .join(", then ");
        format!(
            " The definition gives it {} context calibrator(s) as well as a default; they are \
             tried against this packet's own fields in order — {listed} — and the first whose \
             criteria hold is the one applied. Each is named by the parameter it reads, which \
             is not always the one the definition wrote: a criterion is compared against what \
             the container has decoded so far, so one naming a parameter it has not — the \
             field being calibrated, or one that comes after it — reads the field being \
             calibrated instead.",
            field.contexts.len()
        )
    };
    let doc = format!(
        " `{xtce_name}` in engineering units, by the calibrator the definition gives it.\n\n          The struct field holds the raw value the packet carries; this is what the ground \
         reads. Encoding is unaffected — the calibrator is not inverted, because a \
         spacecraft produces counts.{contexts_doc}"
    );

    Some(quote! {
        #[doc = #doc]
        ///
        /// # Errors
        ///
        /// [`DecodeError::Calibration`] if a spline is asked for a value outside its points
        /// and the definition does not allow extrapolation. A polynomial never fails.
        pub fn #name(&self) -> Result<f64, DecodeError> {
            #body
        }
    })
}

/// A context's criteria as prose, for the accessor's documentation.
///
/// Named by the parameter each comparison *reads*. Where the definition named one the
/// container had not decoded yet, that is already the field being calibrated: the plan
/// resolves the fallback, and the name it keeps is the parameter whose bits are compared.
fn criteria_prose(criterion: &ContextCriterion) -> String {
    match criterion {
        ContextCriterion::Test(test) => match &test.test {
            ContextComparison::Value { operator, value } => {
                let operator = match operator {
                    CompareOp::Equal => "==",
                    CompareOp::NotEqual => "!=",
                    CompareOp::Less => "<",
                    CompareOp::LessOrEqual => "<=",
                    CompareOp::Greater => ">",
                    CompareOp::GreaterOrEqual => ">=",
                };
                format!("{} {operator} {value}", test.xtce_name)
            }
            // A label comparison, already resolved. Written as the set it became, because
            // that is what the code does and the label it came from is in the definition.
            ContextComparison::Labels(ranges) => {
                let listed = ranges
                    .iter()
                    .map(|(low, high)| {
                        if low == high {
                            format!("{low}")
                        } else {
                            format!("{low}..={high}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" or ");
                if listed.is_empty() {
                    format!("{} matches no label", test.xtce_name)
                } else {
                    format!("{} is {listed}", test.xtce_name)
                }
            }
        },
        ContextCriterion::All(children) => {
            if children.is_empty() {
                return "always".to_owned();
            }
            children
                .iter()
                .map(criteria_prose)
                .collect::<Vec<_>>()
                .join(" and ")
        }
        ContextCriterion::Any(children) => {
            if children.is_empty() {
                return "never".to_owned();
            }
            format!(
                "({})",
                children
                    .iter()
                    .map(criteria_prose)
                    .collect::<Vec<_>>()
                    .join(" or ")
            )
        }
    }
}

/// One calibrator applied to a field's raw value, as an expression of type `Result<f64, _>`.
fn apply_calibration(field: &FlightField, calibration: &Calibration) -> TokenStream {
    let raw = {
        let ident = ident(&field.ident);
        quote!(self.#ident)
    };
    let xtce_name = &field.xtce_name;
    let integral = matches!(field.kind, Kind::Unsigned | Kind::Signed(_));

    match calibration {
        Calibration::Polynomial(terms) => {
            let accumulate = terms.iter().map(|term| {
                let coefficient = Literal::f64_unsuffixed(term.coefficient);
                let exponent = Literal::i32_unsuffixed(term.exponent);
                if integral {
                    quote! { sum += #coefficient * integer_power(base, #exponent); }
                } else {
                    quote! { sum += #coefficient * powi(base, #exponent); }
                }
            });
            let base = if integral {
                quote! { let base = i128::from(#raw); }
            } else {
                quote! { let base = f64::from(#raw); }
            };
            quote! {
                #base
                let mut sum = 0.0f64;
                #(#accumulate)*
                Ok(sum)
            }
        }
        Calibration::Spline(spline) => {
            let points = spline.points.iter().map(|point| {
                let raw = Literal::f64_unsuffixed(point.raw);
                let calibrated = Literal::f64_unsuffixed(point.calibrated);
                quote! { (#raw, #calibrated) }
            });
            let order = Literal::u8_unsuffixed(spline.order);
            let extrapolate = spline.extrapolate;
            let query = if integral {
                quote! { i128::from(#raw) as f64 }
            } else {
                quote! { f64::from(#raw) }
            };
            quote! {
                const POINTS: &[(f64, f64)] = &[#(#points),*];
                match spline_value(POINTS, #order, #extrapolate, #query) {
                    Some(value) => Ok(value),
                    None => Err(DecodeError::Calibration { parameter: #xtce_name }),
                }
            }
        }
    }
}

/// Whether a context calibrator's criteria hold, as an expression of type `bool`.
///
/// Each test reads a field of this same struct — the plan has already resolved which — so
/// nothing here touches the packet. That is deliberate: the buffer is gone by the time an
/// accessor runs, and the values it carried are exactly the struct's fields.
fn context_condition(criterion: &ContextCriterion) -> TokenStream {
    match criterion {
        ContextCriterion::Test(test) => context_test(test),
        // An empty conjunction is true and an empty disjunction is false, which is what the
        // interpreter computes and what `all([])` and `any([])` mean.
        ContextCriterion::All(children) => {
            if children.is_empty() {
                return quote!(true);
            }
            let parts = children.iter().map(context_condition);
            quote! { #(#parts)&&* }
        }
        ContextCriterion::Any(children) => {
            if children.is_empty() {
                return quote!(false);
            }
            let parts = children.iter().map(context_condition);
            quote! { (#(#parts)||*) }
        }
    }
}

/// One comparison, in `i128` so that the field's own width and signedness cannot change the
/// answer.
///
/// The plan reads a criterion's literal as an `i128` because the parameter it tests may be
/// signed, and a literal outside the field's range is not an error — it is a comparison with
/// one answer for every packet. Widening both sides gives that answer without a special case,
/// and the constant folds away.
fn context_test(test: &ContextTest) -> TokenStream {
    let field = ident(&test.ident);
    // `i128::from` accepts every type the field can have here — the layout resolves a test to
    // an integer field, a one-bit `bool` whose `From` gives the bit itself, or an enumeration,
    // whose raw value is one method call away.
    let value = if test.enumerated {
        quote!(i128::from(self.#field.raw()))
    } else {
        quote!(i128::from(self.#field))
    };

    match &test.test {
        ContextComparison::Value {
            operator,
            value: literal,
        } => {
            let literal = Literal::i128_unsuffixed(*literal);
            let operator = compare_op(*operator);
            quote! { #value #operator #literal }
        }
        // A label comparison, resolved to raw values when the plan was built. An empty set
        // holds for nothing the field can carry.
        ContextComparison::Labels(ranges) => {
            if ranges.is_empty() {
                return quote! { false };
            }
            let patterns = ranges.iter().map(|(low, high)| {
                let low = Literal::i128_unsuffixed(*low);
                if low.to_string() == Literal::i128_unsuffixed(*high).to_string() {
                    quote! { #low }
                } else {
                    let high = Literal::i128_unsuffixed(*high);
                    quote! { #low..=#high }
                }
            });
            quote! { matches!(#value, #(#patterns)|*) }
        }
    }
}

/// A comparison operator as Rust spells it.
fn compare_op(operator: CompareOp) -> TokenStream {
    match operator {
        CompareOp::Equal => quote!(==),
        CompareOp::NotEqual => quote!(!=),
        CompareOp::Less => quote!(<),
        CompareOp::LessOrEqual => quote!(<=),
        CompareOp::Greater => quote!(>),
        CompareOp::GreaterOrEqual => quote!(>=),
    }
}

/// The Rust type a field's value has in the struct.
fn field_type(field: &FlightField, enums: &[EnumType]) -> TokenStream {
    match &field.kind {
        Kind::Unsigned => unsigned_type(natural_bits(field.bit_width)),
        Kind::Signed(_) => signed_type(natural_bits(field.bit_width)),
        Kind::Float16 | Kind::Float32 => quote!(f32),
        Kind::Float64 => quote!(f64),
        Kind::Bool => quote!(bool),
        Kind::Enumerated(index) => enum_name(*index, enums),
        Kind::Text { .. } => quote!(&'a str),
        Kind::Binary => quote!(&'a [u8]),
    }
}

/// The generated enum's name, by index into the layout's list.
fn enum_name(index: usize, enums: &[EnumType]) -> TokenStream {
    // `layout` builds both sides of this index, so it always resolves. Emitting the unit
    // type rather than panicking keeps the generator total.
    let Some(definition) = enums.get(index) else {
        return quote!(());
    };
    let name = ident(&definition.type_ident);
    quote!(#name)
}

fn unsigned_type(bits: u32) -> TokenStream {
    match bits {
        8 => quote!(u8),
        16 => quote!(u16),
        32 => quote!(u32),
        _ => quote!(u64),
    }
}

fn signed_type(bits: u32) -> TokenStream {
    match bits {
        8 => quote!(i8),
        16 => quote!(i16),
        32 => quote!(i32),
        _ => quote!(i64),
    }
}

fn ident(name: &str) -> Ident {
    Ident::new(name, Span::call_site())
}

// ---------------------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------------------

fn encode_body(container: &Container) -> TokenStream {
    let len = Literal::usize_unsuffixed(container.len_bytes);
    let out = format_ident!("out");

    // Named after the parameter, because a comment cannot survive `quote!` and a reviewer
    // reading the generated encoder should still be able to see which criterion this is.
    let constants = container.constants.iter().map(|constant| {
        let binding = format_ident!(
            "criterion_{}",
            xtce_codegen::plan::field_ident(&constant.xtce_name)
        );
        let raw = Literal::u64_unsuffixed(constant.raw);
        let write = write_bits(
            constant.bit_offset,
            constant.bit_width,
            &quote!(#binding),
            &out,
        );
        quote! {
            {
                let #binding: u64 = #raw;
                #write
            }
        }
    });

    // Bits the definition fixed rather than the criteria: a sync marker, a spare nibble, a
    // trailer. Written the same way and for the same reason — the caller does not get to
    // choose them — but they are not criteria, and `matches` does not read them back.
    let fixed = container.fixed.iter().enumerate().map(|(index, fixed)| {
        let binding = match &fixed.xtce_name {
            Some(name) => format_ident!("fixed_{}", xtce_codegen::plan::field_ident(name)),
            None => format_ident!("fixed_value_{index}"),
        };
        let Some(raw) = fixed.as_u64() else {
            // Wider than a literal. The layout has already established it is whole bytes, so
            // each one is its own assignment — no shifts, no accumulator, and every index a
            // literal into an array of known length, as everywhere else here.
            let writes = fixed.value.iter().enumerate().map(|(offset, byte)| {
                let at = Literal::usize_unsuffixed(fixed.bit_offset / 8 + offset);
                let byte = Literal::u8_unsuffixed(*byte);
                quote! { #out[#at] = #byte; }
            });
            return quote! { #(#writes)* };
        };
        let raw = Literal::u64_unsuffixed(raw);
        let write = write_bits(fixed.bit_offset, fixed.bit_width, &quote!(#binding), &out);
        quote! {
            {
                let #binding: u64 = #raw;
                #write
            }
        }
    });

    let fields = container
        .fields
        .iter()
        .map(|field| encode_field(field, &out));

    quote! {
        /// Writes this packet into `out`, returning how many bytes were written.
        ///
        /// # Errors
        ///
        /// [`EncodeError::TooShort`] if `out` is smaller than [`Self::LEN`], and
        /// [`EncodeError::OutOfRange`] naming the parameter if a value does not fit the
        /// bits the definition gives it. Nothing is truncated silently.
        pub fn encode(&self, out: &mut [u8]) -> Result<usize, EncodeError> {
            // One conversion, and every index below is a literal into an array of known
            // length: in bounds by construction, so no bounds check and no panic remain.
            let out: &mut [u8; #len] = match out.get_mut(..#len) {
                Some(slice) => match <&mut [u8; #len]>::try_from(slice) {
                    Ok(array) => array,
                    Err(_) => return Err(EncodeError::TooShort { needed: #len }),
                },
                None => return Err(EncodeError::TooShort { needed: #len }),
            };

            // Bits are written with `|`, because two fields can share a byte. That only
            // gives the right answer over a clean buffer.
            *out = [0u8; #len];

            #(#constants)*
            #(#fixed)*
            #(#fields)*

            Ok(#len)
        }
    }
}

fn encode_field(field: &FlightField, out: &Ident) -> TokenStream {
    let value = {
        let ident = ident(&field.ident);
        quote!(self.#ident)
    };
    let name = &field.xtce_name;

    match &field.kind {
        Kind::Text { charset, delimiter } => {
            return encode_text(field, &value, *charset, delimiter, out);
        }
        Kind::Binary => return encode_binary(field, &value, out),
        _ => {}
    }

    let raw = raw_from_value(field, &value);
    let raw = reverse_if_little_endian(field, &raw);
    let check = range_check(field, &value, name);
    // `field_ident` escapes a keyword by appending an underscore, and `type__bits` is not a
    // snake-case name.
    let binding = format_ident!("{}_bits", field.ident.trim_end_matches('_'));
    let write = write_bits(field.bit_offset, field.bit_width, &quote!(#binding), out);

    quote! {
        {
            #check
            let #binding: u64 = #raw;
            #write
        }
    }
}

/// The expression turning a field's typed value into the bits that go into the packet.
fn raw_from_value(field: &FlightField, value: &TokenStream) -> TokenStream {
    let mask = Literal::u64_unsuffixed(mask_for(field.bit_width));
    match &field.kind {
        // `Unsigned` under `Signed` is XTCE's way of saying a signed parameter carries an
        // unsigned encoding; the bits are written the same way either way.
        Kind::Unsigned | Kind::Signed(IntegerCoding::Unsigned) => {
            quote!(#value as u64 & #mask)
        }
        Kind::Signed(IntegerCoding::TwosComplement) => quote!(#value as i64 as u64 & #mask),
        Kind::Signed(IntegerCoding::SignMagnitude) => {
            let sign = Literal::u64_unsuffixed(1u64 << (field.bit_width - 1));
            quote! {
                if #value < 0 {
                    #sign | (#value as i64).unsigned_abs()
                } else {
                    #value as u64
                }
            }
        }
        Kind::Signed(IntegerCoding::OnesComplement) => {
            // A negative number is the bitwise complement of its magnitude, which within the
            // field's width is the magnitude exclusive-or'd with the mask.
            quote! {
                if #value < 0 {
                    #mask ^ (#value as i64).unsigned_abs()
                } else {
                    #value as u64
                }
            }
        }
        Kind::Float16 => quote!(f32_to_half(#value) as u64),
        Kind::Float32 => quote!(#value.to_bits() as u64),
        Kind::Float64 => quote!(#value.to_bits()),
        Kind::Bool => quote!(if #value { 1 } else { 0 }),
        Kind::Enumerated(_) => quote!(#value.raw()),
        // Handled before this function is reached.
        Kind::Text { .. } | Kind::Binary => quote!(0),
    }
}

/// Puts a little-endian field's bytes the other way round, on the way out.
///
/// The decoder reverses `width / 8` bytes of what it reads, so the encoder reverses them
/// again — which is the same operation, because a byte reversal is its own inverse. Only
/// whole-byte widths reach here; the layout refuses the rest, since reversing a value
/// narrower than its byte count has no inverse at all.
///
/// `swap_bytes` on the widened integer does the reversal, and the shift discards the bytes
/// above the field: a 24-bit value in a `u32` reverses to four bytes, of which the top three
/// are the ones wanted.
fn reverse_if_little_endian(field: &FlightField, raw: &TokenStream) -> TokenStream {
    if !field.swap_bytes || field.bit_width <= 8 {
        return raw.clone();
    }
    let natural = natural_bits(field.bit_width);
    let ty = unsigned_type(natural);
    let shift = Literal::u32_unsuffixed(natural - field.bit_width);
    // `(#raw)` parenthesised: the expression is a masked `as` cast already, and `as` binds
    // to its last operand rather than to the whole of it.
    if natural == field.bit_width {
        quote!(((#raw) as #ty).swap_bytes() as u64)
    } else {
        quote!((((#raw) as #ty).swap_bytes() >> #shift) as u64)
    }
}

/// The guard that rejects a value the field cannot hold.
///
/// A float always fits: its width picks its type. An integer usually does not — a 12-bit
/// field takes a `u16`, and three quarters of a `u16` do not fit in it.
fn range_check(field: &FlightField, value: &TokenStream, name: &str) -> TokenStream {
    let natural = natural_bits(field.bit_width);
    let width = field.bit_width;

    match &field.kind {
        Kind::Unsigned => {
            if width >= natural {
                return quote!();
            }
            let max = Literal::u64_unsuffixed(mask_for(width));
            quote! {
                if #value as u64 > #max {
                    return Err(EncodeError::OutOfRange { parameter: #name });
                }
            }
        }
        Kind::Signed(IntegerCoding::TwosComplement | IntegerCoding::Unsigned) => {
            if width >= natural {
                return quote!();
            }
            let min = Literal::i64_unsuffixed(-(1i64 << (width - 1)));
            let max = Literal::i64_unsuffixed((1i64 << (width - 1)) - 1);
            quote! {
                if (#value as i64) < #min || (#value as i64) > #max {
                    return Err(EncodeError::OutOfRange { parameter: #name });
                }
            }
        }
        // Sign-magnitude and ones' complement both spend a bit on the sign and have two
        // spellings of zero, so their range is symmetric and one short of two's complement
        // at the bottom. That holds even at the full width of the Rust type, which is why
        // this check is not skipped the way the two's-complement one is.
        Kind::Signed(IntegerCoding::SignMagnitude | IntegerCoding::OnesComplement) => {
            let magnitude = Literal::u64_unsuffixed((1u64 << (width - 1)) - 1);
            quote! {
                if (#value as i64).unsigned_abs() > #magnitude {
                    return Err(EncodeError::OutOfRange { parameter: #name });
                }
            }
        }
        _ => quote!(),
    }
}

fn encode_text(
    field: &FlightField,
    value: &TokenStream,
    charset: TextCharset,
    delimiter: &TextDelimiter,
    out: &Ident,
) -> TokenStream {
    let name = &field.xtce_name;
    let start = field.bit_offset / 8;
    let len = field.bit_width as usize / 8;
    let ascii_check = if matches!(charset, TextCharset::UsAscii) {
        quote! {
            if !#value.is_ascii() {
                return Err(EncodeError::InvalidText { parameter: #name });
            }
        }
    } else {
        quote!()
    };

    let body = match delimiter {
        TextDelimiter::WholeBuffer => {
            let len_literal = Literal::usize_unsuffixed(len);
            let copy = copy_into(start, len, &quote!(bytes), out);
            quote! {
                let bytes = #value.as_bytes();
                // The whole buffer is the string, so a shorter one would decode with its
                // padding attached. Requiring the exact length keeps encode and decode
                // inverses of each other; padding, if a mission wants it, is the caller's.
                if bytes.len() != #len_literal {
                    return Err(EncodeError::TextLength { parameter: #name });
                }
                #copy
            }
        }
        TextDelimiter::TerminationChar(terminator) => {
            let terminator_len = terminator.len();
            let start_literal = Literal::usize_unsuffixed(start);
            let room = Literal::usize_unsuffixed(len.saturating_sub(terminator_len));
            let bytes = terminator.iter().map(|byte| Literal::u8_unsuffixed(*byte));
            let copy = copy_into(start, len, &quote!(bytes), out);
            let write_terminator = (0..terminator_len).map(|index| {
                let at = Literal::usize_unsuffixed(index);
                quote! { slot[#at] = TERMINATOR[#at]; }
            });
            let terminator_len_literal = Literal::usize_unsuffixed(terminator_len);
            quote! {
                const TERMINATOR: &[u8] = &[#(#bytes),*];
                let bytes = #value.as_bytes();
                if bytes.len() > #room {
                    return Err(EncodeError::TextLength { parameter: #name });
                }
                // A terminator inside the string would make the decoded value a prefix of
                // what was encoded — a silent corruption rather than a failure.
                if find(bytes, TERMINATOR).is_some() {
                    return Err(EncodeError::EmbeddedTerminator { parameter: #name });
                }
                #copy
                // The buffer is already zero, so only the terminator itself is written.
                let at = #start_literal + bytes.len();
                match #out.get_mut(at..at + #terminator_len_literal) {
                    Some(slot) => { #(#write_terminator)* }
                    None => return Err(EncodeError::TextLength { parameter: #name }),
                }
            }
        }
        TextDelimiter::LeadingSize { size_in_bits } => {
            let prefix_bytes = *size_in_bits as usize / 8;
            let room = Literal::usize_unsuffixed(len.saturating_sub(prefix_bytes));
            let max_bits = Literal::u64_unsuffixed(mask_for(*size_in_bits));
            let write_prefix =
                write_bits(field.bit_offset, *size_in_bits, &quote!(length_bits), out);
            let copy = copy_into(
                start + prefix_bytes,
                len - prefix_bytes,
                &quote!(bytes),
                out,
            );
            quote! {
                let bytes = #value.as_bytes();
                if bytes.len() > #room {
                    return Err(EncodeError::TextLength { parameter: #name });
                }
                // The prefix counts bits, as XTCE specifies, not bytes.
                let length_bits = bytes.len() as u64 * 8;
                if length_bits > #max_bits {
                    return Err(EncodeError::TextLength { parameter: #name });
                }
                #write_prefix
                #copy
            }
        }
    };

    quote! {
        {
            #ascii_check
            #body
        }
    }
}

fn encode_binary(field: &FlightField, value: &TokenStream, out: &Ident) -> TokenStream {
    let name = &field.xtce_name;
    let start = field.bit_offset / 8;
    let len = field.bit_width as usize / 8;
    let len_literal = Literal::usize_unsuffixed(len);
    let copy = copy_into(start, len, &quote!(bytes), out);

    quote! {
        {
            let bytes: &[u8] = #value;
            // Fixed-size binary has no delimiter, so a short value cannot be told from a
            // padded one on the way back.
            if bytes.len() != #len_literal {
                return Err(EncodeError::BinaryLength { parameter: #name });
            }
            #copy
        }
    }
}

/// Copies at most `len` bytes into the buffer at `start`.
///
/// `zip` rather than `copy_from_slice`: the latter panics when the lengths differ, and the
/// point of this generator is that no such branch exists. The lengths have already been
/// checked, so the two are equivalent here — except in what they leave in the binary.
fn copy_into(start: usize, len: usize, source: &TokenStream, out: &Ident) -> TokenStream {
    let from = Literal::usize_unsuffixed(start);
    let to = Literal::usize_unsuffixed(start + len);
    quote! {
        for (slot, byte) in #out[#from..#to].iter_mut().zip(#source) {
            *slot = *byte;
        }
    }
}

/// Writes `width` bits of `value` at `offset`, by `or`-ing them into the bytes they touch.
///
/// The mirror of the read in `xtce-codegen`: the same span, the same shift, the same choice
/// of the narrowest integer that covers the span. Nine bytes is not a hypothetical — a
/// 64-bit field one bit off a boundary needs `u128` here for the same reason it needs it
/// there, and getting it wrong loses the top of the field rather than failing.
fn write_bits(offset: usize, width: u32, value: &TokenStream, out: &Ident) -> TokenStream {
    let first = offset / 8;
    let last = (offset + width as usize - 1) / 8;
    let span = last - first + 1;
    let bit_in_byte = (offset % 8) as u32;

    let slots = match span {
        1 => 1usize,
        2 => 2,
        3..=4 => 4,
        5..=8 => 8,
        _ => 16,
    };
    let pad = slots - span;
    let shift = (span as u32) * 8 - bit_in_byte - width;
    let whole = shift == 0 && width == (span as u32) * 8;

    let accumulator = match slots {
        1 => quote!(u8),
        2 => quote!(u16),
        4 => quote!(u32),
        8 => quote!(u64),
        _ => quote!(u128),
    };

    // The cast is parenthesised whenever a shift follows it. `x as u64 << 8` does not
    // parse — the type after `as` takes the `<<` for the start of generic arguments — and
    // there is nothing else here to supply the parentheses by accident.
    let shifted = if shift == 0 {
        quote!(#value as #accumulator)
    } else {
        let shift = Literal::u32_unsuffixed(shift);
        quote!((#value as #accumulator) << #shift)
    };

    if slots == 1 {
        let index = Literal::usize_unsuffixed(first);
        // A field that owns its whole byte has nothing to preserve in it.
        return if whole {
            quote! { #out[#index] = #shifted; }
        } else {
            quote! { #out[#index] |= #shifted; }
        };
    }

    let combined = if whole {
        shifted
    } else {
        let existing = (0..pad)
            .map(|_| quote!(0))
            .chain((first..=last).map(|index| {
                let index = Literal::usize_unsuffixed(index);
                quote! { #out[#index] }
            }));
        quote!(#accumulator::from_be_bytes([#(#existing),*]) | #shifted)
    };

    let stores = (0..span).map(|index| {
        let target = Literal::usize_unsuffixed(first + index);
        let source = Literal::usize_unsuffixed(pad + index);
        quote! { #out[#target] = bytes[#source]; }
    });

    quote! {
        {
            let bytes = (#combined).to_be_bytes();
            #(#stores)*
        }
    }
}

// ---------------------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------------------

fn decode_body(container: &Container, enums: &[EnumType]) -> TokenStream {
    let len = Literal::usize_unsuffixed(container.len_bytes);
    let data = format_ident!("data");

    let bindings = container
        .fields
        .iter()
        .map(|field| decode_field(field, &data, enums));
    let names = container.fields.iter().map(|field| ident(&field.ident));

    let lifetime = if container.borrows {
        quote!(&'a [u8])
    } else {
        quote!(&[u8])
    };

    quote! {
        /// Reads this packet out of `data`.
        ///
        /// The inverse of [`Self::encode`]: everything `encode` writes, `decode` gives back
        /// unchanged. It does not check the restriction criteria — use
        /// [`Self::matches`] for that, or the module's `decode` to choose a container.
        ///
        /// # Errors
        ///
        /// [`DecodeError::TooShort`] if `data` is smaller than [`Self::LEN`], and a variant
        /// naming the parameter if its bytes are not a value the definition allows.
        pub fn decode(data: #lifetime) -> Result<Self, DecodeError> {
            let data: &[u8; #len] = match data.get(..#len) {
                Some(slice) => match <&[u8; #len]>::try_from(slice) {
                    Ok(array) => array,
                    Err(_) => return Err(DecodeError::TooShort { needed: #len }),
                },
                None => return Err(DecodeError::TooShort { needed: #len }),
            };

            #(#bindings)*

            Ok(Self { #(#names,)* })
        }
    }
}

fn decode_field(field: &FlightField, data: &Ident, enums: &[EnumType]) -> TokenStream {
    let name = ident(&field.ident);
    let xtce_name = &field.xtce_name;

    let value = match &field.kind {
        Kind::Text { charset, delimiter } => decode_text(field, *charset, delimiter, data),
        Kind::Binary => {
            let slice = byte_slice(field, data);
            quote!(#slice)
        }
        Kind::Bool => {
            let raw = read_bits_in_order(field, data);
            quote!((#raw) != 0)
        }
        Kind::Enumerated(index) => {
            let raw = read_bits_in_order(field, data);
            let ty = enum_name(*index, enums);
            quote! {
                match #ty::from_raw(#raw) {
                    Some(value) => value,
                    None => return Err(DecodeError::UnknownLabel { parameter: #xtce_name }),
                }
            }
        }
        Kind::Float16 => {
            let raw = read_bits_in_order(field, data);
            quote!(half_to_f32((#raw) as u16))
        }
        Kind::Float32 => {
            let raw = read_bits_in_order(field, data);
            quote!(f32::from_bits((#raw) as u32))
        }
        Kind::Float64 => {
            let raw = read_bits_in_order(field, data);
            quote!(f64::from_bits(#raw))
        }
        Kind::Unsigned => {
            let raw = read_bits_in_order(field, data);
            let ty = unsigned_type(natural_bits(field.bit_width));
            quote!((#raw) as #ty)
        }
        Kind::Signed(coding) => {
            let raw = read_bits_in_order(field, data);
            let ty = signed_type(natural_bits(field.bit_width));
            let signed = signed_from_raw(&raw, field.bit_width, *coding);
            quote!(#signed as #ty)
        }
    };

    quote! { let #name = #value; }
}

/// The `i64` a raw field means, under one of XTCE's three signed codings.
///
/// Written to be the exact inverse of what `xtce-decode` computes, because that is the
/// oracle the round trip is checked against.
fn signed_from_raw(raw: &TokenStream, width: u32, coding: IntegerCoding) -> TokenStream {
    match coding {
        IntegerCoding::Unsigned => quote!((#raw) as i64),
        IntegerCoding::TwosComplement => {
            // Sign-extend by shifting up and back down. Subtracting `2^width` overflows at
            // width 63.
            let shift = Literal::u32_unsuffixed(64 - width);
            quote! { ((((#raw) << #shift) as i64) >> #shift) }
        }
        IntegerCoding::SignMagnitude => {
            let sign = Literal::u64_unsuffixed(1u64 << (width - 1));
            let magnitude = Literal::u64_unsuffixed((1u64 << (width - 1)) - 1);
            quote! {
                {
                    let raw = #raw;
                    let magnitude = (raw & #magnitude) as i64;
                    if raw & #sign == 0 { magnitude } else { -magnitude }
                }
            }
        }
        IntegerCoding::OnesComplement => {
            let sign = Literal::u64_unsuffixed(1u64 << (width - 1));
            let mask = Literal::u64_unsuffixed(mask_for(width));
            quote! {
                {
                    let raw = #raw;
                    if raw & #sign == 0 {
                        raw as i64
                    } else {
                        -(((!raw) & #mask) as i64)
                    }
                }
            }
        }
    }
}

fn decode_text(
    field: &FlightField,
    charset: TextCharset,
    delimiter: &TextDelimiter,
    data: &Ident,
) -> TokenStream {
    let name = &field.xtce_name;
    let buffer = byte_slice(field, data);
    let ascii = matches!(charset, TextCharset::UsAscii);

    let bytes = match delimiter {
        TextDelimiter::WholeBuffer => quote!(#buffer),
        TextDelimiter::TerminationChar(terminator) => {
            let literals = terminator.iter().map(|byte| Literal::u8_unsuffixed(*byte));
            quote! {
                {
                    const TERMINATOR: &[u8] = &[#(#literals),*];
                    let buffer = #buffer;
                    match find(buffer, TERMINATOR) {
                        Some(end) => match buffer.get(..end) {
                            Some(text) => text,
                            None => return Err(DecodeError::UnterminatedString { parameter: #name }),
                        },
                        None => return Err(DecodeError::UnterminatedString { parameter: #name }),
                    }
                }
            }
        }
        TextDelimiter::LeadingSize { size_in_bits } => {
            let prefix_bytes = Literal::usize_unsuffixed(*size_in_bits as usize / 8);
            let prefix = read_bits(field.bit_offset, *size_in_bits, data);
            quote! {
                {
                    let buffer = #buffer;
                    let length_bits = #prefix;
                    if length_bits % 8 != 0 {
                        return Err(DecodeError::BadStringLength { parameter: #name });
                    }
                    let length = (length_bits / 8) as usize;
                    match buffer.get(#prefix_bytes..#prefix_bytes + length) {
                        Some(text) => text,
                        None => return Err(DecodeError::BadStringLength { parameter: #name }),
                    }
                }
            }
        }
    };

    let ascii_check = if ascii {
        quote! {
            if !text.is_ascii() {
                return Err(DecodeError::InvalidText { parameter: #name });
            }
        }
    } else {
        quote!()
    };

    quote! {
        {
            let text = #bytes;
            #ascii_check
            match core::str::from_utf8(text) {
                Ok(text) => text,
                Err(_) => return Err(DecodeError::InvalidText { parameter: #name }),
            }
        }
    }
}

/// A field's raw bits, with its byte order applied.
fn read_bits_in_order(field: &FlightField, data: &Ident) -> TokenStream {
    let raw = read_bits(field.bit_offset, field.bit_width, data);
    if !field.swap_bytes || field.bit_width <= 8 {
        return raw;
    }
    let natural = natural_bits(field.bit_width);
    let ty = unsigned_type(natural);
    let shift = Literal::u32_unsuffixed(natural - field.bit_width);
    if natural == field.bit_width {
        quote!(((#raw) as #ty).swap_bytes() as u64)
    } else {
        quote!((((#raw) as #ty).swap_bytes() >> #shift) as u64)
    }
}

/// A byte-aligned field's bytes, as a slice of the packet.
fn byte_slice(field: &FlightField, data: &Ident) -> TokenStream {
    let from = Literal::usize_unsuffixed(field.bit_offset / 8);
    let to = Literal::usize_unsuffixed((field.bit_offset + field.bit_width as usize) / 8);
    quote!(&#data[#from..#to])
}

/// Reads `width` bits at `offset` as a `u64`.
fn read_bits(offset: usize, width: u32, data: &Ident) -> TokenStream {
    let first = offset / 8;
    let last = (offset + width as usize - 1) / 8;
    let span = last - first + 1;
    let bit_in_byte = (offset % 8) as u32;
    let mask = mask_for(width);

    let slots = match span {
        1 => 1usize,
        2 => 2,
        3..=4 => 4,
        5..=8 => 8,
        _ => 16,
    };
    let pad = slots - span;
    let shift = (span as u32) * 8 - bit_in_byte - width;

    let load = if slots == 1 {
        let index = Literal::usize_unsuffixed(first);
        quote!(#data[#index])
    } else {
        let bytes = (0..pad)
            .map(|_| quote!(0))
            .chain((first..=last).map(|index| {
                let index = Literal::usize_unsuffixed(index);
                quote! { #data[#index] }
            }));
        let ty = match slots {
            2 => quote!(u16),
            4 => quote!(u32),
            8 => quote!(u64),
            _ => quote!(u128),
        };
        quote!(#ty::from_be_bytes([#(#bytes),*]))
    };

    // A nine-byte span is loaded as `u128` and has to stay one until after the shift:
    // narrowing first drops exactly the bits it was widened for.
    if slots == 16 {
        let mask = Literal::u64_unsuffixed(mask);
        let shift = Literal::u32_unsuffixed(shift);
        return quote!(((#load >> #shift) as u64) & #mask);
    }

    let mask = Literal::u64_unsuffixed(mask);
    if shift == 0 {
        quote!(#load as u64 & #mask)
    } else {
        let shift = Literal::u32_unsuffixed(shift);
        quote!(((#load as u64) >> #shift) & #mask)
    }
}

// ---------------------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------------------

/// `matches`: whether a buffer carries this container's restriction criteria.
fn matches_body(container: &Container) -> TokenStream {
    let len = Literal::usize_unsuffixed(container.len_bytes);
    let data = format_ident!("data");

    let tests = container.constants.iter().map(|constant| {
        let read = read_bits(constant.bit_offset, constant.bit_width, &data);
        let expected = Literal::u64_unsuffixed(constant.raw);
        quote! {
            if (#read) != #expected {
                return false;
            }
        }
    });

    quote! {
        /// Whether `data` satisfies the restriction criteria that select this container.
        ///
        /// A container with no criteria matches anything long enough to hold it.
        #[must_use]
        pub fn matches(data: &[u8]) -> bool {
            let data: &[u8; #len] = match data.get(..#len) {
                Some(slice) => match <&[u8; #len]>::try_from(slice) {
                    Ok(array) => array,
                    Err(_) => return false,
                },
                None => return false,
            };
            let _ = data;
            #(#tests)*
            true
        }
    }
}

/// The module-level `Packet` enum and `decode`.
fn dispatcher(layout: &Layout) -> TokenStream {
    let borrows = layout.containers.iter().any(|container| container.borrows);
    let lifetime = if borrows { quote!(<'a>) } else { quote!() };

    let variants = layout.containers.iter().map(|container| {
        let variant = ident(&container.type_ident);
        let ty = ident(&container.type_ident);
        let inner = if container.borrows {
            quote!(#ty<'a>)
        } else {
            quote!(#ty)
        };
        let doc = format!("`{}`.", container.xtce_name);
        quote! {
            #[doc = #doc]
            #variant(#inner)
        }
    });

    let arms = layout.containers.iter().map(|container| {
        let ty = ident(&container.type_ident);
        quote! {
            if #ty::matches(data) {
                matched += 1;
                selected = Some(Selected::#ty);
            }
        }
    });
    let selectors = layout.containers.iter().map(|container| {
        let ty = ident(&container.type_ident);
        quote!(#ty)
    });
    let decodes = layout.containers.iter().map(|container| {
        let ty = ident(&container.type_ident);
        quote! {
            Selected::#ty => Ok(Packet::#ty(#ty::decode(data)?))
        }
    });

    let data_type = if borrows {
        quote!(&'a [u8])
    } else {
        quote!(&[u8])
    };
    let lifetime_in_signature = if borrows { quote!(<'a>) } else { quote!() };

    quote! {
        /// Any packet this module can decode.
        #[derive(Clone, Copy, PartialEq, Debug)]
        pub enum Packet #lifetime {
            #(#variants,)*
        }

        /// Chooses a container by its restriction criteria and decodes `data` as it.
        ///
        /// # Errors
        ///
        /// [`DecodeError::Unrecognized`] when no container's criteria hold, and
        /// [`DecodeError::Ambiguous`] when more than one does — a definition that cannot
        /// say which container a packet is, is a defect worth reporting rather than
        /// resolving by declaration order.
        pub fn decode #lifetime_in_signature (data: #data_type) -> Result<Packet #lifetime, DecodeError> {
            #[derive(Clone, Copy)]
            enum Selected {
                #(#selectors,)*
            }

            let mut matched = 0usize;
            let mut selected = None;
            #(#arms)*

            if matched > 1 {
                return Err(DecodeError::Ambiguous);
            }
            match selected {
                Some(selected) => match selected {
                    #(#decodes,)*
                },
                None => Err(DecodeError::Unrecognized),
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Shared definitions
// ---------------------------------------------------------------------------------------

fn helpers(layout: &Layout) -> TokenStream {
    let mut needs = BTreeSet::new();
    for container in &layout.containers {
        for field in &container.fields {
            match &field.kind {
                Kind::Float16 => {
                    needs.insert("half");
                }
                Kind::Text {
                    delimiter: TextDelimiter::TerminationChar(_),
                    ..
                } => {
                    needs.insert("find");
                }
                _ => {}
            }
            // Every calibrator the field can apply, not only its default: a spline reachable
            // solely through a context calibrator still needs `spline_value` written. The
            // integral-or-float split keys off the field's encoding, which all of them share.
            let calibrations = field
                .calibration
                .iter()
                .chain(field.contexts.iter().map(|context| &context.calibration));
            for calibration in calibrations {
                match (calibration, &field.kind) {
                    // The exact-power helper is only reachable from an integral encoding; a
                    // float raw uses `powi` inline.
                    (Calibration::Polynomial(_), Kind::Unsigned | Kind::Signed(_)) => {
                        needs.insert("power");
                        needs.insert("powi");
                    }
                    (Calibration::Polynomial(_), _) => {
                        needs.insert("powi");
                    }
                    (Calibration::Spline(_), _) => {
                        needs.insert("spline");
                    }
                }
            }
        }
    }

    let half = if needs.contains("half") {
        half_helpers()
    } else {
        quote!()
    };
    let find = if needs.contains("find") {
        quote! {
            /// The index of the first occurrence of `needle` in `haystack`.
            ///
            /// A plain scan, matching what the reference implementation does. `windows`
            /// would panic on an empty needle, which cannot happen here: a definition with
            /// an empty termination character is refused when the code is generated.
            fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
                if needle.is_empty() || haystack.len() < needle.len() {
                    return None;
                }
                let mut at = 0usize;
                while at + needle.len() <= haystack.len() {
                    match haystack.get(at..at + needle.len()) {
                        Some(window) if window == needle => return Some(at),
                        _ => {}
                    }
                    at += 1;
                }
                None
            }
        }
    } else {
        quote!()
    };

    let powi = if needs.contains("powi") {
        powi_helper()
    } else {
        quote!()
    };
    let power = if needs.contains("power") {
        integer_power_helper()
    } else {
        quote!()
    };
    let spline = if needs.contains("spline") {
        spline_helpers()
    } else {
        quote!()
    };

    let errors = errors();

    quote! {
        #errors
        #half
        #find
        #powi
        #power
        #spline
    }
}

/// `base^exponent` as `xtce-decode` computes it for an integral raw value.
///
/// Exactly, in `i128`, converted once — not by repeated multiplication, which rounds at every
/// step. Panic-free: `checked_pow` returns `None` rather than overflowing, and there is no
/// indexing.
fn integer_power_helper() -> TokenStream {
    quote! {
        fn integer_power(base: i128, exponent: i32) -> f64 {
            if exponent < 0 {
                return powi(base as f64, exponent);
            }
            match u32::try_from(exponent)
                .ok()
                .and_then(|exponent| base.checked_pow(exponent))
            {
                Some(exact) => exact as f64,
                None => powi(base as f64, exponent),
            }
        }
    }
}

/// `f64::powi`, written out.
///
/// This is why the bare-metal probe exists. `powi` is in `std`, not `core`, so the first
/// version of the calibration emitter produced code that ran the tests perfectly and would
/// not build for a Cortex-M at all. The sequence below is the one `powi` performs — square
/// and multiply, lowest bit first — so it is bit-identical to it, which matters because the
/// interpreter this is checked against calls the real thing.
fn powi_helper() -> TokenStream {
    quote! {
        fn powi(x: f64, exponent: i32) -> f64 {
            // `unsigned_abs`, not negation: `-i32::MIN` overflows.
            let mut remaining = exponent.unsigned_abs();
            let mut result = 1.0f64;
            let mut base = x;
            let mut started = false;
            while remaining > 0 {
                if remaining & 1 == 1 {
                    result = if started { result * base } else { base };
                    started = true;
                }
                remaining >>= 1;
                if remaining > 0 {
                    base = base * base;
                }
            }
            let value = if started { result } else { 1.0 };
            if exponent < 0 { 1.0 / value } else { value }
        }
    }
}

/// Spline interpolation, line for line as `xtce-decode` does it.
///
/// Every lookup is `get`, so nothing here can panic; the order and the extrapolation flag are
/// constants at every call site and fold away.
fn spline_helpers() -> TokenStream {
    quote! {
        fn spline_line(x: f64, x0: f64, x1: f64, y0: f64, y1: f64) -> f64 {
            if (x1 - x0) == 0.0 {
                return y0;
            }
            let slope = (y1 - y0) / (x1 - x0);
            slope * (x - x0) + y0
        }

        fn spline_value(
            points: &[(f64, f64)],
            order: u8,
            extrapolate: bool,
            query: f64,
        ) -> Option<f64> {
            let first = *points.first()?;
            let last = *points.last()?;

            if query < first.0 {
                if !extrapolate {
                    return None;
                }
                return Some(match order {
                    0 => first.1,
                    _ => match points.get(1) {
                        Some(second) => {
                            spline_line(query, first.0, second.0, first.1, second.1)
                        }
                        None => first.1,
                    },
                });
            }

            if query > last.0 {
                if !extrapolate {
                    return None;
                }
                return Some(match order {
                    0 => last.1,
                    _ => match points.len().checked_sub(2).and_then(|at| points.get(at)) {
                        Some(previous) => {
                            spline_line(query, previous.0, last.0, previous.1, last.1)
                        }
                        None => last.1,
                    },
                });
            }

            // The first point strictly above the query. A NaN query makes every comparison
            // false, including both above, so it lands here at zero — which is what the
            // floor is for.
            let hi = points.partition_point(|point| point.0 <= query).max(1);

            Some(match order {
                0 => points
                    .get(hi.saturating_sub(1))
                    .map_or(first.1, |point| point.1),
                _ => {
                    if points.len() < 2 {
                        return Some(first.1);
                    }
                    // A query equal to the last raw value has no point above it;
                    // interpolating over the final segment lands exactly on it.
                    let upper = hi.min(points.len() - 1);
                    match (points.get(upper.saturating_sub(1)), points.get(upper)) {
                        (Some(lower), Some(higher)) => {
                            spline_line(query, lower.0, higher.0, lower.1, higher.1)
                        }
                        _ => first.1,
                    }
                }
            })
        }
    }
}

fn half_helpers() -> TokenStream {
    quote! {
        /// Widens IEEE-754 binary16 to `f32`, exactly.
        ///
        /// Public because a caller that has to build a binary16 value by hand — a test
        /// fixture, a ground tool checking what the spacecraft will see — needs the same
        /// conversion the generated code uses, not a second one that might differ.
        ///
        /// Every binary16 value is representable in `f32`, subnormals included, so this
        /// loses nothing. A binary16 subnormal is a normal `f32`, which is why the
        /// subnormal arm renormalises rather than shifting the fraction into place.
        pub fn half_to_f32(bits: u16) -> f32 {
            let sign = u32::from(bits & 0x8000) << 16;
            let exponent = u32::from((bits >> 10) & 0x1F);
            let fraction = u32::from(bits & 0x03FF);
            match exponent {
                0 if fraction == 0 => f32::from_bits(sign),
                0 => {
                    // `p = 31 - leading_zeros()` is the index of the highest set bit, so
                    // the shift that moves it to bit 10 — the implicit-one position of the
                    // ten-bit window — is `10 - p`, which is `leading_zeros() - 21`. The
                    // mask then drops that implicit one.
                    let shift = fraction.leading_zeros() - 21;
                    let fraction = (fraction << shift) & 0x03FF;
                    let exponent = 113 - shift;
                    f32::from_bits(sign | (exponent << 23) | (fraction << 13))
                }
                0x1F => f32::from_bits(sign | 0x7F80_0000 | (fraction << 13)),
                _ => f32::from_bits(sign | ((exponent + 127 - 15) << 23) | (fraction << 13)),
            }
        }

        /// Narrows `f32` to IEEE-754 binary16, rounding to nearest and ties to even.
        ///
        /// Rounding, not rejection: a flight computer holding a temperature in `f32` should
        /// be able to put it in a 16-bit field, and the definition already said how much
        /// precision that field has. What the ground reads back is this rounded value.
        pub fn f32_to_half(value: f32) -> u16 {
            let bits = value.to_bits();
            let sign = ((bits >> 16) & 0x8000) as u16;
            let exponent = ((bits >> 23) & 0xFF) as i32;
            let fraction = bits & 0x007F_FFFF;

            if exponent == 0xFF {
                // Infinity keeps its fraction of zero; a NaN has to stay a NaN, so a
                // payload that would shift away entirely is replaced by a quiet one.
                let payload = if fraction == 0 {
                    0
                } else {
                    ((fraction >> 13) as u16) | 0x0200
                };
                return sign | 0x7C00 | payload;
            }

            let unbiased = exponent - 127;
            if unbiased > 15 {
                return sign | 0x7C00;
            }
            if unbiased < -24 {
                return sign;
            }
            if unbiased < -14 {
                // Subnormal in binary16: the implicit one becomes explicit and the whole
                // significand shifts down.
                let shift = (-14 - unbiased) as u32;
                let significand = fraction | 0x0080_0000;
                let total = 13 + shift;
                let truncated = significand >> total;
                let remainder = significand & ((1u32 << total) - 1);
                let halfway = 1u32 << (total - 1);
                let round =
                    u32::from(remainder > halfway || (remainder == halfway && truncated & 1 == 1));
                return sign | ((truncated + round) as u16);
            }

            let truncated = (((unbiased + 15) as u32) << 10) | (fraction >> 13);
            let remainder = fraction & 0x1FFF;
            let round =
                u32::from(remainder > 0x1000 || (remainder == 0x1000 && truncated & 1 == 1));
            // A carry out of the fraction lands in the exponent, and out of the exponent
            // lands on infinity. Both are what rounding to nearest is supposed to do.
            sign | ((truncated + round) as u16)
        }
    }
}

// Two error enums and their `Display` impls. Splitting them apart would only mean two
// functions that must be kept in step with each other.
#[allow(clippy::too_many_lines)]
fn errors() -> TokenStream {
    quote! {
        /// Why a packet could not be encoded.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum EncodeError {
            /// The buffer is smaller than the container needs.
            TooShort {
                /// Bytes the container needs.
                needed: usize,
            },
            /// A value does not fit the bits the definition gives its parameter.
            OutOfRange {
                /// The parameter, named as the definition spells it.
                parameter: &'static str,
            },
            /// A string does not fit its field, or does not fill it.
            TextLength {
                /// The parameter, named as the definition spells it.
                parameter: &'static str,
            },
            /// A string contains the byte sequence that terminates it.
            EmbeddedTerminator {
                /// The parameter, named as the definition spells it.
                parameter: &'static str,
            },
            /// A string holds characters its declared character set cannot represent.
            InvalidText {
                /// The parameter, named as the definition spells it.
                parameter: &'static str,
            },
            /// A binary value is not exactly as wide as its field.
            BinaryLength {
                /// The parameter, named as the definition spells it.
                parameter: &'static str,
            },
        }

        impl core::fmt::Display for EncodeError {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                match self {
                    Self::TooShort { needed } => {
                        write!(f, "buffer is smaller than the {needed} byte(s) the container needs")
                    }
                    Self::OutOfRange { parameter } => {
                        write!(f, "{parameter}: value does not fit the field")
                    }
                    Self::TextLength { parameter } => {
                        write!(f, "{parameter}: string does not fit the field")
                    }
                    Self::EmbeddedTerminator { parameter } => {
                        write!(f, "{parameter}: string contains its own terminator")
                    }
                    Self::InvalidText { parameter } => {
                        write!(f, "{parameter}: characters outside the declared character set")
                    }
                    Self::BinaryLength { parameter } => {
                        write!(f, "{parameter}: value is not exactly as wide as the field")
                    }
                }
            }
        }

        impl core::error::Error for EncodeError {}

        /// Why a packet could not be decoded.
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        pub enum DecodeError {
            /// The packet is shorter than the container it was read as.
            TooShort {
                /// Bytes the container needs.
                needed: usize,
            },
            /// No container's restriction criteria hold.
            Unrecognized,
            /// More than one container's restriction criteria hold.
            Ambiguous,
            /// A field's bytes are not valid text.
            InvalidText {
                /// The parameter, named as the definition spells it.
                parameter: &'static str,
            },
            /// A terminated string has no terminator inside its buffer.
            UnterminatedString {
                /// The parameter, named as the definition spells it.
                parameter: &'static str,
            },
            /// A length-prefixed string declares a length its buffer cannot hold.
            BadStringLength {
                /// The parameter, named as the definition spells it.
                parameter: &'static str,
            },
            /// An enumerated field holds a value the definition has no label for.
            UnknownLabel {
                /// The parameter, named as the definition spells it.
                parameter: &'static str,
            },
            /// A spline calibrator was asked for a value outside its points, and the
            /// definition does not allow extrapolation.
            Calibration {
                /// The parameter, named as the definition spells it.
                parameter: &'static str,
            },
        }

        impl core::fmt::Display for DecodeError {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                match self {
                    Self::TooShort { needed } => {
                        write!(f, "packet is shorter than the {needed} byte(s) the container needs")
                    }
                    Self::Unrecognized => write!(f, "no container's restriction criteria hold"),
                    Self::Ambiguous => {
                        write!(f, "more than one container's restriction criteria hold")
                    }
                    Self::InvalidText { parameter } => {
                        write!(f, "{parameter}: bytes are not valid text")
                    }
                    Self::UnterminatedString { parameter } => {
                        write!(f, "{parameter}: termination character not found")
                    }
                    Self::BadStringLength { parameter } => {
                        write!(f, "{parameter}: leading size is larger than the buffer")
                    }
                    Self::UnknownLabel { parameter } => {
                        write!(f, "{parameter}: no label for this value")
                    }
                    Self::Calibration { parameter } => write!(
                        f,
                        "{parameter}: query point falls outside the spline points and \
                         extrapolate is false"
                    ),
                }
            }
        }

        impl core::error::Error for DecodeError {}
    }
}
