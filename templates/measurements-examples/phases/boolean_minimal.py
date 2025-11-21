import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_minimal_boolean(phase, test_api, ui):
    """Basic power status flag"""

    # Basic power status - no optional fields
    power_status = random.choice([True, False])
    test_api.measurements.power_status = power_status
    test_api.log.info(f"Power status: {power_status}")

    