import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_intermediary_empty_null(phase, test_api, ui):
    """Optional calibration data with unit and explanation"""

    # Simulate optional calibration data that may or may not be available
    calibration_available = random.choice([True, False])

    if calibration_available:
        # Calibration data is available
        optional_calibration = {
            "offset": 0.05,
            "gain": 1.02,
            "timestamp": "2024-01-15T10:30:00Z"
        }
    else:
        # Calibration data is not available
        optional_calibration = None

    # ✅ Correct: Only provide value, YAML defines docstring and unit
    test_api.measurements.optional_calibration = optional_calibration

    if optional_calibration is None:
        test_api.log.info("Optional calibration: Not available (null)")
    else:
        test_api.log.info(f"Optional calibration: Available with {len(optional_calibration)} parameters")

    