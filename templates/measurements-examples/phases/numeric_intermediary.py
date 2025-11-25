import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_intermediary_numeric(phase, run, ui):
    """Voltage with unit and docstring metadata"""

    # Voltage measurement with metadata - unit and docstring come from YAML,
    # but we can override in Python
    voltage = 3.3 + random.uniform(-0.05, 0.05)
    # ✅ Correct: Only provide value, YAML defines docstring and unit
    measurements.voltage_with_metadata = voltage
    run.log.info(f"Voltage with metadata: {voltage:.4f}V")

    