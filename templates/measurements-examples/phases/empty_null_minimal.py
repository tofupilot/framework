import sys
import os
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_minimal_empty_null(phase, run, ui):
    """Basic null value measurement"""

    # Basic null value measurement - no optional fields
    basic_null = None
    measurements.basic_null = basic_null
    run.log.info("Basic null measurement added")

    