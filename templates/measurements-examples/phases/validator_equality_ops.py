import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_equality_operations(phase, run, ui):
    """Basic equality, array unwrapping, type mismatches"""

    # Basic equality operators
    voltage = random.choice([3.3, 2.8])  # Sometimes matches == 3.3, sometimes doesn't
    measurements.basic_equality = voltage
    run.log.info(f"Basic equality test: {voltage}V")

    # Array unwrapping - expected_value is ["PASS"] array but we provide scalar
    status = random.choice(["PASS", "FAIL"])
    measurements.array_unwrapping = status
    run.log.info(f"Array unwrapping test: {status}")

    # Type mismatch - YAML expects string "3.3" but we provide different types
    mixed_value = random.choice([3.3, "2.8", True])  # Number, string, or boolean
    measurements.type_mismatch = mixed_value
    run.log.info(f"Type mismatch test: {mixed_value}")

    