"""Phase control flow examples showing different return types."""

import sys
import os
import time
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', '..', 'src-tauri', 'python'))


def return_repeat(phase, test_api, ui):
    """Returns RETRY up to retry_limit times (additional attempts after initial)."""
    time.sleep(0.25)
    phase.retry("Retrying phase")
