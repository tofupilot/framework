import sys
import os
import random
import math
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_edge_cases(phase, run, ui):
    """Empty values, complex values, edge float values, unknown operators, boundary conditions"""

    # Empty values - null, empty string, empty objects/arrays
    empty_values = [None, "", {}, []]
    empty_value = random.choice(empty_values)
    measurements.empty_values = empty_value
    run.log.info(f"Empty values test: {empty_value}")

    # Complex values - nested objects and multidimensional arrays
    complex_values = [
        {"nested": {"deep": {"value": 42}}},
        [[1, 2], [3, 4], [5, 6]],
        {"array": [1, 2, 3], "object": {"key": "value"}}
    ]
    complex_value = random.choice(complex_values)
    measurements.complex_values = complex_value
    run.log.info(f"Complex values test: {type(complex_value).__name__}")

    # Edge float values - infinity, NaN (handle carefully to avoid issues)
    try:
        # Use string representations to avoid actual inf/nan propagation issues
        edge_floats = [1.7976931348623157e+308, -1.7976931348623157e+308, 0.0, -0.0]
        edge_float = random.choice(edge_floats)
        measurements.edge_float_values = edge_float
        run.log.info(f"Edge float test: {edge_float}")
    except:
        # Fallback to safe value if there are issues
        measurements.edge_float_values = 999999.999
        run.log.info("Edge float test: large finite value (fallback)")

    # Unknown operators - should result in UNSET outcome
    test_value = 1.0
    measurements.unknown_operators = test_value
    run.log.info(f"Unknown operators test: {test_value}")

    # Boundary conditions - exactly equal to boundary values
    boundary_value = 3.0  # Exactly matches both >= 3.0 and <= 3.0
    measurements.boundary_conditions = boundary_value
    run.log.info(f"Boundary conditions test: {boundary_value}V")

    