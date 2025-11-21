import sys
import os
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_existing_fail(phase, test_api, ui):
    """YAML validator with outcome=FAIL"""

    # Provide any value - YAML validator has outcome=FAIL, so it should always fail regardless
    voltage = 3.3  # This doesn't match expected 5.0V, but outcome is pre-set to FAIL anyway
    test_api.measurements.existing_fail_test = voltage
    test_api.log.info(f"Existing fail test voltage: {voltage}V (will always fail due to YAML outcome=FAIL)")

    