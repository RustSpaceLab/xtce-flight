//! Proof, rather than a claim, that the generated code belongs on a flight computer.
//!
//! Three properties, each checked by building rather than by asserting:
//!
//! * **`no_std`.** The crate attribute below is the check. Nothing generated may name
//!   `std`, `alloc`, `String` or `Vec`, or this does not compile.
//! * **No `unsafe`.** `forbid` rather than `deny`, so the generated code cannot opt back in.
//! * **No panicking branch.** Every `encode` and `decode` reaches this crate's public
//!   surface through the functions below, and `scripts/check-no-panic.sh` fails if
//!   `core::panicking` appears anywhere in the emitted LLVM IR. That is a stronger statement
//!   than "we do not call `unwrap`": a single slice index would put a panic path in the
//!   binary, and on a part with no unwinder and no console, a panic is a silent reset.
//!
//! Built for `thumbv7em-none-eabihf` — Cortex-M4F, the class of part that flies as a payload
//! or bus controller — but the properties are the target's business, not this crate's.

#![no_std]
#![forbid(unsafe_code)]
#![deny(clippy::all, clippy::pedantic)]

#[allow(dead_code, clippy::all, clippy::pedantic)]
pub mod numeric_edges {
    include!(concat!(env!("OUT_DIR"), "/numeric_edges.rs"));
}

#[allow(dead_code, clippy::all, clippy::pedantic)]
pub mod flight_shapes {
    include!(concat!(env!("OUT_DIR"), "/flight_shapes.rs"));
}

#[allow(dead_code, clippy::all, clippy::pedantic)]
pub mod jpss {
    include!(concat!(env!("OUT_DIR"), "/jpss.rs"));
}

#[allow(dead_code, clippy::all, clippy::pedantic)]
pub mod calibrated {
    include!(concat!(env!("OUT_DIR"), "/calibrated.rs"));
}

#[allow(dead_code, clippy::all, clippy::pedantic)]
pub mod byte_order {
    include!(concat!(env!("OUT_DIR"), "/byte_order.rs"));
}

/// Encodes a `NumericEdges` packet, exercising every numeric shape the emitter produces.
#[inline(never)]
pub fn encode_numeric_edges(out: &mut [u8]) -> usize {
    let packet = numeric_edges::NumericEdges {
        pad_4: 1,
        f16_unaligned: 1.5,
        f32_unaligned: 2.5,
        f64_unaligned: 3.5,
        s16_twos_unaligned: -1,
        s16_signmag_unaligned: -2,
        s16_ones_unaligned: -3,
        u64_unaligned: u64::MAX,
        u57_unaligned: 1,
        s63_twos_unaligned: -4,
        pad_4b: 2,
        f16_aligned: 4.5,
        f32_aligned: 5.5,
        f64_aligned: 6.5,
        s16_twos_aligned: -5,
        s16_signmag_aligned: -6,
        s16_ones_aligned: -7,
        u64_aligned: 9,
        s63_twos_aligned: -8,
        pad_1: 1,
    };
    packet.encode(out).unwrap_or(0)
}

/// Decodes a `NumericEdges` packet.
#[must_use]
#[inline(never)]
pub fn decode_numeric_edges(data: &[u8]) -> bool {
    numeric_edges::NumericEdges::decode(data).is_ok()
}

/// Encodes a `StatusReport`, which is the text and binary path.
#[inline(never)]
pub fn encode_status_report(out: &mut [u8], label: &str, blob: &[u8]) -> usize {
    let packet = flight_shapes::StatusReport {
        type_: 0,
        sec_hdr_flag: 1,
        seq_flgs: 3,
        seq_ctr: 1,
        pkt_len: 54,
        mode: flight_shapes::Mode::Nominal,
        heater_on: true,
        spare_4: 0,
        build_id: "v1.0.0-a",
        label,
        note: "ok",
        blob,
        temp: -40,
        count: 1,
    };
    packet.encode(out).unwrap_or(0)
}

/// Decodes a `StatusReport`.
#[must_use]
#[inline(never)]
pub fn decode_status_report(data: &[u8]) -> bool {
    flight_shapes::StatusReport::decode(data).is_ok()
}

/// Encodes a `Beacon`, which is the unaligned path.
#[inline(never)]
pub fn encode_beacon(out: &mut [u8]) -> usize {
    let packet = flight_shapes::Beacon {
        type_: 0,
        sec_hdr_flag: 0,
        seq_flgs: 3,
        seq_ctr: 2,
        pkt_len: 28,
        flag_a: 1,
        small: 100,
        wide_odd: 12_345_678_901,
        ones_odd: -9,
        signmag_odd: -10,
        f32_odd: 1.25,
        f64_odd: 2.75,
        f16_odd: 0.5,
        baud: flight_shapes::Baud::C115200,
        tail_pad: 0,
        pad_5: 0,
    };
    packet.encode(out).unwrap_or(0)
}

/// Chooses a container and decodes it, which is the dispatcher.
#[must_use]
#[inline(never)]
pub fn dispatch_flight_shapes(data: &[u8]) -> bool {
    flight_shapes::decode(data).is_ok()
}

/// Decodes a real mission packet.
#[must_use]
#[inline(never)]
pub fn decode_jpss(data: &[u8]) -> bool {
    jpss::JpssAttEphem::decode(data).is_ok()
}

/// Applies every calibrator the definition declares.
///
/// Here for the panic check rather than for its own sake: a calibrator that is generated but
/// never called does not reach the emitted LLVM IR, and the check would then pass on it
/// without having looked at it. The arithmetic it exercises is the interesting part —
/// `i128::checked_pow`, a fallback to `powi`, a division, and a binary search over `f64`.
#[inline(never)]
#[must_use]
pub fn calibrate_all(data: &[u8]) -> f64 {
    let Ok(packet) = calibrated::Telemetry::decode(data) else {
        return f64::NAN;
    };
    let mut sum = 0.0;
    for value in [
        packet.poly_u32_calibrated(),
        packet.poly_f64_calibrated(),
        packet.poly_big_calibrated(),
        packet.poly_s16_calibrated(),
        packet.spl0_u8_calibrated(),
        packet.spl1_u8_calibrated(),
    ] {
        match value {
            Ok(value) => sum += value,
            Err(_) => return f64::NAN,
        }
    }
    sum
}

/// Encodes and decodes a packet of little-endian fields.
///
/// Here for the same reason as `calibrate_all`: a byte reversal that is generated but never
/// called does not reach the emitted IR, and the panic check would pass without having
/// looked at it.
#[inline(never)]
#[must_use]
pub fn round_trip_little_endian(out: &mut [u8]) -> bool {
    let packet = byte_order::Telemetry {
        u16_le: 0x1234,
        u24_le: 0x0012_3456,
        u32_le: 0x1234_5678,
        u64_le: 0x1234_5678_9ABC_DEF0,
        s16_le: -1234,
        s32_le: -123_456,
        f16_le: 1.5,
        f32_le: 2.5,
        f64_le: 3.5,
        u8_le: 7,
        nib: 5,
        u16_le_odd: 0xBEEF,
        pad_4: 0,
        u16_be: 0xCAFE,
        f32_be: -4.5,
    };
    if packet.encode(out).is_err() {
        return false;
    }
    match byte_order::Telemetry::decode(out) {
        Ok(returned) => returned == packet,
        Err(_) => false,
    }
}
