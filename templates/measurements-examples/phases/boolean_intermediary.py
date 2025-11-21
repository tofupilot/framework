import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_intermediary_boolean(phase, test_api, ui):
    """Safety interlock with unit and docstring"""

    # Safety interlock status with enhanced metadata
    safety_interlock = random.choice([True, False])

    # ✅ Correct: Only provide value, YAML defines docstring and unit
    test_api.measurements.safety_interlock = safety_interlock
    test_api.log.info(f"Safety interlock engaged: {safety_interlock}")

    