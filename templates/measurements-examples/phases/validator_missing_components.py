import sys
import os
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_missing_components(phase, test_api, ui):
    """Validators with missing operators, expected values, or expressions"""

    # Missing operator - should result in UNSET outcome
    voltage1 = 3.3
    test_api.measurements.missing_operator = voltage1
    test_api.log.info(f"Missing operator test: {voltage1}V")

    # Missing expected value - should result in UNSET outcome
    voltage2 = 2.8
    test_api.measurements.missing_expected_value = voltage2
    test_api.log.info(f"Missing expected value test: {voltage2}V")

    # Expression only (no operator/expected_value) - should evaluate expression
    voltage3 = 1.5
    test_api.measurements.expression_only = voltage3
    test_api.log.info(f"Expression only test: {voltage3}V")

    