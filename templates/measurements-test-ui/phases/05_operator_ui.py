import sys
import os
import time

sys.path.append(
    os.path.join(os.path.dirname(__file__), "..", "..", "src-tauri", "python")
)


def python_bound_ui(phase, test_api, ui):
    test_api.measurements.humidity = 42

    
