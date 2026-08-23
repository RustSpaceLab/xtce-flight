//! Generates the same flight code the tests use, for a bare-metal target.

use std::path::{Path, PathBuf};

const CASES: &[(&str, &str)] = &[
    ("numeric_edges", "numeric_edges.xml"),
    ("flight_shapes", "flight_shapes.xml"),
    ("jpss", "jpss1_geolocation_xtce_v1.xml"),
    ("calibrated", "calibrated.xml"),
    ("context_calibrated", "context_calibrated.xml"),
    ("byte_order", "byte_order.xml"),
    ("arrays", "arrays.xml"),
    ("aggregates", "aggregates.xml"),
];

fn main() {
    println!("cargo::rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let testdata = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");

    for (module, relative) in CASES {
        let path = testdata.join(relative);
        println!("cargo::rerun-if-changed={}", path.display());

        let db = xtce_model::XtceDb::from_path(&path)
            .unwrap_or_else(|error| panic!("{relative}: {error}"));
        let options = xtce_flight::Options {
            root: None,
            source_label: Some(format!("testdata/{relative}")),
        };
        let source = xtce_flight::generate(&db, &options)
            .unwrap_or_else(|error| panic!("{relative}: {error}"));
        std::fs::write(out_dir.join(format!("{module}.rs")), source)
            .unwrap_or_else(|error| panic!("{module}.rs: {error}"));
    }
}
