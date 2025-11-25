import sys
import os
import random
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_minimal_object_complex(phase, run, ui):
    """Simple configuration dictionary + basic sample arrays"""

    # Simple configuration dictionary
    simple_config = {
        "device_id": "DEV001",
        "mode": "auto",
        "enabled": True,
        "threshold": 3.3
    }
    measurements.simple_config = simple_config
    run.log.info(f"Simple config with {len(simple_config)} parameters")

    # Basic sample arrays
    basic_samples = [random.uniform(0, 10) for _ in range(10)]
    measurements.basic_samples = basic_samples
    run.log.info(f"Basic samples array with {len(basic_samples)} values")

    