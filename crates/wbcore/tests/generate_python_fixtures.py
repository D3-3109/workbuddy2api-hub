"""Regenerate bounded, synthetic CPython oracle data (stdlib only, no network).

Run this file with CPython 3.10+; it writes only tests/fixtures/python_compat.json.
Expected results come from Python builtins/json, never from Rust or project code.
Cargo tests use the checked-in data and do not need a Python installation.
This is representative coverage, not a claim of equality for every Python value.
"""

import json
import math
from pathlib import Path
import platform
import random
import struct
import sys


def bits(value):
    return struct.pack(">d", value).hex()


def int_result(value):
    try:
        result = int(value)
        return str(result) if -(2**63) <= result < 2**63 else None
    except (TypeError, ValueError, OverflowError):
        return None


def float_result(value):
    try:
        result = float(value)
        return "nan" if math.isnan(result) else bits(result)
    except (TypeError, ValueError, OverflowError):
        return None


def main():
    assert platform.python_implementation() == "CPython", "CPython is the oracle"
    assert sys.version_info >= (3, 10)
    strings = [
        "", "true false null", "untrue falsehood nullable", "'", '"', "both'\"",
        "back\\slash /", "\n\r\t\b\f\0", "".join(map(chr, range(0x20))),
        "~\x7f\x80\x85\xa0", "é你好😀", "\u2028\u2029\u200b\ufeff",
        "a\u0301", "\ue000\U000f0000\U0010ffff", "\u0378",
    ]
    numbers = [
        0, 1, -1, -(2**63), 2**63 - 1, 2**63, 2**64 - 1,
        0.0, -0.0, 1.0, -1.0, 2.675, 1.005, 0.1, -2.5,
        0.0001, math.nextafter(0.0001, 0.0), 1e-5,
        1e15, math.nextafter(1e16, 0.0), 1e16, 1e20, 1e21,
        2**53 - 1.0, float(2**53), float(2**63),
        math.nextafter(float(2**63), 0.0),
        -float(2**63), math.nextafter(-float(2**63), -math.inf),
        sys.float_info.min, sys.float_info.max, 5e-324, -5e-324,
    ]
    values = [
        None, False, True, [], {}, [0], {"x": False},
        *numbers, *strings,
        strings,
        {"true false null": [True, False, None, *strings]},
        {"z": {}, "a": [], "é": [1.0, -0.0], "😀": {"q'": "'\""}},
        {"\U00010000": 1, "\ue000": 2, "a": 3},
        [[[["nested", None, False]]]],
    ]
    styles = {
        "default": {},
        "unicode": {"ensure_ascii": False},
        "compact": {"ensure_ascii": False, "separators": (",", ":")},
        "sorted": {"ensure_ascii": False, "sort_keys": True},
        "pretty": {"ensure_ascii": False, "indent": 2},
        "ascii_pretty_sorted": {"ensure_ascii": True, "indent": 2, "sort_keys": True},
        "indent_zero": {"ensure_ascii": False, "indent": 0},
    }
    json_cases = []
    for value in values:
        json_cases.append({
            "input": json.dumps(value, ensure_ascii=False),
            "dumps": {name: json.dumps(value, **opts) for name, opts in styles.items()},
            "str": str(value),
            "str_or_empty": str(value or ""),
            "truthy": bool(value),
        })

    conversion_values = [
        *numbers, None, True, False, [], {}, "", " ", "0", "-0", " +0042 ",
        " -42\t", "-9223372036854775808", "9223372036854775807",
        "9223372036854775808", "-9223372036854775809", "18446744073709551615",
        "1.5", "-1.5", "1e3", "-0.0", "NaN", "Infinity", "-inf", "1e400",
        "1e-400", "no number",
    ]
    conversions = [{
        "input": json.dumps(value), "int": int_result(value), "float": float_result(value),
    } for value in conversion_values]

    round_inputs = [
        0.0, -0.0, 0.5, -0.5, 1.5, 2.5, -1.5, -2.5,
        2.675, -2.675, 1.005, -1.005, 0.045, 12.34,
        149.9, 150.0, 150.1, 250.0, -149.9, -150.0, -250.0,
        0.15, 0.25, 1e20, 1e23, 1e-308, -1e-308,
        5e-324, -5e-324, sys.float_info.min, sys.float_info.max, -sys.float_info.max,
        math.inf, -math.inf, math.nan,
    ]
    for boundary in [0.5, 1.5, 2.675, 150.0, 250.0, 1e16]:
        round_inputs.extend([math.nextafter(boundary, -math.inf), math.nextafter(boundary, math.inf)])
    digits = [-(2**31), -309, -308, -307, -23, -16, -2, -1, 0, 1, 2, 15, 16, 22, 307, 308, 322, 323, 324, 2**31 - 1]
    pairs = [(value, nd) for value in round_inputs for nd in digits]
    # Fixed seed, finite workload, and bit-pattern inputs rather than a decimal
    # conversion shared with the implementation under test.
    rng = random.Random(0x5742434F5245)
    for _ in range(256):
        value = struct.unpack(">d", rng.getrandbits(64).to_bytes(8, "big"))[0]
        for nd in [-308, -2, 0, 2, 16, 308, 323, rng.randrange(-310, 326)]:
            pairs.append((value, nd))
    rounds = []
    for value, nd in pairs:
        case = {"input_bits": bits(value), "nd": nd}
        try:
            result = round(value, nd)
            case["expected"] = "nan" if math.isnan(result) else bits(result)
        except OverflowError:
            # The Rust f64 API explicitly maps Python's exception to infinity.
            case["expected"] = "overflow"
        rounds.append(case)

    data = {
        "oracle": f"CPython {platform.python_version()}; synthetic stdlib-only inputs",
        "json_cases": json_cases, "conversions": conversions, "rounds": rounds,
    }
    serialized = json.dumps(data, ensure_ascii=True, indent=2) + "\n"
    assert len(serialized.encode("utf-8")) < 512 * 1024
    target = Path(__file__).resolve().parent / "fixtures" / "python_compat.json"
    target.parent.mkdir(exist_ok=True)
    target.write_text(serialized, encoding="utf-8", newline="\n")
    print(f"Wrote {len(json_cases)} JSON, {len(conversions)} conversion, {len(rounds)} rounding cases to {target}")


if __name__ == "__main__":
    main()
