"""Phase control flow examples showing different return types."""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def measurement_validation_fail(phase, run, ui):
    """Returns CONTINUE but measurement fails critical validator → outcome is FAIL."""
    time.sleep(0.25)
    measurements.voltage = 2.5
    
