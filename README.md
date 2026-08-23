# xtce-flight

[![CI](https://github.com/RustSpaceLab/xtce-flight/actions/workflows/ci.yml/badge.svg)](https://github.com/RustSpaceLab/xtce-flight/actions/workflows/ci.yml)

Compile an XTCE definition into a `no_std` Rust **encoder** — and decoder — for the spacecraft
side. Telemetry going down, telecommands coming up.

Ground software reads packets, and the public XTCE tooling reflects that: parsers, decoders,
displays. Flight software writes them, on a part with no heap, no operating system and a hard
rule against a code path that can panic. The generators that produce that half are written
inside the companies that fly and stay there.

```console
$ xtce-flight generate mission.xml -o telemetry.rs
```

```rust
// Every field the definition leaves free, and only those: VERSION and PKT_APID are
// absent because the restriction criteria fix them.
let report = StatusReport {
    type_: 0,
    sec_hdr_flag: 1,
    seq_flgs: 3,
    seq_ctr: 1,
    pkt_len: 54,                  // the caller's: see below

    mode: Mode::Nominal,          // an enumeration, not a magic number
    heater_on: true,
    spare_4: 0,
    build_id: "v1.0.0-a",         // borrowed; nothing is allocated
    label: "startup",
    note: "ok",
    blob: &payload,
    temp: -40,                    // i16, because the field is 16 bits
    count: 1,
};

let mut buffer = [0u8; StatusReport::LEN];
let written = report.encode(&mut buffer)?;
```

The restriction criteria that identify the container are not fields. They are what makes the
packet recognisable, so `encode` writes them and the caller cannot get them wrong.

## What is guaranteed, and how it is checked

| Claim | How CI proves it |
|---|---|
| Compiles with no `std`, no `alloc` | `probes/nostd` is `#![no_std]` and builds for `thumbv7em-none-eabihf` |
| No `unsafe` | the probe is `#![forbid(unsafe_code)]` |
| **No panicking branch** | `scripts/check-no-panic.sh` greps the emitted LLVM IR for `core::panicking` |
| Encodes what the ground will read | encode here, decode with `xtce-decode`, compare with what went in |
| A value that does not fit is refused | `EncodeError::OutOfRange`, named — never a silent truncation |

The panic check is the one worth explaining. "We do not call `unwrap`" is easy and nearly
worthless: a single slice index with a runtime bound puts a panic path in the binary, and on
a part with no unwinder and no console a panic is a silent reset. So `encode` and `decode`
convert the caller's slice to a reference to a fixed-size array once, at the top, and every
index after that is a literal into an array of known length — provably in bounds, so the
check folds away. The script then reads the IR and fails if any reference to the panicking
machinery survived. It also fails if the probe's own functions are *absent* from the IR, so
it cannot pass by having optimised away the code it is meant to inspect.

Current result, on the nine definitions the probe compiles:

```
no panic path in 10718 lines of IR for thumbv7em-none-eabihf
```

## How correctness is argued

Encoding and decoding with the same generated code proves only that the generator agrees with
itself: a field written at the wrong offset and read back from the wrong offset round trips
perfectly. So the packet goes out through `xtce-flight` and comes back through
[`xtce-decode`](https://github.com/RustSpaceLab/xtce-rs), which shares no code with it and is
already checked against the `space_packet_parser` reference on roughly 17 000 real packets.

```
xtce-flight encodes  →  xtce-decode decodes  →  compare against the values that went in
                             ↑
                        already equal to space_packet_parser
```

For a calibrated field the two halves are checked separately: the raw value against what was
encoded, and `x_calibrated()` against the interpreter's own calibration — an implementation
this one shares no code with.

The values are invented by a harness that is itself generated from the same layout — one
`struct` per container means a hand-written filler cannot be reused, and a hand-written one
tends to cover the fields whose shape the author was thinking about. What keeps that from
being circular is that the harness never asks the encoder what a value *means*: it reports
the number it put in, and the comparison is against the independent decoder.

## How it relates to `xtce-rs`

**One new back end, not a new tool chain.** Parsing, the intermediate representation,
container flattening, restriction criteria and the refusal rules all come from
[`xtce_codegen::plan`](https://github.com/RustSpaceLab/xtce-rs). What is new here is the
emitter, and the direction: values in, bits out.

That is also why this cannot go to crates.io yet — `xtce-rs` is unpublished, so the
dependencies are pinned git revisions. Not a defect, just a consequence.

## Scope

Containers laid out entirely at generation time. Everything else is refused **by name**,
never quietly skipped — a generator whose output silently covers half a definition is worse
than one that stops, because the gap only shows up in flight.

| | |
|---|---|
| Integers, 1–64 bits, all three signed codings | Yes |
| IEEE-754 binary16, binary32, binary64 | Yes |
| MIL-STD-1750A | Refused — reading it is many-to-one, so there is no inverse to write |
| Booleans, enumerations (as generated Rust enums) | Yes |
| Fixed-size text: whole-buffer, terminated, length-prefixed | Yes |
| Fixed-size binary | Yes |
| Container inheritance and equality restriction criteria | Yes |
| `BooleanExpression`: a conjunction of `Condition`s | Yes — it is a conjunction of equalities however the XML spells it |
| `ArrayParameterType`, `ArrayParameterRefEntry` | Yes — one struct field per element, `temps_0`…, expanded before this generator runs |
| `AggregateParameterType` | Yes — one field per member, `state_samples_2`…; nests with arrays either way |
| `ORedConditions` | Refused — a disjunction is not a packet an encoder can write |
| `leastSignificantByteFirst`, whole-byte widths | Yes — including a criterion, inverted when the code is generated |
| `leastSignificantByteFirst`, other widths | Refused — reversing a value narrower than its byte count has no inverse |
| `DefaultCalibrator`: polynomial and spline | Yes — on the decode side, as an accessor |
| `ContextCalibratorList` | Yes — an else-if chain over the packet's own fields, resolved when the code is generated |
| `CommandMetaData`: `MetaCommand`, `ArgumentList`, `CommandContainer` | Yes — a command is a container, an argument is a field |
| `ArgumentAssignment` | Yes — the same thing a restriction criterion is, read the other way round |
| `FixedValueEntry` | Yes — written by `encode`, stepped over by `decode`; see below |
| A fixed value wider than 64 bits | Yes when it is whole bytes on a byte boundary — written one byte at a time; refused otherwise, since it would have to be shifted across every byte it touches |
| A context criterion on bits a restriction criterion fixes, or on a boolean wider than a bit | Refused — the struct does not carry the value it would compare |
| Splines above first order | Refused |
| A width that comes from the packet | Refused — it has no fixed place in a `struct` |
| An inequality restriction criterion | Refused — it names a set, and an encoder writes one value |
| A float that is not 16, 32 or 64 bits | Refused — there is no such IEEE-754 format |
| Text or binary off a byte boundary | Refused — it is written as a slice |

### A telecommand is a container, and its arguments are its fields

XTCE describes a telecommand with its own vocabulary — a `MetaCommand` with an `ArgumentList`,
an inheritance link carrying `ArgumentAssignment`s, a `CommandContainer` whose entries may name
arguments, parameters and fixed values. None of that is a new shape. It is a container of
fields selected by fixed values, which is what a telemetry container is, so it compiles into
the same `struct` with the same `encode` and `decode`.

The direction is what changes. Telemetry goes down: this generator writes it, the ground reads
it. A telecommand goes up: the ground writes it, the part reads it. Same two pieces of code,
swapped over — and `encode` is worth having on both ends anyway, because a spacecraft that can
build the command it is about to obey can test itself.

An `ArgumentAssignment` becomes a restriction criterion, because it is one read backwards:
assigning `OPCODE = 7` is what makes this command a specialisation of its base, and comparing
`OPCODE == 7` is what recognises an arriving packet as this command. So `encode` writes the
opcode and the caller cannot get it wrong, exactly as with a telemetry criterion.

A `FixedValueEntry` — a sync marker, a spare nibble, a trailer — is written by `encode` and
**not** read by `decode` or `matches`. That is worth saying out loud, because a sync marker
looks like something a receiver ought to check. XTCE selects a container by its restriction
criteria and has no rule that makes a fixed value discriminate; the interpreter this generator
is checked against does not check them either; and checking here would make the two disagree
about whether a malformed packet is this command, in the direction where only one of them
refuses. Verifying a sync marker is a layer below XTCE, where the framing and the checksum
live.

### Byte order is an operation to invert, not to apply

A decoder reads a `leastSignificantByteFirst` field big-endian and reverses `ceil(width / 8)`
bytes of the value. An encoder has to undo that, which for a whole number of bytes is the
same operation again — reversing bytes is its own inverse — and for anything else is not
possible at all: a twelve-bit field decodes to values up to sixteen bits wide, and most of
them cannot be written back. Whole-byte widths compile; the rest are refused with that as the
reason.

The case worth naming is a *criterion* on a little-endian field. It compares the value after
the reversal, so `encode` has to write the reversal undone — and it does, computed when the
code is generated rather than costing anything in flight. Getting that wrong would not
produce a wrong number; it would produce a packet the ground cannot recognise at all.

### Calibration does not touch encoding

A calibrator turns an ADC count into a temperature. A spacecraft has the count; the
conversion is the ground's job, and XTCE says how. So `encode` is byte-identical whether a
field is calibrated or not, no calibrator is inverted, and no struct field changes type.

What is added is one accessor per calibrated field:

```rust
let raw = packet.temp;                     // what the sensor gave you
let celsius = packet.temp_calibrated()?;   // what the ground will read
```

It is a method rather than a stored field on purpose: storing it would cost eight bytes of
RAM per calibrated parameter on the part least able to spare them, to hold a number the
flight side usually never looks at.

A parameter may have several calibrators and let the packet choose — a
`<ContextCalibratorList>`, tried in order, with the default behind them. The accessor is then
an else-if chain over the *struct's own fields*, and which field each criterion reads is
decided when the code is generated. That resolution has one corner worth knowing about: a
criterion is compared against what the container has decoded so far, so one naming a
parameter that comes *later* in the same container does not read that parameter at all — it
reads the raw value of the field being calibrated. Surprising, agreed on by the reference
implementation and both generators here, and pinned by a test that drives it deliberately
rather than hoping a seed lands on it.

Two criteria are refused rather than guessed at: one testing bits the restriction criteria
fix, because those are not a field of the struct and there is nothing to compare; and one
testing a boolean wider than a single bit, because `decode` keeps whether the bits were
nonzero and a criterion means the bits.

The arithmetic is `xtce-decode`'s, line for line, and checked against it on `to_bits()`.
That is not pedantry. Floating-point addition is neither associative nor commutative, so
summing a polynomial by Horner's method — or sorted by exponent, or in any order but the
document's — gives an answer right to fourteen digits and wrong in the last bit. Worse: an
*integral* raw value is raised to its power exactly and rounded once, while a *float* raw
goes through repeated squaring, which rounds at every step. Those are different numbers, and
`testdata/calibrated.xml` carries identical polynomial terms over both encodings so that a
generator collapsing them into one routine fails a test rather than a mission.

This is also where the bare-metal probe earned its keep: `f64::powi` is in `std`, not `core`,
so the first version of the calibration emitter passed every test and would not build for a
Cortex-M at all. The generated code now writes out the same square-and-multiply sequence
`powi` performs — verified bit-identical over four million comparisons, and pinned by the
differential test.

### Four choices worth knowing about

**Whole-buffer text has to fill its field exactly.** XTCE's un-delimited string *is* its
buffer, so a shorter value would decode with its padding attached. Requiring the exact length
keeps `encode` and `decode` inverses; padding, if a mission wants it, is the caller's to add.

**The CCSDS length and sequence fields are the caller's.** `encode` does not compute
`PKT_LEN`: XTCE does not say which parameter carries the packet length, and inferring it from
a name would be a guess about the mission. A wrong value there produces a packet that is
byte-perfect and unframeable, so set it deliberately.

**MIL-STD-1750A is read, not written.** `xtce-rs` decodes it and matches the Python reference
doing so. This generator refuses it, because the format is many-to-one: an unnormalised
mantissa and a zero that kept its exponent both denote numbers that cannot be put back as the
bits they came from. `encode` would have to pick a representation and stop being the inverse
of `decode` for most inputs — which is a claim this README makes and would rather keep. Same
line as a little-endian field that is not a whole number of bytes.

**binary16 rounds, and does not reject.** A flight computer holding a temperature in `f32`
should be able to put it in a 16-bit field. Encoding rounds to nearest, ties to even; the
ground reads back the rounded value. All 63 488 finite binary16 encodings are checked both
ways against the definition of the format, because nothing else would catch an error there —
no mission definition in reach has a 16-bit float at all.

## Testing

```console
$ cargo test --workspace                        # 32 tests, no Python needed
$ ./scripts/check-no-panic.sh                   # the bare-metal gate
```

| Command | Proves |
|---|---|
| `cargo test -p xtce-flight` | what the layout decides, and what it refuses, on inline XTCE |
| `cargo test -p xtce-flight-e2e` | the generated encoder and calibration against `xtce-decode`, 256 seeded packets per container |
| `./scripts/check-no-panic.sh` | `no_std`, no `unsafe`, no panic path, for Cortex-M4F |

Neither the flight code nor the harness is committed: `build.rs` writes both. That is the
shape a mission uses, and it is why a definition that expands to thousands of lines can be
tested without putting them in the repository.

## Test definitions

| File | Why it is here |
|---|---|
| `testdata/jpss1_geolocation_xtce_v1.xml` | a real mission definition — JPSS-1 attitude and ephemeris, three criteria deep |
| `testdata/numeric_edges.xml` | purpose-built: every numeric shape, aligned and four bits off a boundary, including the 64-bit float that spans nine bytes |
| `testdata/flight_shapes.xml` | purpose-built: inheritance, enumerations whose labels are not Rust identifiers, and all three ways XTCE delimits a string |
| `testdata/calibrated.xml` | purpose-built: polynomials over both an integer and a float encoding, a negative exponent, and splines of both orders |
| `testdata/calibrated_bounded.xml` | purpose-built: one spline that may not extrapolate, so the refusal can be driven on both sides of every boundary |
| `testdata/context_calibrated.xml` | purpose-built: several calibrators for one parameter, chosen by the packet — including a criterion on the field being calibrated, and one on a parameter decoded after it |
| `testdata/commands.xml` | purpose-built: a `<CommandMetaData>` half — two commands specialising an abstract base by argument assignment, a four-byte sync marker, a four-bit spare that leaves the payload off a byte boundary, and a trailer whose `binaryValue` is wider than its `sizeInBits` |
| `testdata/contrived_inheritance_structure.xml` | a real mission definition whose container is selected by a `<BooleanExpression>` rather than a `<ComparisonList>` |
| `testdata/byte_order.xml` | purpose-built: little-endian fields of every whole-byte width, aligned and not, and a little-endian criterion |
| `testdata/arrays.xml` | purpose-built: arrays of one and two dimensions, a subset of one, and six four-bit elements that keep everything after them off a byte boundary |
| `testdata/aggregates.xml` | purpose-built: an aggregate, an array of them, one holding an array, and a four-bit member that ends it off a byte boundary |

Nine of the eleven are written rather than found, and deliberately so. Mission files are the
right thing to validate against, but between them they reach almost none of an encoder's
edges: no label that needs sanitising, no terminated string, no float at an offset that makes
it span an extra byte, and **no calibrator anywhere at all**. Two bugs in this repository
were found only by a purpose-built file: the binary16 widening was wrong for every subnormal
with more than one bit set in its fraction, and the calibration emitter used a `std`
function in code that has to build for a bare-metal target.

`jpss1_geolocation_xtce_v1.xml` is vendored from
[`lasp/space_packet_parser`](https://github.com/lasp/space_packet_parser) under BSD 3-Clause;
see `testdata/LICENSE.txt`.

## Layout

```
crates/xtce-flight        the generator, and the `xtce-flight` command
crates/xtce-flight-e2e    generated code and harness, against the interpreter
probes/nostd              the bare-metal build, and what the panic check reads
scripts/check-no-panic.sh the panic check
testdata/                 eleven definitions, two of them real
```

## Licence

MIT or Apache-2.0, at your option.
