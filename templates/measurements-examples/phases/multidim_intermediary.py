import sys
import os
import math
def test_intermediary_multidim(phase, run, ui):
    """Frequency response plot with axis metadata and docstrings"""

    # Generate frequency response data (logarithmic frequency sweep)
    frequencies = [10 ** (i / 10) for i in range(10, 60)]  # 1 Hz to 1 MHz

    # Magnitude response (low-pass filter characteristic)
    magnitudes = []
    phases = []
    for f in frequencies:
        # Simple RC low-pass filter response
        fc = 1000  # Corner frequency at 1 kHz
        omega = 2 * math.pi * f
        omega_c = 2 * math.pi * fc

        # Magnitude in dB
        magnitude_db = -20 * math.log10(math.sqrt(1 + (omega / omega_c) ** 2))
        magnitudes.append(magnitude_db)

        # Phase in degrees
        phase_deg = -math.degrees(math.atan(omega / omega_c))
        phases.append(phase_deg)

    # Create axes with enhanced metadata
    x_axis = Axis(data=frequencies)

    magnitude_axis = Axis(data=magnitudes)

    phase_axis = Axis(data=phases)

    # Create multidimensional measurement
    frequency_response = MultiDim(
        x_axis=x_axis,
        y_axis=[magnitude_axis, phase_axis],
    )

    measurements.frequency_response = frequency_response
    run.log.info(
        f"Generated frequency response with {len(frequencies)} frequency points"
    )
    run.log.info(
        f"Frequency range: {frequencies[0]:.1f} Hz to {frequencies[-1]:.0f} Hz"
    )

    
