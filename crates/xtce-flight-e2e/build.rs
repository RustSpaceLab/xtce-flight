//! Compiles each bundled definition into flight code, and a harness to exercise it.
//!
//! Panicking is the right failure mode: a definition this project ships and expects to
//! compile must not quietly turn into a skipped test.

use std::path::{Path, PathBuf};

/// `(module name, definition, root container)`.
const CASES: &[(&str, &str, Option<&str>)] = &[
    // Purpose-built. Every numeric shape the emitter can produce, byte-aligned and one
    // nibble off a boundary, including the 64-bit float that spans nine bytes.
    ("numeric_edges", "numeric_edges.xml", None),
    // Purpose-built. Inheritance and restriction criteria, enumerations whose labels are
    // not Rust identifiers, and all three ways XTCE delimits a string.
    ("flight_shapes", "flight_shapes.xml", None),
    // Purpose-built. Calibrators, which no mission definition in reach has at all.
    ("calibrated", "calibrated.xml", None),
    ("calibrated_bounded", "calibrated_bounded.xml", None),
    // Purpose-built. `leastSignificantByteFirst`, which an encoder has to invert rather than
    // merely apply.
    ("byte_order", "byte_order.xml", None),
    // Purpose-built. Arrays, which are one field per element by the time the emitter sees
    // them — this checks that nothing about the encoder needed to know that.
    ("arrays", "arrays.xml", None),
    // A real mission definition: JPSS-1 attitude and ephemeris, three criteria deep.
    // Rooted at CCSDSPacket, not the telemetry sub-container: the criteria that select
    // JPSS_ATT_EPHEM test fields the primary header declares, and a plan that starts below
    // them cannot see the bits they name.
    ("jpss", "jpss1_geolocation_xtce_v1.xml", None),
    // The same packet, selected by a <BooleanExpression> instead of a <ComparisonList>. It
    // is the only real definition in reach with one, and every condition has to be written
    // by `encode` or the interpreter will not recognise what comes out.
    ("contrived", "contrived_inheritance_structure.xml", None),
];

fn main() {
    println!("cargo::rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let testdata = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");

    for (module, relative, root) in CASES {
        let path = testdata.join(relative);
        println!("cargo::rerun-if-changed={}", path.display());

        let db = xtce_model::XtceDb::from_path(&path)
            .unwrap_or_else(|error| panic!("{relative}: {error}"));

        let options = xtce_flight::Options {
            root: root.map(str::to_owned),
            source_label: Some(format!("testdata/{relative}")),
        };

        let flight = xtce_flight::generate(&db, &options)
            .unwrap_or_else(|error| panic!("{relative}: {error}"));
        std::fs::write(out_dir.join(format!("{module}.rs")), flight)
            .unwrap_or_else(|error| panic!("{module}.rs: {error}"));

        let layout = xtce_flight::layout(&db, &options)
            .unwrap_or_else(|error| panic!("{relative}: {error}"));
        let harness = xtce_flight::harness::generate(&layout, "super::flight");
        std::fs::write(out_dir.join(format!("{module}_harness.rs")), harness)
            .unwrap_or_else(|error| panic!("{module}_harness.rs: {error}"));
    }
}
