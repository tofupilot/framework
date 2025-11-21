import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_comparison_operations(phase, test_api, ui):
    """Numeric comparison, string comparison, array unwrapping, type mismatches"""

    # Numeric comparison with multiple operators
    voltage = random.uniform(1.5, 4.5)  # Random voltage to test various operators
    test_api.measurements.numeric_comparison = voltage
    test_api.log.info(f"Numeric comparison test: {voltage}V")

    # String comparison (lexicographic)
    versions = ["v0.9.0", "v1.0.0", "v1.1.0", "v2.0.0"]
    version = random.choice(versions)
    test_api.measurements.string_comparison = version
    test_api.log.info(f"String comparison test: {version}")

    # Array unwrapping for comparison - expected [2.0] unwraps to 2.0
    test_voltage = random.uniform(1.0, 3.0)
    test_api.measurements.array_unwrap_comparison = test_voltage
    test_api.log.info(f"Array unwrap comparison test: {test_voltage}V")

    