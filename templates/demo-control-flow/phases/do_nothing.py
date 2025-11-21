"""Phase control flow examples showing different return types."""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def do_nothing(test_api, ui):
    """Returns None = CONTINUE, outcome is PASS/FAIL based on measurements."""
    time.sleep(0.25)
    pass
