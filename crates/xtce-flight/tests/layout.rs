//! What the layout decides, and what it refuses.
//!
//! These use inline XTCE rather than the bundled definitions: a refusal is easiest to state
//! next to the smallest definition that provokes it, and a test that has to be read against a
//! 14 000-line mission file is a test nobody reads.

use xtce_flight::{ContextCriterion, ContextTest, FlightError, Kind, Options};
use xtce_model::XtceDb;

/// Wraps `body` in the smallest space system that will load.
fn definition(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<SpaceSystem xmlns="http://www.omg.org/spec/XTCE/20180204" name="Test">
  <TelemetryMetaData>
{body}
  </TelemetryMetaData>
</SpaceSystem>"#
    )
}

fn layout_of(body: &str) -> Result<xtce_flight::Layout, FlightError> {
    let db = XtceDb::from_xml(&definition(body)).expect("definition loads");
    xtce_flight::layout(&db, &Options::default())
}

/// A single container holding one parameter of `type_body`, plus enough padding to make it a
/// whole number of bytes.
fn one_field(type_body: &str, pad_bits: u32) -> String {
    let pad = if pad_bits == 0 {
        String::new()
    } else {
        format!(
            r#"<IntegerParameterType name="PAD_T">
        <IntegerDataEncoding sizeInBits="{pad_bits}" encoding="unsigned"/>
      </IntegerParameterType>"#
        )
    };
    let pad_parameter = if pad_bits == 0 {
        String::new()
    } else {
        r#"<Parameter name="PAD" parameterTypeRef="PAD_T"/>"#.to_owned()
    };
    let pad_entry = if pad_bits == 0 {
        String::new()
    } else {
        r#"<ParameterRefEntry parameterRef="PAD"/>"#.to_owned()
    };

    format!(
        r#"    <ParameterTypeSet>
      {type_body}
      {pad}
    </ParameterTypeSet>
    <ParameterSet>
      <Parameter name="FIELD" parameterTypeRef="FIELD_T"/>
      {pad_parameter}
    </ParameterSet>
    <ContainerSet>
      <SequenceContainer name="Only">
        <EntryList>
          <ParameterRefEntry parameterRef="FIELD"/>
          {pad_entry}
        </EntryList>
      </SequenceContainer>
    </ContainerSet>"#
    )
}

fn refusal(result: Result<xtce_flight::Layout, FlightError>) -> String {
    match result {
        Ok(_) => panic!("expected a refusal, got a layout"),
        Err(error) => error.to_string(),
    }
}

/// A whole document with a one-field telemetry half and the given `<CommandMetaData>` body.
///
/// Not `definition`: that one wraps its argument in `<TelemetryMetaData>`, and a command half
/// is a sibling of it rather than a child.
fn command_definition(command_body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<SpaceSystem xmlns="http://www.omg.org/spec/XTCE/20180204" name="Test">
  <TelemetryMetaData>
    <ParameterTypeSet>
      <IntegerParameterType name="FIELD_T">
        <IntegerDataEncoding sizeInBits="8" encoding="unsigned"/>
      </IntegerParameterType>
    </ParameterTypeSet>
    <ParameterSet><Parameter name="FIELD" parameterTypeRef="FIELD_T"/></ParameterSet>
    <ContainerSet>
      <SequenceContainer name="Only">
        <EntryList><ParameterRefEntry parameterRef="FIELD"/></EntryList>
      </SequenceContainer>
    </ContainerSet>
  </TelemetryMetaData>
  <CommandMetaData>
    <ArgumentTypeSet>
      <IntegerArgumentType name="U8_A">
        <IntegerDataEncoding sizeInBits="8" encoding="unsigned"/>
      </IntegerArgumentType>
    </ArgumentTypeSet>
    <MetaCommandSet>
      <MetaCommand name="Cmd">
        <ArgumentList><Argument name="A" argumentTypeRef="U8_A"/></ArgumentList>
        <CommandContainer name="Packing">
          <EntryList>
{command_body}
            <ArgumentRefEntry argumentRef="A"/>
          </EntryList>
        </CommandContainer>
      </MetaCommand>
    </MetaCommandSet>
  </CommandMetaData>
</SpaceSystem>"#
    )
}

/// The layout of `Packing` in a `command_definition`.
fn command_layout(command_body: &str) -> Result<xtce_flight::Layout, FlightError> {
    let db = XtceDb::from_xml(&command_definition(command_body)).expect("definition loads");
    xtce_flight::layout(
        &db,
        &Options {
            root: Some("Packing".to_owned()),
            source_label: None,
        },
    )
}

#[test]
fn a_plain_container_becomes_one_struct() {
    let layout = layout_of(&one_field(
        r#"<IntegerParameterType name="FIELD_T">
        <IntegerDataEncoding sizeInBits="12" encoding="unsigned"/>
      </IntegerParameterType>"#,
        4,
    ))
    .expect("compiles");

    assert_eq!(layout.containers.len(), 1);
    let container = &layout.containers[0];
    assert_eq!(container.xtce_name, "Only");
    assert_eq!(container.type_ident, "Only");
    assert_eq!(container.len_bytes, 2, "12 bits and 4 of pad is two bytes");
    assert_eq!(container.fields.len(), 2);
    assert_eq!(container.fields[0].kind, Kind::Unsigned);
    assert_eq!(container.fields[0].bit_width, 12);
    assert!(!container.borrows, "an integer does not borrow");
}

/// A width that is not one of IEEE-754's is refused rather than invented.
#[test]
fn a_float_that_is_not_16_32_or_64_bits_is_refused() {
    let message = refusal(layout_of(&one_field(
        r#"<FloatParameterType name="FIELD_T">
        <FloatDataEncoding sizeInBits="24" encoding="IEEE754"/>
      </FloatParameterType>"#,
        0,
    )));
    assert!(
        message.contains("16, 32 or 64 bits"),
        "unexpected refusal: {message}"
    );
}

/// Text has to start on a byte and fill whole ones, because it is written as a slice.
#[test]
fn text_off_a_byte_boundary_is_refused() {
    let body = r#"    <ParameterTypeSet>
      <IntegerParameterType name="LEAD_T">
        <IntegerDataEncoding sizeInBits="4" encoding="unsigned"/>
      </IntegerParameterType>
      <StringParameterType name="TEXT_T">
        <StringDataEncoding encoding="UTF-8">
          <SizeInBits><Fixed><FixedValue>32</FixedValue></Fixed></SizeInBits>
        </StringDataEncoding>
      </StringParameterType>
    </ParameterTypeSet>
    <ParameterSet>
      <Parameter name="LEAD" parameterTypeRef="LEAD_T"/>
      <Parameter name="TEXT" parameterTypeRef="TEXT_T"/>
    </ParameterSet>
    <ContainerSet>
      <SequenceContainer name="Only">
        <EntryList>
          <ParameterRefEntry parameterRef="LEAD"/>
          <ParameterRefEntry parameterRef="TEXT"/>
        </EntryList>
      </SequenceContainer>
    </ContainerSet>"#;
    // The plan refuses this before the layout sees it — a decoder cannot borrow an
    // unaligned slice either — so the message is `xtce-codegen`'s. The layout keeps its own
    // check anyway: it is the one that holds if the plan ever grows an unaligned path.
    let message = refusal(layout_of(body));
    assert!(
        message.contains("not byte-aligned"),
        "unexpected refusal: {message}"
    );
}

/// A width that comes from the packet has no fixed place in a struct.
#[test]
fn a_data_dependent_width_is_refused() {
    let body = r#"    <ParameterTypeSet>
      <IntegerParameterType name="LEN_T">
        <IntegerDataEncoding sizeInBits="16" encoding="unsigned"/>
      </IntegerParameterType>
      <BinaryParameterType name="BLOB_T">
        <BinaryDataEncoding>
          <SizeInBits>
            <DynamicValue>
              <ParameterInstanceRef parameterRef="LEN"/>
              <LinearAdjustment intercept="0" slope="8"/>
            </DynamicValue>
          </SizeInBits>
        </BinaryDataEncoding>
      </BinaryParameterType>
    </ParameterTypeSet>
    <ParameterSet>
      <Parameter name="LEN" parameterTypeRef="LEN_T"/>
      <Parameter name="BLOB" parameterTypeRef="BLOB_T"/>
    </ParameterSet>
    <ContainerSet>
      <SequenceContainer name="Only">
        <EntryList>
          <ParameterRefEntry parameterRef="LEN"/>
          <ParameterRefEntry parameterRef="BLOB"/>
        </EntryList>
      </SequenceContainer>
    </ContainerSet>"#;
    let message = refusal(layout_of(body));
    assert!(
        message.contains("depends on packet content"),
        "unexpected refusal: {message}"
    );
}

/// A criterion the encoder writes is not also a field the caller sets.
#[test]
fn restriction_criteria_become_constants_and_leave_the_struct() {
    let body = r#"    <ParameterTypeSet>
      <IntegerParameterType name="APID_T">
        <IntegerDataEncoding sizeInBits="8" encoding="unsigned"/>
      </IntegerParameterType>
      <IntegerParameterType name="DATA_T">
        <IntegerDataEncoding sizeInBits="8" encoding="unsigned"/>
      </IntegerParameterType>
    </ParameterTypeSet>
    <ParameterSet>
      <Parameter name="APID" parameterTypeRef="APID_T"/>
      <Parameter name="DATA" parameterTypeRef="DATA_T"/>
    </ParameterSet>
    <ContainerSet>
      <SequenceContainer name="Base" abstract="true">
        <EntryList>
          <ParameterRefEntry parameterRef="APID"/>
        </EntryList>
      </SequenceContainer>
      <SequenceContainer name="Child">
        <EntryList>
          <ParameterRefEntry parameterRef="DATA"/>
        </EntryList>
        <BaseContainer containerRef="Base">
          <RestrictionCriteria>
            <Comparison parameterRef="APID" value="42" useCalibratedValue="false"/>
          </RestrictionCriteria>
        </BaseContainer>
      </SequenceContainer>
    </ContainerSet>"#;

    let layout = layout_of(body).expect("compiles");
    assert_eq!(layout.containers.len(), 1, "only Child is concrete");
    let child = &layout.containers[0];

    assert_eq!(child.constants.len(), 1);
    assert_eq!(child.constants[0].xtce_name, "APID");
    assert_eq!(child.constants[0].raw, 42);

    let names: Vec<&str> = child
        .fields
        .iter()
        .map(|field| field.xtce_name.as_str())
        .collect();
    assert_eq!(
        names,
        ["DATA"],
        "APID is written by encode, so it is not the caller's to set"
    );
}

/// An inequality names a set of values, and an encoder has to write one.
#[test]
fn a_non_equality_criterion_is_refused() {
    let body = r#"    <ParameterTypeSet>
      <IntegerParameterType name="APID_T">
        <IntegerDataEncoding sizeInBits="8" encoding="unsigned"/>
      </IntegerParameterType>
    </ParameterTypeSet>
    <ParameterSet>
      <Parameter name="APID" parameterTypeRef="APID_T"/>
    </ParameterSet>
    <ContainerSet>
      <SequenceContainer name="Base" abstract="true">
        <EntryList>
          <ParameterRefEntry parameterRef="APID"/>
        </EntryList>
      </SequenceContainer>
      <SequenceContainer name="Child">
        <EntryList/>
        <BaseContainer containerRef="Base">
          <RestrictionCriteria>
            <Comparison parameterRef="APID" value="42" comparisonOperator="&gt;"
                        useCalibratedValue="false"/>
          </RestrictionCriteria>
        </BaseContainer>
      </SequenceContainer>
    </ContainerSet>"#;
    let message = refusal(layout_of(body));
    assert!(
        message.contains("equality criterion"),
        "unexpected refusal: {message}"
    );
}

/// Two parameters of the same enumerated type share one generated Rust enum.
#[test]
fn identical_enumerations_are_generated_once() {
    let body = r#"    <ParameterTypeSet>
      <EnumeratedParameterType name="MODE_T">
        <IntegerDataEncoding sizeInBits="8" encoding="unsigned"/>
        <EnumerationList>
          <Enumeration value="0" label="OFF"/>
          <Enumeration value="1" label="ON"/>
        </EnumerationList>
      </EnumeratedParameterType>
    </ParameterTypeSet>
    <ParameterSet>
      <Parameter name="MODE_A" parameterTypeRef="MODE_T"/>
      <Parameter name="MODE_B" parameterTypeRef="MODE_T"/>
    </ParameterSet>
    <ContainerSet>
      <SequenceContainer name="Only">
        <EntryList>
          <ParameterRefEntry parameterRef="MODE_A"/>
          <ParameterRefEntry parameterRef="MODE_B"/>
        </EntryList>
      </SequenceContainer>
    </ContainerSet>"#;

    let layout = layout_of(body).expect("compiles");
    assert_eq!(layout.enums.len(), 1, "one XTCE type, one Rust type");
    assert_eq!(layout.containers[0].fields[0].kind, Kind::Enumerated(0));
    assert_eq!(layout.containers[0].fields[1].kind, Kind::Enumerated(0));
}

/// Labels are free text; identifiers are not.
#[test]
fn labels_that_are_not_identifiers_are_sanitised_without_colliding() {
    let body = r#"    <ParameterTypeSet>
      <EnumeratedParameterType name="MODE_T">
        <IntegerDataEncoding sizeInBits="8" encoding="unsigned"/>
        <EnumerationList>
          <Enumeration value="0" label="LOW-POWER"/>
          <Enumeration value="1" label="low power"/>
          <Enumeration value="2" label="9600"/>
          <Enumeration value="3" label=""/>
        </EnumerationList>
      </EnumeratedParameterType>
    </ParameterTypeSet>
    <ParameterSet>
      <Parameter name="MODE" parameterTypeRef="MODE_T"/>
    </ParameterSet>
    <ContainerSet>
      <SequenceContainer name="Only">
        <EntryList>
          <ParameterRefEntry parameterRef="MODE"/>
        </EntryList>
      </SequenceContainer>
    </ContainerSet>"#;

    let layout = layout_of(body).expect("compiles");
    let variants: Vec<&str> = layout.enums[0]
        .variants
        .iter()
        .map(|(ident, _, _)| ident.as_str())
        .collect();
    assert_eq!(variants, ["LowPower", "LowPower2", "C9600", "Value3"]);

    // The label itself is kept verbatim, because that is what the ground sees.
    assert_eq!(layout.enums[0].variants[1].2, "low power");
}

/// The generated names are the ones a caller types, so they keep the definition's casing.
#[test]
fn type_names_keep_mixed_case_and_fold_shouting() {
    use xtce_flight::layout::type_ident;

    assert_eq!(type_ident("StatusReport"), "StatusReport");
    assert_eq!(type_ident("JPSS_ATT_EPHEM"), "JpssAttEphem");
    assert_eq!(type_ident("Sci0TypeNonZero"), "Sci0TypeNonZero");
    assert_eq!(type_ident("LOW-POWER"), "LowPower");
    assert_eq!(type_ident("9600"), "C9600");
    assert_eq!(type_ident("---"), "Value");
}

/// An `<ANDedConditions>` is a conjunction, so it becomes constants like any other.
#[test]
fn a_conjunction_of_conditions_becomes_constants() {
    let body = r#"    <ParameterTypeSet>
      <IntegerParameterType name="U8"><IntegerDataEncoding sizeInBits="8" encoding="unsigned"/></IntegerParameterType>
    </ParameterTypeSet>
    <ParameterSet>
      <Parameter name="A" parameterTypeRef="U8"/>
      <Parameter name="B" parameterTypeRef="U8"/>
      <Parameter name="BODY" parameterTypeRef="U8"/>
    </ParameterSet>
    <ContainerSet>
      <SequenceContainer name="Base" abstract="true">
        <EntryList>
          <ParameterRefEntry parameterRef="A"/>
          <ParameterRefEntry parameterRef="B"/>
        </EntryList>
      </SequenceContainer>
      <SequenceContainer name="Child">
        <EntryList><ParameterRefEntry parameterRef="BODY"/></EntryList>
        <BaseContainer containerRef="Base">
          <RestrictionCriteria>
            <BooleanExpression>
              <ANDedConditions>
                <Condition>
                  <ParameterInstanceRef parameterRef="A" useCalibratedValue="false"/>
                  <ComparisonOperator>==</ComparisonOperator>
                  <Value>7</Value>
                </Condition>
                <Condition>
                  <ParameterInstanceRef parameterRef="B" useCalibratedValue="false"/>
                  <ComparisonOperator>==</ComparisonOperator>
                  <Value>9</Value>
                </Condition>
              </ANDedConditions>
            </BooleanExpression>
          </RestrictionCriteria>
        </BaseContainer>
      </SequenceContainer>
    </ContainerSet>"#;

    let layout = layout_of(body).expect("compiles");
    let child = &layout.containers[0];
    let written: Vec<(&str, u64)> = child
        .constants
        .iter()
        .map(|constant| (constant.xtce_name.as_str(), constant.raw))
        .collect();
    assert_eq!(written, [("A", 7), ("B", 9)]);
    assert_eq!(
        child
            .fields
            .iter()
            .map(|field| field.xtce_name.as_str())
            .collect::<Vec<_>>(),
        ["BODY"],
        "both criteria are written by encode, so neither is the caller's to set"
    );
}

/// A disjunction is not a packet.
///
/// A decoder can evaluate any boolean expression — it has the packet in front of it. An
/// encoder has to produce one, and `A == 1 || A == 2` does not say which.
#[test]
fn a_disjunction_of_conditions_is_refused() {
    let body = r#"    <ParameterTypeSet>
      <IntegerParameterType name="U8"><IntegerDataEncoding sizeInBits="8" encoding="unsigned"/></IntegerParameterType>
    </ParameterTypeSet>
    <ParameterSet>
      <Parameter name="A" parameterTypeRef="U8"/>
      <Parameter name="BODY" parameterTypeRef="U8"/>
    </ParameterSet>
    <ContainerSet>
      <SequenceContainer name="Base" abstract="true">
        <EntryList><ParameterRefEntry parameterRef="A"/></EntryList>
      </SequenceContainer>
      <SequenceContainer name="Child">
        <EntryList><ParameterRefEntry parameterRef="BODY"/></EntryList>
        <BaseContainer containerRef="Base">
          <RestrictionCriteria>
            <BooleanExpression>
              <ORedConditions>
                <Condition>
                  <ParameterInstanceRef parameterRef="A" useCalibratedValue="false"/>
                  <ComparisonOperator>==</ComparisonOperator>
                  <Value>1</Value>
                </Condition>
                <Condition>
                  <ParameterInstanceRef parameterRef="A" useCalibratedValue="false"/>
                  <ComparisonOperator>==</ComparisonOperator>
                  <Value>2</Value>
                </Condition>
              </ORedConditions>
            </BooleanExpression>
          </RestrictionCriteria>
        </BaseContainer>
      </SequenceContainer>
    </ContainerSet>"#;

    let message = refusal(layout_of(body));
    assert!(
        message.contains("disjunction"),
        "unexpected refusal: {message}"
    );
}

/// Reversing the bytes of a field that is not a whole number of them has no inverse.
///
/// A decoder can do it — `xtce-rs` does, because agreeing with the reference is its contract
/// — and a twelve-bit little-endian field then decodes to values up to sixteen bits wide.
/// Most of those cannot be written back into twelve bits at all, so an encoder that accepted
/// the field would produce packets its own decoder disagrees with.
#[test]
fn a_little_endian_field_that_is_not_whole_bytes_is_refused() {
    let refused = layout_of(&one_field(
        r#"<IntegerParameterType name="FIELD_T">
        <IntegerDataEncoding sizeInBits="12" encoding="unsigned"
                             byteOrder="leastSignificantByteFirst"/>
      </IntegerParameterType>"#,
        4,
    ));
    let message = refusal(refused);
    assert!(
        message.contains("no inverse"),
        "unexpected refusal: {message}"
    );

    // Sixteen bits of the same thing is a whole number of bytes, and compiles.
    let layout = layout_of(&one_field(
        r#"<IntegerParameterType name="FIELD_T">
        <IntegerDataEncoding sizeInBits="16" encoding="unsigned"
                             byteOrder="leastSignificantByteFirst"/>
      </IntegerParameterType>"#,
        0,
    ))
    .expect("whole bytes compile");
    assert!(layout.containers[0].fields[0].swap_bytes);
}

/// A criterion on a little-endian field is written with its bytes the other way round.
///
/// The interpreter compares the value *after* the reversal, so the encoder has to write the
/// reversal undone — and that happens here, when the code is generated, rather than costing
/// anything at run time.
#[test]
fn a_little_endian_criterion_is_inverted_when_it_is_planned() {
    let body = r#"    <ParameterTypeSet>
      <IntegerParameterType name="SEL_T">
        <IntegerDataEncoding sizeInBits="16" encoding="unsigned"
                             byteOrder="leastSignificantByteFirst"/>
      </IntegerParameterType>
      <IntegerParameterType name="U8"><IntegerDataEncoding sizeInBits="8" encoding="unsigned"/></IntegerParameterType>
    </ParameterTypeSet>
    <ParameterSet>
      <Parameter name="SEL" parameterTypeRef="SEL_T"/>
      <Parameter name="BODY" parameterTypeRef="U8"/>
    </ParameterSet>
    <ContainerSet>
      <SequenceContainer name="Base" abstract="true">
        <EntryList><ParameterRefEntry parameterRef="SEL"/></EntryList>
      </SequenceContainer>
      <SequenceContainer name="Child">
        <EntryList><ParameterRefEntry parameterRef="BODY"/></EntryList>
        <BaseContainer containerRef="Base">
          <RestrictionCriteria>
            <Comparison parameterRef="SEL" value="513" useCalibratedValue="false"/>
          </RestrictionCriteria>
        </BaseContainer>
      </SequenceContainer>
    </ContainerSet>"#;

    let layout = layout_of(body).expect("compiles");
    let constant = &layout.containers[0].constants[0];
    // 513 is 0x0201; the bytes that produce it are 0x01 0x02, which read big-endian are
    // 0x0102 — 258.
    assert_eq!(constant.reported, 513);
    assert_eq!(constant.raw, 258);
}

/// MIL-STD-1750A can be read but not written, so it is refused here.
///
/// The format is many-to-one: the standard normalises its mantissa but does not require a
/// decoder to reject an unnormalised one, and a zero mantissa keeps whatever exponent it had.
/// So a great many words denote the same number, and `encode` would have to pick one — making
/// it not the inverse of `decode` for most inputs, in a generator whose whole claim is that
/// the two are inverses.
///
/// `xtce-rs` decodes it, and matches the Python reference doing so. Reading is well defined;
/// only writing is not.
#[test]
fn a_mil_std_1750a_float_is_refused() {
    let message = refusal(layout_of(&one_field(
        r#"<FloatParameterType name="FIELD_T">
        <FloatDataEncoding sizeInBits="32" encoding="MILSTD_1750A"/>
      </FloatParameterType>"#,
        0,
    )));
    assert!(
        message.contains("no inverse to write"),
        "unexpected refusal: {message}"
    );

    // The control: an IEEE-754 word of the same width compiles.
    let layout = layout_of(&one_field(
        r#"<FloatParameterType name="FIELD_T">
        <FloatDataEncoding sizeInBits="32" encoding="IEEE754"/>
      </FloatParameterType>"#,
        0,
    ))
    .expect("IEEE-754 compiles");
    assert_eq!(layout.containers[0].fields[0].kind, Kind::Float32);
}

/// A context calibrator resolves to the struct's own fields, by where the bits are.
///
/// The plan states a criterion as an offset and a width, because the decoder it was written
/// for reads the packet. An accessor on a decoded struct cannot; it has the fields and
/// nothing else. So each test is resolved to the field occupying exactly those bits — which
/// is also the only way the second case here comes out right. SELF's criterion names SELF,
/// a parameter the container has not decoded when the comparison is made, and what the
/// reference compares then is the raw value of the field being calibrated: itself.
#[test]
fn a_context_calibrator_resolves_to_the_fields_of_the_struct() {
    let layout = layout_of(
        r#"    <ParameterTypeSet>
      <IntegerParameterType name="MODE_T">
        <IntegerDataEncoding sizeInBits="8" encoding="unsigned"/>
      </IntegerParameterType>
      <IntegerParameterType name="SENSOR_T">
        <IntegerDataEncoding sizeInBits="8" encoding="unsigned">
          <ContextCalibratorList>
            <ContextCalibrator>
              <ContextMatch>
                <Comparison parameterRef="MODE" value="1" useCalibratedValue="false"/>
              </ContextMatch>
              <Calibrator>
                <PolynomialCalibrator><Term coefficient="0.5" exponent="1"/></PolynomialCalibrator>
              </Calibrator>
            </ContextCalibrator>
            <ContextCalibrator>
              <ContextMatch>
                <Comparison parameterRef="SENSOR" value="7" comparisonOperator="&gt;"
                            useCalibratedValue="false"/>
              </ContextMatch>
              <Calibrator>
                <PolynomialCalibrator><Term coefficient="2.0" exponent="1"/></PolynomialCalibrator>
              </Calibrator>
            </ContextCalibrator>
          </ContextCalibratorList>
          <DefaultCalibrator>
            <PolynomialCalibrator><Term coefficient="1.0" exponent="1"/></PolynomialCalibrator>
          </DefaultCalibrator>
        </IntegerDataEncoding>
      </IntegerParameterType>
    </ParameterTypeSet>
    <ParameterSet>
      <Parameter name="MODE" parameterTypeRef="MODE_T"/>
      <Parameter name="SENSOR" parameterTypeRef="SENSOR_T"/>
    </ParameterSet>
    <ContainerSet>
      <SequenceContainer name="Only">
        <EntryList>
          <ParameterRefEntry parameterRef="MODE"/>
          <ParameterRefEntry parameterRef="SENSOR"/>
        </EntryList>
      </SequenceContainer>
    </ContainerSet>"#,
    )
    .expect("compiles");

    let sensor = &layout.containers[0].fields[1];
    assert_eq!(sensor.ident, "sensor");
    assert!(
        sensor.calibration.is_some(),
        "the default calibrator is the fallback, and a list without one is refused upstream"
    );
    assert_eq!(sensor.contexts.len(), 2, "tried in order, then the default");

    let tests: Vec<&ContextTest> = sensor
        .contexts
        .iter()
        .map(|context| match &context.criteria {
            ContextCriterion::Test(test) => test,
            other => panic!("expected one comparison, got {other:?}"),
        })
        .collect();

    assert_eq!(
        tests[0].ident, "mode",
        "a preceding field, by name and bits"
    );
    assert_eq!(tests[0].value, 1);
    // The definition says SENSOR, and SENSOR is what it gets: the field being calibrated.
    assert_eq!(tests[1].ident, "sensor");
    assert_eq!(tests[1].xtce_name, "SENSOR");
    assert_eq!(tests[1].value, 7);
}

/// A criterion on bits the restriction criteria fix is refused, not quietly ignored.
///
/// Those bits are not a field: they are what makes the packet recognisable, so the encoder
/// writes them and the struct does not carry them. The accessor therefore has nothing to
/// compare, and falling back to the default calibrator would report a number that is wrong
/// in the one way nothing downstream can catch.
#[test]
fn a_context_criterion_on_a_restriction_criterion_is_refused() {
    let error = refusal(layout_of(
        r#"    <ParameterTypeSet>
      <IntegerParameterType name="APID_T">
        <IntegerDataEncoding sizeInBits="8" encoding="unsigned"/>
      </IntegerParameterType>
      <IntegerParameterType name="SENSOR_T">
        <IntegerDataEncoding sizeInBits="8" encoding="unsigned">
          <ContextCalibratorList>
            <ContextCalibrator>
              <ContextMatch>
                <Comparison parameterRef="APID" value="9" useCalibratedValue="false"/>
              </ContextMatch>
              <Calibrator>
                <PolynomialCalibrator><Term coefficient="0.5" exponent="1"/></PolynomialCalibrator>
              </Calibrator>
            </ContextCalibrator>
          </ContextCalibratorList>
          <DefaultCalibrator>
            <PolynomialCalibrator><Term coefficient="1.0" exponent="1"/></PolynomialCalibrator>
          </DefaultCalibrator>
        </IntegerDataEncoding>
      </IntegerParameterType>
    </ParameterTypeSet>
    <ParameterSet>
      <Parameter name="APID" parameterTypeRef="APID_T"/>
      <Parameter name="SENSOR" parameterTypeRef="SENSOR_T"/>
    </ParameterSet>
    <ContainerSet>
      <SequenceContainer name="Base" abstract="true">
        <EntryList>
          <ParameterRefEntry parameterRef="APID"/>
        </EntryList>
      </SequenceContainer>
      <SequenceContainer name="Only">
        <EntryList>
          <ParameterRefEntry parameterRef="SENSOR"/>
        </EntryList>
        <BaseContainer containerRef="Base">
          <RestrictionCriteria>
            <Comparison parameterRef="APID" value="9" useCalibratedValue="false"/>
          </RestrictionCriteria>
        </BaseContainer>
      </SequenceContainer>
    </ContainerSet>"#,
    ));

    assert!(
        error.contains("ContextCalibrator") && error.contains("does not carry"),
        "unexpected refusal: {error}"
    );
}

/// A boolean wider than one bit keeps no raw value for a criterion to compare.
///
/// The struct holds `bool`, and `decode` sets it from whether the bits were nonzero. For one
/// bit that is lossless and the comparison is exact. For more, the value the criterion means
/// — the bits themselves — is gone by the time an accessor could look, and a comparison
/// against 0 or 1 would answer differently from the interpreter on the packets that differ.
#[test]
fn a_context_criterion_on_a_wide_boolean_is_refused() {
    let error = refusal(layout_of(
        r#"    <ParameterTypeSet>
      <BooleanParameterType name="FLAG_T" oneStringValue="TRUE" zeroStringValue="FALSE">
        <IntegerDataEncoding sizeInBits="8" encoding="unsigned"/>
      </BooleanParameterType>
      <IntegerParameterType name="SENSOR_T">
        <IntegerDataEncoding sizeInBits="8" encoding="unsigned">
          <ContextCalibratorList>
            <ContextCalibrator>
              <ContextMatch>
                <Comparison parameterRef="FLAG" value="3" useCalibratedValue="false"/>
              </ContextMatch>
              <Calibrator>
                <PolynomialCalibrator><Term coefficient="0.5" exponent="1"/></PolynomialCalibrator>
              </Calibrator>
            </ContextCalibrator>
          </ContextCalibratorList>
          <DefaultCalibrator>
            <PolynomialCalibrator><Term coefficient="1.0" exponent="1"/></PolynomialCalibrator>
          </DefaultCalibrator>
        </IntegerDataEncoding>
      </IntegerParameterType>
    </ParameterTypeSet>
    <ParameterSet>
      <Parameter name="FLAG" parameterTypeRef="FLAG_T"/>
      <Parameter name="SENSOR" parameterTypeRef="SENSOR_T"/>
    </ParameterSet>
    <ContainerSet>
      <SequenceContainer name="Only">
        <EntryList>
          <ParameterRefEntry parameterRef="FLAG"/>
          <ParameterRefEntry parameterRef="SENSOR"/>
        </EntryList>
      </SequenceContainer>
    </ContainerSet>"#,
    ));

    assert!(
        error.contains("boolean wider than one bit"),
        "unexpected refusal: {error}"
    );
}

/// A `<FixedValueEntry>` becomes bits the encoder writes, reduced to the width it declares.
#[test]
fn a_fixed_value_becomes_bits_the_encoder_writes() {
    let plain = layout_of(&one_field(
        r#"<IntegerParameterType name="FIELD_T">
        <IntegerDataEncoding sizeInBits="8" encoding="unsigned"/>
      </IntegerParameterType>"#,
        0,
    ))
    .expect("compiles");
    assert!(
        plain.containers[0].fixed.is_empty(),
        "a telemetry container has none"
    );

    let layout = command_layout(
        r#"            <FixedValueEntry name="SYNC" binaryValue="DEADBEEF" sizeInBits="16"/>"#,
    )
    .expect("the command compiles");

    let fixed = &layout.containers[0].fixed;
    assert_eq!(fixed.len(), 1);
    assert_eq!(fixed[0].xtce_name.as_deref(), Some("SYNC"));
    assert_eq!(fixed[0].bit_offset, 0);
    assert_eq!(fixed[0].bit_width, 16);
    // Four bytes given for sixteen bits: the low half is what goes on the wire.
    assert_eq!(fixed[0].raw, 0xBEEF);
    // The argument sits after it, and is the caller's to set.
    assert_eq!(layout.containers[0].fields.len(), 1);
    assert_eq!(layout.containers[0].fields[0].bit_offset, 16);
}

/// A fixed value wider than a `u64` is refused rather than written in pieces.
#[test]
fn a_fixed_value_wider_than_64_bits_is_refused() {
    let error = refusal(command_layout(
        r#"            <FixedValueEntry name="FILL" binaryValue="00112233445566778899AABBCCDDEEFF" sizeInBits="128"/>"#,
    ));
    assert!(
        error.contains("FixedValueEntry") && error.contains("64 bits"),
        "unexpected refusal: {error}"
    );
}
