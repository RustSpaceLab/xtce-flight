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
case!(calibrated, "calibrated");
case!(calibrated_bounded, "calibrated_bounded");
case!(context_calibrated, "context_calibrated");
case!(contrived, "contrived");
case!(byte_order, "byte_order");
case!(arrays, "arrays");
case!(aggregates, "aggregates");

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
                    .chain(case.calibrated.iter().map(|(parameter, _, _)| *parameter))
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

                // A calibrated parameter has two values, and they are checked against
                // different halves of what the interpreter reports: the raw one against what
                // was encoded, and the engineering one against the interpreter's own
                // calibration — an implementation this generator shares no code with.
                for (parameter, raw, engineering) in &case.calibrated {
                    let (actual, actual_raw) = seen.get(*parameter).unwrap_or_else(|| {
                        panic!("{label}: the interpreter did not report {parameter}")
                    });
                    assert_eq!(
                        actual_raw, raw,
                        "{label}: {parameter}: the raw value was not encoded correctly"
                    );
                    let engineering = engineering.unwrap_or_else(|| {
                        panic!(
                            "{label}: {parameter}: the calibrator refused a value the \
                             interpreter accepted"
                        )
                    });
                    match actual {
                        // By bit pattern. A calibrated value that is right to fourteen
                        // digits and wrong in the last bit is the failure worth catching.
                        Seen::Float(interpreted) => assert_eq!(
                            engineering.to_bits(),
                            interpreted.to_bits(),
                            "{label}: {parameter}: calibrated to {engineering}, interpreter \
                             read {interpreted}"
                        ),
                        other => panic!(
                            "{label}: {parameter}: a calibrated field should read as a \
                             float, not {other:?}"
                        ),
                    }
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

/// Calibration, against the interpreter, over seeded packets.
///
/// Encoding is untouched by a calibrator — a spacecraft has an ADC count, and turning counts
/// into engineering units is the ground's job — so what is under test here is the accessor,
/// against `xtce-decode`'s own calibration. Two implementations sharing no code, the same
/// input bits, compared on `to_bits()`.
#[test]
fn calibration_matches_the_interpreter_bit_for_bit() {
    let checked = check!(calibrated, "calibrated.xml", None::<&str>, 256u64);
    assert!(checked > 1_500, "only {checked} field(s) compared");
}

/// Context calibration, against the interpreter, with every branch taken.
///
/// A `<ContextCalibratorList>` gives one parameter several calibrators and picks between them
/// by what the rest of the packet says. Encoding is untouched by that too, so what is under
/// test is again the accessor — but an accessor that is now an else-if chain, and a
/// comparison is only worth what the packets exercised.
///
/// That is the second half of this test. The harness invents a uniformly random value for
/// every field, so a criterion on an eight-bit field would hold in one packet in 256: the
/// default branch would be checked thousands of times and the contexts twice, and the test
/// would pass whatever the chain did with them. The fields the criteria test are narrow for
/// that reason, and the counts below fail loudly if a change to the definition, the seed or
/// the harness quietly stops reaching a branch.
#[test]
fn context_calibrators_match_the_interpreter() {
    let checked = check!(
        context_calibrated,
        "context_calibrated.xml",
        None::<&str>,
        256u64
    );
    assert!(checked > 2_000, "only {checked} field(s) compared");

    use context_calibrated::harness::Expected;

    let mut modes = [0usize; 4];
    let mut valid = 0usize;
    // SELF's context tests SELF itself, and LOOKAHEAD's tests a parameter decoded after it,
    // which resolves to LOOKAHEAD's own raw value. Both are counted from the raw value the
    // harness reports, which is the value the criterion compares.
    let mut self_above = 0usize;
    let mut lookahead_hit = 0usize;

    for round in 0..256u64 {
        // The same seeds `check!` used, so these are the packets that were compared.
        let seed = 0x2545_F491_4F6C_DD1Du64.wrapping_mul(round + 1) ^ (round << 32);
        for case in context_calibrated::harness::cases(seed) {
            for (parameter, value) in &case.expected {
                match (*parameter, value) {
                    ("MODE", Expected::Unsigned(mode)) => {
                        modes[usize::try_from(*mode).expect("MODE is two bits")] += 1;
                    }
                    ("VALID", Expected::Bool(true)) => valid += 1,
                    _ => {}
                }
            }
            for (parameter, raw, _) in &case.calibrated {
                match *parameter {
                    "SELF" if *raw > 2048 => self_above += 1,
                    "LOOKAHEAD" if *raw == 5 => lookahead_hit += 1,
                    _ => {}
                }
            }
        }
    }

    for (mode, count) in modes.iter().enumerate() {
        assert!(
            *count > 20,
            "MODE was {mode} in only {count} packet(s), so a branch of SENSOR's chain went \
             untested"
        );
    }
    assert!(valid > 20, "VALID was set in only {valid} packet(s)");
    assert!(
        self_above > 20,
        "SELF was above its own threshold in only {self_above} packet(s)"
    );
    assert!(
        lookahead_hit > 5,
        "LOOKAHEAD's criterion held in only {lookahead_hit} packet(s)"
    );
}

/// A criterion naming a parameter decoded later reads the field being calibrated.
///
/// The surprising rule, driven deliberately rather than left to the seeds. LOOKAHEAD's
/// context tests `LATER == 5`, and LATER sits *after* LOOKAHEAD in the container. A criterion
/// compares what has been decoded so far, so LATER is not there to compare and what the
/// reference reaches for instead is the raw value of the field being calibrated. The
/// definition says `LATER`; the comparison is against LOOKAHEAD.
///
/// Two packets, chosen so that a generator following the parameter's *name* gives the
/// opposite answer on both. The interpreter is asked as well, because agreeing with it is the
/// point — but the assertions on the values stand on their own, so this test still says what
/// it means if the two implementations ever drift together.
#[test]
fn a_criterion_on_a_later_parameter_reads_the_field_being_calibrated() {
    use context_calibrated::flight::{Flags, Telemetry};

    let db = XtceDb::from_path(testdata("context_calibrated.xml")).expect("definition loads");
    let decoder = Decoder::new(&db).expect("root container");

    let template = Telemetry {
        mode: 0,
        flags: Flags::Idle,
        valid: false,
        spare_3: 0,
        sensor: 0,
        armed: 0,
        self_: 0,
        lookahead: 0,
        spline_ctx: 0,
        later: 0,
        plain: 0,
    };

    // `(LOOKAHEAD, LATER, what the context calibrator makes of it)`. The context applies
    // 7.0; the default is the raw value itself.
    let cases = [
        (
            5u8,
            0u8,
            7.0f64,
            "the criterion holds on LOOKAHEAD's own value",
        ),
        (0, 5, 0.0, "LATER is 5, but LATER is not what is compared"),
    ];

    for (lookahead, later, expected, what) in cases {
        let packet = Telemetry {
            lookahead,
            later,
            ..template
        };
        let mut buffer = [0u8; Telemetry::LEN];
        packet.encode(&mut buffer).expect("every value fits");

        let decoded = Telemetry::decode(&buffer).expect("what was just encoded decodes");
        let value = decoded
            .lookahead_calibrated()
            .expect("a polynomial never fails");
        assert_eq!(
            value.to_bits(),
            expected.to_bits(),
            "{what}: LOOKAHEAD {lookahead}, LATER {later} calibrated to {value}"
        );

        let (_, seen) = interpret(&db, &decoder, &buffer);
        let (interpreted, _) = seen.get("LOOKAHEAD").expect("the interpreter reports it");
        match interpreted {
            Seen::Float(interpreted) => assert_eq!(
                value.to_bits(),
                interpreted.to_bits(),
                "{what}: the interpreter read {interpreted}"
            ),
            other => panic!("{what}: the interpreter read {other:?}"),
        }
    }
}

/// The integral and floating-point power paths are not interchangeable.
///
/// `calibrated.xml` gives `POLY_U32` and `POLY_F64` byte-for-byte identical terms over
/// different encodings. Fed the same number they must still disagree in the last bit: an
/// integral raw value is cubed exactly and rounded once, a float raw by repeated squaring,
/// which rounds twice.
///
/// The test above would not catch a generator that collapsed them, because it never feeds
/// the two fields the same number — each is compared against the interpreter separately, and
/// the interpreter would be wrong in the same way only if it had the same bug.
#[test]
fn the_integer_power_path_is_not_the_float_one() {
    use calibrated::flight::Telemetry;

    // 2^27 + 1. Its cube needs 82 bits, so rounding it once is not the same as rounding the
    // square and then the product.
    const VALUE: u32 = (1 << 27) + 1;

    let mut packet = vec![0u8; Telemetry::LEN];
    packet[0..4].copy_from_slice(&VALUE.to_be_bytes());
    packet[4..12].copy_from_slice(&f64::from(VALUE).to_bits().to_be_bytes());

    let decoded = Telemetry::decode(&packet).expect("the packet decodes");
    let integral = decoded
        .poly_u32_calibrated()
        .expect("a polynomial never fails");
    let floating = decoded
        .poly_f64_calibrated()
        .expect("a polynomial never fails");

    assert_ne!(
        integral.to_bits(),
        floating.to_bits(),
        "the same value through the integral and floating-point paths came out identical, \
         so the emitter is using one path for both"
    );
    // A last-bit disagreement, not a wrong answer.
    assert!(
        (integral - floating).abs() / integral.abs() < 1e-15,
        "the two paths differ by more than rounding: {integral} against {floating}"
    );
}

/// A spline that may not extrapolate refuses, rather than inventing a value.
///
/// Driven deliberately rather than through the loop above: this is the only thing on the
/// decode side that can fail for a packet that is otherwise perfectly well formed, and it has
/// to fail on exactly the values the interpreter fails on — not one either side.
#[test]
fn a_bounded_spline_refuses_outside_its_points() {
    use calibrated_bounded::flight::{Bounded, DecodeError};

    let db = XtceDb::from_path(testdata("calibrated_bounded.xml")).expect("definition loads");
    let decoder = Decoder::new(&db).expect("root container");

    let mut refused = 0usize;
    let mut answered = 0usize;

    for raw in 0..=u8::MAX {
        let packet = [raw];
        let decoded = Bounded::decode(&packet).expect("one byte always decodes");
        let generated = decoded.bounded_calibrated();

        let mut interpreted = decoder.new_packet(&packet);
        let by_interpreter = decoder.decode_into(&mut interpreted, &packet);

        match (&generated, by_interpreter) {
            (Err(_), Err(_)) => {
                refused += 1;
                assert!(
                    !(50..=200).contains(&raw),
                    "raw {raw} is inside the spline's points but was refused"
                );
                assert_eq!(
                    generated,
                    Err(DecodeError::Calibration {
                        parameter: "BOUNDED"
                    })
                );
            }
            (Ok(value), Ok(())) => {
                answered += 1;
                assert!(
                    (50..=200).contains(&raw),
                    "raw {raw} is outside the spline's points but got an answer"
                );
                let (_, seen) = interpret(&db, &decoder, &packet);
                let (interpreted_value, _) =
                    seen.get("BOUNDED").expect("the interpreter reports it");
                match interpreted_value {
                    Seen::Float(expected) => assert_eq!(
                        value.to_bits(),
                        expected.to_bits(),
                        "raw {raw}: calibrated to {value}, interpreter read {expected}"
                    ),
                    other => panic!("raw {raw}: interpreter read {other:?}"),
                }
            }
            (generated, interpreted_result) => panic!(
                "raw {raw}: the two implementations disagree about whether it calibrates — \
                 generated {generated:?}, interpreter {interpreted_result:?}"
            ),
        }
    }

    // 50 to 200 inclusive is 151 of the 256 values, and the rest have no answer.
    assert_eq!(answered, 151);
    assert_eq!(refused, 105);
}

/// A container selected by a `<BooleanExpression>` survives the round trip.
///
/// `contrived_inheritance_structure.xml` is the only real definition in reach whose criteria
/// are conditions rather than comparisons. Nothing about encoding changes — a conjunction of
/// equalities is a conjunction of equalities however the XML spells it — but that is a claim
/// worth checking rather than asserting, because if `encode` failed to write one of the
/// conditions the interpreter would not recognise the packet at all.
#[test]
fn a_container_selected_by_conditions_round_trips() {
    let checked = check!(
        contrived,
        "contrived_inheritance_structure.xml",
        None::<&str>,
        128u64
    );
    assert!(checked > 2_000, "only {checked} field(s) compared");
}

/// Little-endian fields survive the round trip, including the criterion that selects them.
///
/// For a decoder `leastSignificantByteFirst` is an operation to apply; for an encoder it is
/// one to *invert*. Reversing bytes is its own inverse, so a whole-byte field costs nothing
/// — but `SEL` is the interesting one: the criterion compares the value *after* the
/// reversal, so `encode` has to write the reversal undone or the interpreter will not
/// recognise the packet at all. If it got that wrong, every packet here would fail to decode
/// rather than decode to the wrong number.
#[test]
fn little_endian_fields_survive_the_interpreter() {
    let checked = check!(byte_order, "byte_order.xml", None::<&str>, 256u64);
    assert!(checked > 3_000, "only {checked} field(s) compared");
}

/// Arrays survive the round trip, and needed nothing of the encoder.
///
/// An array entry is expanded into one parameter per element when the definition loads, so by
/// the time this generator runs there are no arrays — only twenty-seven fields with names
/// like `TEMPS[3]`. That is the claim, and it is worth a test rather than a comment: if the
/// expansion produced fields the encoder placed differently from where the interpreter reads
/// them, every packet here would come back wrong.
///
/// Note the nibble array in the middle. Its elements are four bits, so they are not
/// byte-aligned and neither is anything after them until the array ends — the case where an
/// expansion that quietly rounded to bytes would show up.
#[test]
fn arrays_survive_the_interpreter() {
    let checked = check!(arrays, "arrays.xml", None::<&str>, 256u64);
    assert!(checked > 6_000, "only {checked} field(s) compared");
}

/// Aggregates survive the round trip, and needed nothing of the encoder either.
///
/// Same claim as arrays and the same reason it is worth testing rather than asserting: by the
/// time this generator runs there are no aggregates, only seventeen fields with names like
/// `STATE.samples[2]`. The fixture nests the two in both directions and ends its aggregate on
/// a four-bit member, so nothing after it is byte-aligned until the pad — an expansion that
/// rounded a member up to a byte would move every field behind it.
#[test]
fn aggregates_survive_the_interpreter() {
    let checked = check!(aggregates, "aggregates.xml", None::<&str>, 256u64);
    assert!(checked > 4_000, "only {checked} field(s) compared");
}
