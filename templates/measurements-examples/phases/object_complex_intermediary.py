import sys
import os
import random
import math
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_intermediary_object_complex(phase, run, ui):
    """Calibration parameters object and sine waveform arrays with metadata"""

    # Calibration parameters object with nested structure
    calibration_params = {
        "version": "2.1.0",
        "timestamp": "2024-01-15T14:30:00Z",
        "temperature_compensation": {
            "enabled": True,
            "coefficient": -0.002,
            "reference_temp": 25.0
        },
        "offset_corrections": [0.05, -0.02, 0.01],
        "gain_matrix": [
            [1.00, 0.01, 0.00],
            [0.00, 1.02, 0.01],
            [0.00, 0.00, 0.99]
        ],
        "checksum": "abc123def456"
    }

    # ✅ Correct: Only provide value, YAML defines docstring and unit
    measurements.calibration_params = calibration_params
    run.log.info("Calibration parameters loaded with temperature compensation")

    # Sine waveform arrays with metadata
    sample_rate = 1000  # Hz
    duration = 0.1      # 100ms
    frequency = 50      # 50 Hz sine wave

    time_array = [i / sample_rate for i in range(int(sample_rate * duration))]
    amplitude_array = [math.sin(2 * math.pi * frequency * t) for t in time_array]

    sine_waveform = {
        "time": time_array,
        "amplitude": amplitude_array,
        "sample_rate": sample_rate,
        "frequency": frequency,
        "duration": duration,
        "points": len(time_array)
    }

    # ✅ Correct: Only provide value, YAML defines docstring
    measurements.sine_waveform = sine_waveform
    run.log.info(f"Generated {frequency}Hz sine waveform with {len(time_array)} samples")

    