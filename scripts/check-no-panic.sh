#!/usr/bin/env bash
#
# Fails if the generated flight code can panic.
#
# Not "does not call unwrap" — that is easy and nearly worthless. This builds the bare-metal
# probe for a Cortex-M4F, emits the LLVM IR, and looks for any reference to `core::panicking`.
# A single slice index with a runtime bound would put one there, and on a part with no
# unwinder and no console a panic is a silent reset in flight.
#
# Usage: scripts/check-no-panic.sh [target]

set -euo pipefail

target="${1:-thumbv7em-none-eabihf}"
probe="$(cd "$(dirname "${BASH_SOURCE[0]}")/../probes/nostd" && pwd)"

deps="${probe}/target/${target}/release/deps"

# An IR file left over from an earlier build would satisfy every check below while describing
# code that is no longer what was compiled. Clearing them is not enough on its own: cargo
# would then see an up-to-date crate and skip the rustc invocation that writes them. So the
# probe's own artefacts go too, which forces a real compile. Its dependencies are left alone,
# because they are all build-dependencies and rebuilding them proves nothing.
rm -f "${deps}"/*.ll
cargo clean \
    --manifest-path "${probe}/Cargo.toml" \
    --release \
    --target "${target}" \
    -p xtce-flight-nostd-probe

echo "building the probe for ${target}"
cargo rustc \
    --manifest-path "${probe}/Cargo.toml" \
    --release \
    --target "${target}" \
    -- --emit=llvm-ir -C debuginfo=0

count="$(find "${deps}" -name '*.ll' | wc -l | tr -d ' ')"
if [[ "${count}" -ne 1 ]]; then
    echo "expected exactly one IR file in ${deps}, found ${count}" >&2
    exit 1
fi
ir="$(find "${deps}" -name '*.ll' -print -quit)"

# The probe's own entry points have to be in there, or the optimiser removed the very code
# this is meant to inspect and a clean result would mean nothing.
for symbol in encode_numeric_edges encode_status_report encode_beacon decode_jpss calibrate_all calibrate_in_context round_trip_little_endian round_trip_arrays round_trip_aggregates; do
    if ! grep -q "${symbol}" "${ir}"; then
        echo "${symbol} is not in the emitted IR; the check would pass vacuously" >&2
        exit 1
    fi
done

hits="$(grep -c 'panicking\|panic_bounds_check\|panic_fmt' "${ir}" || true)"
lines="$(wc -l < "${ir}" | tr -d ' ')"

if [[ "${hits}" -ne 0 ]]; then
    echo "found ${hits} reference(s) to panicking machinery in ${lines} lines of IR:" >&2
    grep -n 'panicking\|panic_bounds_check\|panic_fmt' "${ir}" | head -20 >&2
    exit 1
fi

echo "no panic path in ${lines} lines of IR for ${target}"
