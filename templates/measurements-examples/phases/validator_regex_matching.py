import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_regex_matching(phase, run, ui):
    """Pattern matching, array unwrapping, invalid patterns, type mismatches"""

    # Pattern matching - should match version pattern ^v[0-9]+\.[0-9]+\.[0-9]+$
    test_versions = ["v1.0.0", "v2.3.45", "1.0.0", "v1.0", "invalid"]
    version = random.choice(test_versions)
    measurements.pattern_matching = version
    run.log.info(f"Pattern matching test: {version}")

    # Array unwrapping regex - expected value is array with single regex pattern
    serial_numbers = ["AB123456", "XY789012", "123456", "AB12345X"]
    serial = random.choice(serial_numbers)
    measurements.array_unwrap_regex = serial
    run.log.info(f"Array unwrap regex test: {serial}")

    # Invalid pattern - malformed regex should result in UNSET
    test_string = "test_string"
    measurements.invalid_pattern = test_string
    run.log.info(f"Invalid pattern test: {test_string}")

    # Type mismatch - regex validator with numeric value
    numeric_value = random.choice([123, 456.78])
    measurements.type_mismatch_regex = numeric_value
    run.log.info(f"Type mismatch regex test: {numeric_value}")

    