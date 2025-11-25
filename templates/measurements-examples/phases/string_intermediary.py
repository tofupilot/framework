import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_intermediary_string(phase, run, ui):
    """Serial number with unit and docstring"""

    # Generate a realistic serial number
    serial_prefixes = ["ABC", "DEF", "XYZ", "TUV"]
    serial_number = f"{random.choice(serial_prefixes)}{random.randint(100000, 999999)}"

    # ✅ Correct: Only provide value, YAML defines docstring and unit
    measurements.serial_number = serial_number
    run.log.info(f"Serial number: {serial_number}")

    