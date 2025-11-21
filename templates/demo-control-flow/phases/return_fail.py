"""Phase control flow examples showing different return types."""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def return_fail(phase, test_api, ui):
    """Returns FAIL, outcome is FAIL (when.fail determines next action)."""
    time.sleep(0.25)
    phase.fail("Phase failed")
