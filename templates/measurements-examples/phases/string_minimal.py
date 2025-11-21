import sys
import os
import random
import json
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_minimal_string(phase, test_api, ui):
    """Simple firmware version string + edge cases"""

    # Simple firmware version string
    firmware_version = f"v{random.randint(1, 3)}.{random.randint(0, 9)}.{random.randint(0, 99)}"
    test_api.measurements.firmware_version = firmware_version
    test_api.log.info(f"Firmware version: {firmware_version}")

    # Empty string edge case
    empty_string = ""
    test_api.measurements.empty_string = empty_string
    test_api.log.info("Empty string measurement added")

    # Unicode characters edge case
    unicode_characters = "Test αβγ δεζ ηθι Unicode: 温度 ñáéíóú 🚀"
    test_api.measurements.unicode_characters = unicode_characters
    test_api.log.info(f"Unicode text: {unicode_characters}")

    # Very long string edge case
    very_long_string = "A" * 10000  # 10KB string
    test_api.measurements.very_long_string = very_long_string
    test_api.log.info(f"Very long string length: {len(very_long_string)} characters")

    # JSON-formatted string edge case
    json_data = {
        "device_id": "DEV123",
        "status": "operational",
        "sensors": ["temp", "humidity", "pressure"],
        "config": {
            "sampling_rate": 1000,
            "enabled": True
        }
    }
    json_formatted_string = json.dumps(json_data, indent=2)
    test_api.measurements.json_formatted_string = json_formatted_string
    test_api.log.info("JSON-formatted string measurement added")

    