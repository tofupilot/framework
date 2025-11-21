import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_equality_operations(phase, test_api, ui):
    """Basic equality, array unwrapping, type mismatches"""

    # Basic equality operators
    voltage = random.choice([3.3, 2.8])  # Sometimes matches == 3.3, sometimes doesn't
    test_api.measurements.basic_equality = voltage
    test_api.log.info(f"Basic equality test: {voltage}V")

    # Array unwrapping - expected_value is ["PASS"] array but we provide scalar
    status = random.choice(["PASS", "FAIL"])
    test_api.measurements.array_unwrapping = status
    test_api.log.info(f"Array unwrapping test: {status}")

    # Type mismatch - YAML expects string "3.3" but we provide different types
    mixed_value = random.choice([3.3, "2.8", True])  # Number, string, or boolean
    test_api.measurements.type_mismatch = mixed_value
    test_api.log.info(f"Type mismatch test: {mixed_value}")

    