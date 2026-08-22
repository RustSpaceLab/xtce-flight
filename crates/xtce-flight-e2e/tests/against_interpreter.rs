//! The generated encoder, checked against an independent decoder.
//!
//! # Why this is the proof
//!
//! Encoding and decoding with the same generated code would prove only that the generator is
//! consistent with itself; a field written at the wrong offset and read back from the wrong
//! offset round trips perfectly. So the packet goes out through `xtce-flight` and comes back
//! through `xtce-decode`, which shares no code with it and is already checked against
//! `space_packet_parser` on roughly 17 000 real packets. Agreeing with it is agreeing with
//! the reference implementation.
//!
//! What goes in is a value the harness invented, and what is compared is that same value —
//! never something derived from the encoder. A bug in the encoder therefore cannot make the
//! comparison agree with it.
//!
//! Neither the flight code nor the harness is committed: `build.rs` writes both.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use xtce_decode::{Decoder, EngValue, RawValue};
use xtce_model::XtceDb;

macro_rules! case {
    ($name:ident, $file:literal) => {
        mod $name {
            #[allow(dead_code, clippy::all, clippy::pedantic)]
            pub mod flight {
                include!(concat!(env!("OUT_DIR"), "/", $file, ".rs"));
            }
            #[allow(dead_code, clippy::all, clippy::pedantic)]
            pub mod harness {
                include!(concat!(env!("OUT_DIR"), "/", $file, "_harness.rs"));
            }
        }
    };
}

case!(numeric_edges, "numeric_edges");
case!(flight_shapes, "flight_shapes");
case!(jpss, "jpss");

fn testdata(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata")
        .join(relative)
}

/// What the interpreter reported for one parameter, reduced to a comparable shape.
#[derive(Debug)]
enum Seen {
    Unsigned(u64),
    Signed(i64),
    Float(f64),
    Bool(bool),
    Label(String),
    Text(String),
    Bytes(Vec<u8>),
}

/// Decodes `bytes` with the interpreter and indexes the result by parameter name.
///
/// The last value wins where a parameter appears twice, which is what the reference does:
/// its output is a dictionary, so a repeated entry overwrites the earlier one.
fn interpret(
    db: &XtceDb,
    decoder: &Decoder<'_>,
    bytes: &[u8],
) -> (String, HashMap<String, (Seen, u64)>) {
    let mut packet = decoder.new_packet(bytes);
    decoder
        .decode_into(&mut packet, bytes)
        .unwrap_or_else(|error| panic!("interpreted decode failed: {error}"));

    let container = db
        .container(packet.container())
        .map(|container| db.name(container.name).to_owned())
        .expect("container resolves");

    let mut values = HashMap::new();
    for value in packet.values() {
        let name = db
            .parameter(value.parameter)
            .map(|parameter| db.name(parameter.name).to_owned())
            .expect("parameter resolves");

        let eng = match &value.eng {
            EngValue::Unsigned(number) => Seen::Unsigned(*number),
            EngValue::Signed(number) => Seen::Signed(*number),
            EngValue::Float(number) => Seen::Float(*number),
            EngValue::Bool(flag) => Seen::Bool(*flag),
            EngValue::Label(label) => Seen::Label((*label).to_string()),
            EngValue::Text(text) => Seen::Text(text.as_ref().to_owned()),
            EngValue::Bytes(bytes) => Seen::Bytes(bytes.as_ref().to_vec()),
        };

        // Criteria are checked against the raw value, because a criterion tests bits and the
        // parameter carrying them may be signed, boolean or enumerated.
        let raw = match &value.raw {
            RawValue::Unsigned(number) => *number,
            RawValue::Signed(number) => *number as u64,
            RawValue::Float(number) => number.to_bits(),
            RawValue::Bytes(_) => 0,
        };

        values.insert(name, (eng, raw));
    }
    (container, values)
}

/// Runs every case a definition's harness produces, over `rounds` seeds.
macro_rules! check {
    ($module:ident, $definition:literal, $root:expr, $rounds:expr) => {{
        use $module::harness::Expected;

        let db = XtceDb::from_path(testdata($definition)).expect("definition loads");
        let decoder = match $root {
            Some(name) => Decoder::with_root(&db, name),
            None => Decoder::new(&db),
        }
        .expect("root container");

        let mut checked = 0usize;
        for round in 0..$rounds {
            // A different seed each round, from a fixed start: reproducible, but not the
            // same packet a thousand times.
            let seed = 0x2545_F491_4F6C_DD1Du64.wrapping_mul(round + 1) ^ (round << 32);
            for case in $module::harness::cases(seed) {
                let label = format!("{} round {round}", case.container);
                let (container, seen) = interpret(&db, &decoder, &case.bytes);
                assert_eq!(
                    container, case.container,
                    "{label}: the interpreter chose a different container, so the encoder \
                     did not write the restriction criteria correctly"
                );

                // Name-by-name lookup alone cannot see a field that went missing: the
                // struct would not have it, the harness would not expect it, `encode` would
                // leave its bits zero, and every comparison below would still pass. So the
                // two sets are compared as sets first.
                let mut claimed: Vec<&str> = case
                    .expected
                    .iter()
                    .map(|(parameter, _)| *parameter)
                    .chain(case.criteria.iter().map(|(parameter, _, _)| *parameter))
                    .collect();
                claimed.sort_unstable();
                let mut reported: Vec<&str> = seen.keys().map(String::as_str).collect();
                reported.sort_unstable();
                assert_eq!(
                    claimed, reported,
                    "{label}: the parameters the encoder writes are not the parameters the \
                     definition puts in this container"
                );

                for (parameter, raw, mask) in &case.criteria {
                    let (_, actual) = seen.get(*parameter).unwrap_or_else(|| {
                        panic!("{label}: the interpreter did not report {parameter}")
                    });
                    assert_eq!(
                        actual & mask,
                        *raw,
                        "{label}: {parameter}: restriction criterion was not written"
                    );
                }

                for (parameter, expected) in &case.expected {
                    let (actual, _) = seen.get(*parameter).unwrap_or_else(|| {
                        panic!("{label}: the interpreter did not report {parameter}")
                    });
                    let same = match (expected, actual) {
                        (Expected::Unsigned(a), Seen::Unsigned(b)) => a == b,
                        (Expected::Signed(a), Seen::Signed(b)) => a == b,
                        // By bit pattern: both sides read the same bits, so anything short
                        // of exact equality is a real difference.
                        (Expected::Float(a), Seen::Float(b)) => a.to_bits() == b.to_bits(),
                        (Expected::Bool(a), Seen::Bool(b)) => a == b,
                        (Expected::Label(a), Seen::Label(b)) => a == b,
                        (Expected::Text(a), Seen::Text(b)) => a == b,
                        (Expected::Bytes(a), Seen::Bytes(b)) => a == b,
                        _ => false,
                    };
                    assert!(
                        same,
                        "{label}: {parameter}: encoded {expected:?}, interpreter read {actual:?}"
                    );
                    checked += 1;
                }
            }
        }
        checked
    }};
}

#[test]
fn every_numeric_shape_survives_the_interpreter() {
    // The point of this definition: a 16-bit float, a 64-bit float four bits off a byte
    // boundary, and all three signed codings, none of which the mission files contain.
    let checked = check!(numeric_edges, "numeric_edges.xml", None::<&str>, 256u64);
    assert!(checked > 5_000, "only {checked} field(s) compared");
}

#[test]
fn inheritance_enumerations_and_strings_survive_the_interpreter() {
    let checked = check!(flight_shapes, "flight_shapes.xml", None::<&str>, 256u64);
    assert!(checked > 5_000, "only {checked} field(s) compared");
}

#[test]
fn a_real_mission_definition_survives_the_interpreter() {
    let checked = check!(jpss, "jpss1_geolocation_xtce_v1.xml", None::<&str>, 256u64);
    assert!(checked > 5_000, "only {checked} field(s) compared");
}

/// A value one bit too wide for its field is refused, not truncated.
///
/// The failure this guards against does not look like a failure: the packet is the right
/// length, every other field is right, and one number is quietly wrong.
#[test]
fn a_value_that_does_not_fit_is_refused() {
    use flight_shapes::flight::{Baud, Beacon, EncodeError};

    let mut buffer = [0u8; Beacon::LEN];
    let mut beacon = Beacon {
        type_: 0,
        sec_hdr_flag: 0,
        seq_flgs: 0,
        seq_ctr: 0,
        pkt_len: 0,
        flag_a: 0,
        small: 0,
        wide_odd: 0,
        ones_odd: 0,
        signmag_odd: 0,
        f32_odd: 0.0,
        f64_odd: 0.0,
        f16_odd: 0.0,
        baud: Baud::C9600,
        tail_pad: 0,
        pad_5: 0,
    };
    assert!(
        beacon.encode(&mut buffer).is_ok(),
        "the control case encodes"
    );

    // SMALL is seven bits, so 127 is the largest value it can hold.
    beacon.small = 127;
    assert!(beacon.encode(&mut buffer).is_ok());
    beacon.small = 128;
    assert_eq!(
        beacon.encode(&mut buffer),
        Err(EncodeError::OutOfRange { parameter: "SMALL" })
    );

    // Ones' complement spends a bit on the sign and has two zeros, so it is one short of
    // two's complement at the bottom: i16::MIN does not fit a 16-bit ones'-complement field.
    beacon.small = 0;
    beacon.ones_odd = i16::MIN;
    assert_eq!(
        beacon.encode(&mut buffer),
        Err(EncodeError::OutOfRange {
            parameter: "ONES_ODD"
        })
    );
    beacon.ones_odd = i16::MIN + 1;
    assert!(beacon.encode(&mut buffer).is_ok());
}

/// A buffer that is too small is an error rather than a partial packet.
#[test]
fn a_short_buffer_is_refused() {
    use flight_shapes::flight::{Beacon, EncodeError};

    let beacon = Beacon {
        type_: 0,
        sec_hdr_flag: 0,
        seq_flgs: 0,
        seq_ctr: 0,
        pkt_len: 0,
        flag_a: 0,
        small: 0,
        wide_odd: 0,
        ones_odd: 0,
        signmag_odd: 0,
        f32_odd: 0.0,
        f64_odd: 0.0,
        f16_odd: 0.0,
        baud: flight_shapes::flight::Baud::C9600,
        tail_pad: 0,
        pad_5: 0,
    };
    let mut buffer = vec![0u8; Beacon::LEN - 1];
    assert_eq!(
        beacon.encode(&mut buffer),
        Err(EncodeError::TooShort {
            needed: Beacon::LEN
        })
    );
}

/// The string rules, which are where an encoder and a decoder most easily disagree.
#[test]
fn string_fields_have_to_fit_exactly_or_be_terminated() {
    use flight_shapes::flight::{EncodeError, Mode, StatusReport};

    let mut buffer = [0u8; StatusReport::LEN];
    let mut report = StatusReport {
        type_: 0,
        sec_hdr_flag: 0,
        seq_flgs: 0,
        seq_ctr: 0,
        pkt_len: 0,
        mode: Mode::Nominal,
        heater_on: true,
        spare_4: 0,
        build_id: "v1.2.3xy",
        label: "hello",
        note: "note",
        blob: &[1, 2, 3, 4, 5, 6, 7, 8],
        temp: -40,
        count: 7,
    };
    assert!(
        report.encode(&mut buffer).is_ok(),
        "the control case encodes"
    );

    // BUILD_ID is the whole buffer, eight bytes of it, so it has to be filled exactly.
    report.build_id = "short";
    assert_eq!(
        report.encode(&mut buffer),
        Err(EncodeError::TextLength {
            parameter: "BUILD_ID"
        })
    );
    report.build_id = "v1.2.3xy";

    // LABEL is terminated by a NUL, which therefore cannot appear inside it.
    report.label = "he\0llo";
    assert_eq!(
        report.encode(&mut buffer),
        Err(EncodeError::EmbeddedTerminator { parameter: "LABEL" })
    );
    report.label = "hello";

    // NOTE is US-ASCII, so a character outside it is refused rather than mangled.
    report.note = "ąę";
    assert_eq!(
        report.encode(&mut buffer),
        Err(EncodeError::InvalidText { parameter: "NOTE" })
    );
    report.note = "note";

    report.blob = &[1, 2, 3];
    assert_eq!(
        report.encode(&mut buffer),
        Err(EncodeError::BinaryLength { parameter: "BLOB" })
    );
}

/// Decoded strings and binaries point into the caller's buffer.
///
/// This is the property that makes the generated decoder usable where there is no
/// allocator, so it is worth an assertion rather than a comment.
#[test]
fn decoded_text_borrows_from_the_packet() {
    use flight_shapes::flight::StatusReport;

    let case = &flight_shapes::harness::case_status_report(1);
    let decoded = StatusReport::decode(&case.bytes).expect("decodes");

    let packet = case.bytes.as_ptr() as usize;
    let end = packet + case.bytes.len();
    for pointer in [
        decoded.build_id.as_ptr() as usize,
        decoded.label.as_ptr() as usize,
        decoded.note.as_ptr() as usize,
        decoded.blob.as_ptr() as usize,
    ] {
        assert!(
            (packet..end).contains(&pointer),
            "a decoded value was copied instead of borrowed"
        );
    }
}

/// Two containers, and the dispatcher picks the one whose criteria hold.
#[test]
fn the_dispatcher_chooses_by_restriction_criteria() {
    use flight_shapes::flight::{DecodeError, Packet, decode};

    let report = flight_shapes::harness::case_status_report(7);
    let beacon = flight_shapes::harness::case_beacon(7);

    assert!(matches!(
        decode(&report.bytes).expect("a StatusReport decodes"),
        Packet::StatusReport(_)
    ));
    assert!(matches!(
        decode(&beacon.bytes).expect("a Beacon decodes"),
        Packet::Beacon(_)
    ));

    // APID 999 belongs to neither.
    let mut stranger = beacon.bytes.clone();
    stranger[0] = 0x03;
    stranger[1] = 0xE7;
    assert_eq!(decode(&stranger), Err(DecodeError::Unrecognized));
}

/// Every binary16 encoding, both ways, against the exact value of the format.
///
/// This is here because it is the one conversion the generator performs that is not a shift
/// and a mask, and because nothing else would catch it: no mission definition in reach has a
/// 16-bit float, so a wrong widening would pass the differential test above by never being
/// exercised. It also caught a real off-by-one while this crate was being written — a
/// subnormal whose fraction had more than one bit set came back an octave low.
#[test]
fn binary16_round_trips_through_every_encoding() {
    use numeric_edges::flight::{f32_to_half, half_to_f32};

    /// The value a binary16 encoding denotes, from the definition of the format rather than
    /// from any implementation of it.
    fn reference(bits: u16) -> f64 {
        let sign = if bits & 0x8000 == 0 { 1.0 } else { -1.0 };
        let exponent = i32::from((bits >> 10) & 0x1F);
        let fraction = f64::from(bits & 0x03FF);
        match exponent {
            0 => sign * fraction * 2f64.powi(-24),
            _ => sign * (1.0 + fraction / 1024.0) * 2f64.powi(exponent - 15),
        }
    }

    let mut subnormals = 0usize;
    for bits in 0..=u16::MAX {
        // Infinities and NaNs are excluded on purpose: every NaN encodes to *a* NaN, but not
        // to the same payload, so a round trip over them tests a convention rather than the
        // arithmetic.
        if (bits >> 10) & 0x1F == 0x1F {
            continue;
        }
        if (bits >> 10) & 0x1F == 0 && bits & 0x03FF != 0 {
            subnormals += 1;
        }

        let widened = half_to_f32(bits);
        assert_eq!(
            f64::from(widened),
            reference(bits),
            "half_to_f32({bits:#06x}) is not the value the format defines"
        );
        assert_eq!(
            f32_to_half(widened),
            bits,
            "f32_to_half did not return {bits:#06x} to where it came from"
        );
    }
    assert_eq!(
        subnormals, 2046,
        "every subnormal encoding should have been covered"
    );
}

/// Narrowing rounds to nearest, ties to even, as IEEE-754 requires.
#[test]
fn binary16_narrowing_rounds_to_nearest_even() {
    use numeric_edges::flight::{f32_to_half, half_to_f32};

    // Halfway between the two smallest normals, 2^-14 and 2^-14 * (1 + 1/1024): the tie
    // goes to the even encoding, which is the lower one.
    let low = half_to_f32(0x0400);
    let high = half_to_f32(0x0401);
    assert_eq!(f32_to_half((low + high) / 2.0), 0x0400);

    // The same halfway step one encoding up, where the even neighbour is the higher one.
    let low = half_to_f32(0x0401);
    let high = half_to_f32(0x0402);
    assert_eq!(f32_to_half((low + high) / 2.0), 0x0402);

    // Above the largest finite binary16, 65504, narrowing saturates to infinity rather than
    // wrapping to something small.
    assert_eq!(f32_to_half(70000.0), 0x7C00);
    assert_eq!(f32_to_half(-70000.0), 0xFC00);

    // Below the smallest subnormal, it flushes to a signed zero.
    assert_eq!(f32_to_half(1e-30), 0x0000);
    assert_eq!(f32_to_half(-1e-30), 0x8000);
}
