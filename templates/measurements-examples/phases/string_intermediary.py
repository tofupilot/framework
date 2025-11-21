import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_intermediary_string(phase, test_api, ui):
    """Serial number with unit and docstring"""

    # Generate a realistic serial number
    serial_prefixes = ["ABC", "DEF", "XYZ", "TUV"]
    serial_number = f"{random.choice(serial_prefixes)}{random.randint(100000, 999999)}"

    # ✅ Correct: Only provide value, YAML defines docstring and unit
    test_api.measurements.serial_number = serial_number
    test_api.log.info(f"Serial number: {serial_number}")

    