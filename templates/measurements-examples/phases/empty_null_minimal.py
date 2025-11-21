import sys
import os
sys.path.append(os.path.join(os.path.dirname(__file__), '..', '..', 'src-tauri', 'python'))

def test_minimal_empty_null(phase, test_api, ui):
    """Basic null value measurement"""

    # Basic null value measurement - no optional fields
    basic_null = None
    test_api.measurements.basic_null = basic_null
    test_api.log.info("Basic null measurement added")

    