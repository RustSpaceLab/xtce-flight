//! Generating the check as well as the code.
//!
//! A generated encoder is only worth as much as the evidence that it is right, and that
//! evidence is awkward to write by hand: every container is a different `struct`, so a test
//! that fills one in cannot be reused for the next. Written by hand, the test also tends to
//! cover the fields whose shape the author was thinking about.
//!
//! So the harness is generated too, from the same layout. For each container it produces a
//! function that draws a value for every field from a seeded generator, encodes it, and
//! reports **the values it drew** alongside the bytes.
//!
//! What keeps this from being circular: the harness never asks the encoder what a value
//! means. It reports the value it put in, and the comparison is against an independent
//! decoder — `xtce-decode`, which is itself checked against the Python reference. The
//! harness only has to be able to invent a number, not to know what it should look like on
//! the wire.
//!
//! Everything here is `std`, and belongs in a test crate. It is deliberately emitted to a
//! separate file so that the flight code stays free of it.

use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};
use xtce_codegen::plan::{TextCharset, TextDelimiter};
use xtce_model::types::IntegerCoding;

use crate::layout::{Container, EnumType, Kind, Layout, mask_for, natural_bits};

/// Renders a test harness for `layout`, calling into the flight module at `module_path`.
///
/// `module_path` is spliced into the generated source as written, so `super::flight` and
/// `crate::telemetry` are both fine.
#[must_use]
pub fn generate(layout: &Layout, module_path: &str) -> String {
    let path: TokenStream = module_path
        .parse()
        .unwrap_or_else(|_| quote!(super::flight));

    let cases = layout
        .containers
        .iter()
        .map(|container| case(container, &layout.enums, &path));
    let calls = layout
        .containers
        .iter()
        .enumerate()
        .map(|(index, container)| {
            let name = format_ident!("case_{}", snake(&container.type_ident));
            let salt =
                Literal::u64_unsuffixed(0x9E37_79B9_7F4A_7C15u64.wrapping_mul(index as u64 + 1));
            quote! { #name(seed ^ #salt) }
        });

    let prelude = prelude();
    let tokens = quote! {
        #prelude
        #(#cases)*

        /// One case per container, all drawn from `seed`.
        #[must_use]
        pub fn cases(seed: u64) -> Vec<Case> {
            vec![#(#calls),*]
        }
    };

    match syn::parse2(tokens.clone()) {
        Ok(file) => prettyplease::unparse(&file),
        Err(_) => tokens.to_string(),
    }
}

/// The part of the harness that does not depend on the definition.
fn prelude() -> TokenStream {
    quote! {
        /// One encoded packet, and everything that is known about what should come back.
        #[derive(Clone, Debug)]
        pub struct Case {
            /// The container, named as the definition spells it.
            pub container: &'static str,
            /// The encoded packet.
            pub bytes: Vec<u8>,
            /// `(parameter, value that was encoded)`, for every field the caller set.
            pub expected: Vec<(&'static str, Expected)>,
            /// `(parameter, raw value, mask)` for every restriction criterion, which the
            /// encoder wrote rather than the caller.
            pub criteria: Vec<(&'static str, u64, u64)>,
            /// `(parameter, raw value, engineering value)` for every calibrated field.
            ///
            /// These are not in `expected`, because for a calibrated parameter the value
            /// that went in is the *raw* one and the value the ground reads is not. The
            /// engineering value is `None` when the accessor refused it — a spline asked
            /// outside its points — in which case the interpreter has to refuse too.
            pub calibrated: Vec<(&'static str, u64, Option<f64>)>,
        }

        /// A value as it went into the encoder.
        #[derive(Clone, Debug, PartialEq)]
        pub enum Expected {
            /// An unsigned integer.
            Unsigned(u64),
            /// A signed integer.
            Signed(i64),
            /// A float, widened to `f64` as the interpreter reports it.
            Float(f64),
            /// A boolean.
            Bool(bool),
            /// An enumeration label, spelled as the definition spells it.
            Label(&'static str),
            /// Text.
            Text(String),
            /// Raw bytes.
            Bytes(Vec<u8>),
        }

        /// `xorshift64*`: small, seedable, and the same sequence on every machine, which is
        /// what a failing case has to be to be worth reporting.
        pub struct Rng(u64);

        impl Rng {
            /// A generator for `seed`. Zero is not a usable state, so it is replaced.
            #[must_use]
            pub fn new(seed: u64) -> Self {
                Self(if seed == 0 { 0x2545_F491_4F6C_DD1D } else { seed })
            }

            /// The next 64 bits.
            pub fn next(&mut self) -> u64 {
                let mut state = self.0;
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                self.0 = state;
                state.wrapping_mul(0x2545_F491_4F6C_DD1D)
            }

            /// A number below `limit`, which must not be zero.
            pub fn below(&mut self, limit: usize) -> usize {
                if limit == 0 { 0 } else { (self.next() % limit as u64) as usize }
            }

            /// `count` arbitrary bytes.
            pub fn bytes(&mut self, count: usize) -> Vec<u8> {
                (0..count).map(|_| (self.next() & 0xFF) as u8).collect()
            }

            /// A binary16 bit pattern that is neither infinite nor a NaN.
            ///
            /// Those two are excluded because they do not round trip as values: every NaN
            /// encodes, but not to the same payload it came from, and comparing them would
            /// be testing the payload convention rather than the encoder. Subnormals are
            /// kept — they are the interesting half of binary16.
            pub fn finite_half(&mut self) -> u16 {
                let bits = (self.next() & 0xFFFF) as u16;
                if (bits >> 10) & 0x1F == 0x1F { bits & 0xF3FF } else { bits }
            }

            /// A binary32 bit pattern that is neither infinite nor a NaN.
            pub fn finite_f32_bits(&mut self) -> u32 {
                let bits = (self.next() & 0xFFFF_FFFF) as u32;
                if (bits >> 23) & 0xFF == 0xFF { bits & 0xFF7F_FFFF } else { bits }
            }

            /// A binary64 bit pattern that is neither infinite nor a NaN.
            pub fn finite_f64_bits(&mut self) -> u64 {
                let bits = self.next();
                if (bits >> 52) & 0x7FF == 0x7FF { bits & 0xFFEF_FFFF_FFFF_FFFF } else { bits }
            }

            /// Text of exactly `bytes` bytes, avoiding every sequence in `forbidden`.
            pub fn text_of_exactly(
                &mut self,
                bytes: usize,
                utf8: bool,
                forbidden: &[u8],
            ) -> String {
                let mut out = String::new();
                let mut written = 0usize;
                while written < bytes {
                    // Two-byte characters only where two bytes are left, so the field is
                    // filled exactly rather than one short.
                    let wide = utf8 && bytes - written >= 2 && self.next() % 4 == 0;
                    if wide {
                        out.push(MULTIBYTE[self.below(MULTIBYTE.len())]);
                        written += 2;
                    } else {
                        let candidate = PRINTABLE[self.below(PRINTABLE.len())];
                        if forbidden.contains(&(candidate as u8)) {
                            continue;
                        }
                        out.push(candidate);
                        written += 1;
                    }
                }
                out
            }

            /// Text of at most `bytes` bytes, sometimes empty.
            pub fn text_up_to(&mut self, bytes: usize, utf8: bool, forbidden: &[u8]) -> String {
                let length = self.below(bytes + 1);
                self.text_of_exactly(length, utf8, forbidden)
            }
        }

        /// One byte each, and none of them a plausible terminator.
        const PRINTABLE: &[char] = &[
            'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p',
            'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P',
            '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', ' ', '-', '_', '.', '/', '+',
        ];

        /// Two bytes each in UTF-8, so a field declared as UTF-8 is not only ever handed
        /// ASCII.
        const MULTIBYTE: &[char] = &['ą', 'ę', 'ó', 'ż', 'µ', '£', 'Ω', 'é'];
    }
}

// One function, because the pieces it builds all read from the same container and splitting
// them apart would mean threading five borrowed slices between them for no gain.
#[allow(clippy::too_many_lines)]
fn case(container: &Container, enums: &[EnumType], path: &TokenStream) -> TokenStream {
    let name = format_ident!("case_{}", snake(&container.type_ident));
    let type_name = format_ident!("{}", container.type_ident);
    let xtce_name = &container.xtce_name;
    let len = Literal::usize_unsuffixed(container.len_bytes);

    // Owned values first: a borrowed field points into one of these, so they have to
    // outlive the struct.
    let owned = container.fields.iter().filter_map(|field| {
        let binding = format_ident!("{}", field.ident);
        let bytes = field.bit_width as usize / 8;
        let bytes_literal = Literal::usize_unsuffixed(bytes);
        match &field.kind {
            Kind::Text { charset, delimiter } => {
                let utf8 = matches!(charset, TextCharset::Utf8);
                Some(match delimiter {
                    TextDelimiter::WholeBuffer => quote! {
                        let #binding: String = rng.text_of_exactly(#bytes_literal, #utf8, &[]);
                    },
                    TextDelimiter::TerminationChar(terminator) => {
                        let literals = terminator.iter().map(|byte| Literal::u8_unsuffixed(*byte));
                        let room =
                            Literal::usize_unsuffixed(bytes.saturating_sub(terminator.len()));
                        quote! {
                            let #binding: String =
                                rng.text_up_to(#room, #utf8, &[#(#literals),*]);
                        }
                    }
                    TextDelimiter::LeadingSize { size_in_bits } => {
                        let room = Literal::usize_unsuffixed(
                            bytes.saturating_sub(*size_in_bits as usize / 8),
                        );
                        quote! {
                            let #binding: String = rng.text_up_to(#room, #utf8, &[]);
                        }
                    }
                })
            }
            Kind::Binary => Some(quote! {
                let #binding: Vec<u8> = rng.bytes(#bytes_literal);
            }),
            _ => None,
        }
    });

    let scalars = container
        .fields
        .iter()
        .filter(|field| !field.kind.borrows())
        .map(|field| {
            let binding = format_ident!("{}", field.ident);
            let draw = draw(field.bit_width, &field.kind, enums, path);
            quote! { let #binding = #draw; }
        });

    let initialisers = container.fields.iter().map(|field| {
        let name = format_ident!("{}", field.ident);
        let binding = format_ident!("{}", field.ident);
        if field.kind.borrows() {
            quote! { #name: &#binding }
        } else {
            quote! { #name: #binding }
        }
    });

    // What the interpreter should report, keyed by the name the definition uses. A
    // calibrated field is excluded and handled below: what went in was a raw count, and what
    // comes back is what the calibrator makes of it.
    let expectations = container
        .fields
        .iter()
        .filter(|field| field.calibration.is_none())
        .map(|field| {
            let binding = format_ident!("{}", field.ident);
            let xtce = &field.xtce_name;
            let expected = match &field.kind {
                Kind::Unsigned => quote!(Expected::Unsigned(u64::from(#binding))),
                Kind::Signed(_) => quote!(Expected::Signed(i64::from(#binding))),
                Kind::Float16 | Kind::Float32 => quote!(Expected::Float(f64::from(#binding))),
                Kind::Float64 => quote!(Expected::Float(#binding)),
                Kind::Bool => quote!(Expected::Bool(#binding)),
                Kind::Enumerated(_) => quote!(Expected::Label(#binding.label())),
                Kind::Text { .. } => quote!(Expected::Text(#binding.clone())),
                Kind::Binary => quote!(Expected::Bytes(#binding.clone())),
            };
            quote! { (#xtce, #expected) }
        });

    let calibrated = container
        .fields
        .iter()
        .filter(|field| field.calibration.is_some())
        .map(|field| {
            let binding = format_ident!("{}", field.ident);
            let accessor = format_ident!("{}_calibrated", field.ident.trim_end_matches('_'));
            let xtce = &field.xtce_name;
            // The same reduction to `u64` the interpreter's raw value gets on the other
            // side, so the two are comparable without a second value enum.
            let raw = match &field.kind {
                Kind::Signed(_) => quote!(#binding as i64 as u64),
                Kind::Float16 | Kind::Float32 => quote!(f64::from(#binding).to_bits()),
                Kind::Float64 => quote!(#binding.to_bits()),
                _ => quote!(u64::from(#binding)),
            };
            quote! { (#xtce, #raw, packet.#accessor().ok()) }
        });

    let criteria = container.constants.iter().map(|constant| {
        let xtce = &constant.xtce_name;
        let raw = Literal::u64_unsuffixed(constant.raw);
        let mask = Literal::u64_unsuffixed(mask_for(constant.bit_width));
        quote! { (#xtce, #raw, #mask) }
    });

    let doc = format!("A random `{xtce_name}`, encoded, with the values that went in.");

    quote! {
        #[doc = #doc]
        #[must_use]
        pub fn #name(seed: u64) -> Case {
            let mut rng = Rng::new(seed);
            #(#owned)*
            #(#scalars)*

            let packet = #path::#type_name { #(#initialisers,)* };

            let mut bytes = vec![0u8; #len];
            let written = packet
                .encode(&mut bytes)
                .unwrap_or_else(|error| panic!("{}: encode failed: {error}", #xtce_name));
            assert_eq!(written, #len, "{}: encode wrote the wrong length", #xtce_name);

            // The generated decoder is not the oracle — the interpreter is — but a struct
            // that does not survive its own round trip is a bug worth catching here, next
            // to the values, rather than in the comparison.
            let returned = #path::#type_name::decode(&bytes)
                .unwrap_or_else(|error| panic!("{}: decode failed: {error}", #xtce_name));
            assert_eq!(returned, packet, "{}: does not survive its own round trip", #xtce_name);
            assert!(
                #path::#type_name::matches(&bytes),
                "{}: encoded packet does not satisfy its own criteria",
                #xtce_name
            );

            Case {
                container: #xtce_name,
                bytes,
                expected: vec![#(#expectations),*],
                criteria: vec![#(#criteria),*],
                calibrated: vec![#(#calibrated),*],
            }
        }
    }
}

/// The expression that draws one value for a field.
fn draw(width: u32, kind: &Kind, enums: &[EnumType], path: &TokenStream) -> TokenStream {
    let natural = natural_bits(width);
    let mask = Literal::u64_unsuffixed(mask_for(width));

    match kind {
        Kind::Unsigned => {
            let ty = format_ident!("u{natural}");
            quote!((rng.next() & #mask) as #ty)
        }
        Kind::Signed(IntegerCoding::TwosComplement | IntegerCoding::Unsigned) => {
            let ty = format_ident!("i{natural}");
            let shift = Literal::u32_unsuffixed(64 - width);
            // Sign-extend a random field-width pattern, so negatives are as likely as
            // positives and the extremes are reachable.
            quote!(((((rng.next() & #mask) << #shift) as i64) >> #shift) as #ty)
        }
        Kind::Signed(IntegerCoding::SignMagnitude | IntegerCoding::OnesComplement) => {
            let ty = format_ident!("i{natural}");
            let magnitude = Literal::u64_unsuffixed((1u64 << (width - 1)) - 1);
            // These two codings are one short of two's complement at the bottom, and the
            // encoder rejects anything outside that. Drawing a value it would reject would
            // be testing the harness, not the encoder.
            quote! {
                {
                    let magnitude = (rng.next() & #magnitude) as i64;
                    (if rng.next() & 1 == 0 { magnitude } else { -magnitude }) as #ty
                }
            }
        }
        Kind::Float16 => quote!(#path::half_to_f32(rng.finite_half())),
        Kind::Float32 => quote!(f32::from_bits(rng.finite_f32_bits())),
        Kind::Float64 => quote!(f64::from_bits(rng.finite_f64_bits())),
        Kind::Bool => quote!(rng.next() & 1 == 1),
        Kind::Enumerated(index) => {
            let (name, count) = match enums.get(*index) {
                Some(definition) => (
                    format_ident!("{}", definition.type_ident),
                    definition.variants.len(),
                ),
                None => (format_ident!("Unknown"), 1),
            };
            let arms = enums
                .get(*index)
                .map(|definition| {
                    definition
                        .variants
                        .iter()
                        .enumerate()
                        .map(|(at, (variant, _, _))| {
                            let at = Literal::usize_unsuffixed(at);
                            let variant = format_ident!("{variant}");
                            quote! { #at => #path::#name::#variant }
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let count = Literal::usize_unsuffixed(count);
            let first = arms
                .first()
                .cloned()
                .unwrap_or_else(|| quote!(0 => todo!()));
            let _ = first;
            quote! {
                match rng.below(#count) {
                    #(#arms,)*
                    _ => unreachable!("below({}) is in range", #count),
                }
            }
        }
        Kind::Text { .. } | Kind::Binary => quote!(unreachable!("drawn as an owned value")),
    }
}

/// `StatusReport` as `status_report`, for a function name.
fn snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (index, character) in name.char_indices() {
        if character.is_uppercase() && index != 0 {
            out.push('_');
        }
        out.extend(character.to_lowercase());
    }
    out
}
