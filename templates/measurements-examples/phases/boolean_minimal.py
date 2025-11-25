import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_minimal_boolean(phase, run, ui):
    """Basic power status flag"""

    # Basic power status - no optional fields
    power_status = random.choice([True, False])
    measurements.power_status = power_status
    run.log.info(f"Power status: {power_status}")

    