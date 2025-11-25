import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_minimal_numeric(phase, run, ui):
    """Basic voltage measurement (no optional fields) + scientific notation + numeric types"""

    # Basic voltage measurement - no optional fields
    basic_voltage = 3.3 + random.uniform(-0.1, 0.1)
    measurements.basic_voltage = basic_voltage
    run.log.info(f"Basic voltage: {basic_voltage:.3f}V")

    # Scientific notation - very large
    scientific_large = 2.4e9
    measurements.scientific_large = scientific_large
    run.log.info(f"Large frequency: {scientific_large:.2e}Hz")

    # Scientific notation - very small
    scientific_small = 1.23e-15
    measurements.scientific_small = scientific_small
    run.log.info(f"Small current: {scientific_small:.2e}A")

    # Pure integer
    pure_integer = 42
    measurements.pure_integer = pure_integer
    run.log.info(f"Pure integer: {pure_integer}")

    # Integer as float
    integer_as_float = 100.0
    measurements.integer_as_float = integer_as_float
    run.log.info(f"Integer as float: {integer_as_float}")

    # High precision float
    high_precision_float = 3.141592653589793
    measurements.high_precision_float = high_precision_float
    run.log.info(f"High precision float: {high_precision_float:.15f}V")

    