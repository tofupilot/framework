import sys
import os
import math
def test_minimal_multidim(phase, run, ui):
    """Simple linear XY curve (time vs voltage)"""

    # Generate simple time vs voltage curve
    time_points = [i * 0.1 for i in range(100)]  # 0 to 9.9 seconds
    voltage_points = [3.3 + 0.2 * math.sin(2 * math.pi * 0.1 * t) for t in time_points]

    # Create axes
    x_axis = Axis(data=time_points)
    y_axis = [Axis(data=voltage_points)]

    # Create multidimensional measurement
    time_voltage_curve = MultiDim(
        x_axis=x_axis,
        y_axis=y_axis,
    )

    measurements.time_voltage_curve = time_voltage_curve
    run.log.info(f"Generated time-voltage curve with {len(time_points)} data points")

    
