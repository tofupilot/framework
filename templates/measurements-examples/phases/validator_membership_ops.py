import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_membership_operations(phase, test_api, ui):
    """Array membership, homogeneous arrays, type mismatches, invalid expected, empty arrays"""

    # Array membership - value should be in ["PASS", "FAIL", "SKIP"]
    status_values = ["PASS", "FAIL", "SKIP", "ERROR", "TIMEOUT"]
    status = random.choice(status_values)
    test_api.measurements.array_membership = status
    test_api.log.info(f"Array membership test: {status}")

    # Homogeneous numeric array - test with numeric values
    voltage_values = [2.8, 3.0, 3.1, 3.2, 3.3, 4.0]
    voltage = random.choice(voltage_values)
    test_api.measurements.homogeneous_numeric = voltage
    test_api.log.info(f"Homogeneous numeric test: {voltage}V")

    # Type mismatch - YAML expects numeric array [1, 2, 3] but we provide string
    mixed_test_values = ["one", 2, 3.0]
    mixed_value = random.choice(mixed_test_values)
    test_api.measurements.type_mismatch_membership = mixed_value
    test_api.log.info(f"Type mismatch membership test: {mixed_value}")

    # Empty array membership - any value with empty array should fail
    test_value = "anything"
    test_api.measurements.empty_array_membership = test_value
    test_api.log.info(f"Empty array membership test: {test_value}")

    