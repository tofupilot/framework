import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_minimal_numeric(phase, test_api, ui):
    """Basic voltage measurement (no optional fields) + scientific notation + numeric types"""

    # Basic voltage measurement - no optional fields
    basic_voltage = 3.3 + random.uniform(-0.1, 0.1)
    test_api.measurements.basic_voltage = basic_voltage
    test_api.log.info(f"Basic voltage: {basic_voltage:.3f}V")

    # Scientific notation - very large
    scientific_large = 2.4e9
    test_api.measurements.scientific_large = scientific_large
    test_api.log.info(f"Large frequency: {scientific_large:.2e}Hz")

    # Scientific notation - very small
    scientific_small = 1.23e-15
    test_api.measurements.scientific_small = scientific_small
    test_api.log.info(f"Small current: {scientific_small:.2e}A")

    # Pure integer
    pure_integer = 42
    test_api.measurements.pure_integer = pure_integer
    test_api.log.info(f"Pure integer: {pure_integer}")

    # Integer as float
    integer_as_float = 100.0
    test_api.measurements.integer_as_float = integer_as_float
    test_api.log.info(f"Integer as float: {integer_as_float}")

    # High precision float
    high_precision_float = 3.141592653589793
    test_api.measurements.high_precision_float = high_precision_float
    test_api.log.info(f"High precision float: {high_precision_float:.15f}V")

    